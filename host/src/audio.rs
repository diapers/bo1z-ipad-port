//! Native audio output reading the engine's mixer ring directly (audio-worklet.js
//! "mixer" mode): no wasm calls on the audio thread.
//!
//! KB_AudioRing() -> u32[7] = [capacity, samples, read, write, times, mixTimes, demand].
//! read/write are monotonic frame counters; samples are interleaved f32 stereo at
//! (index % capacity) * 2. After each callback, if fewer than 2048 frames remain,
//! the mixer is woken through Atomics.add/notify on `demand`.

use std::sync::atomic::{AtomicU32, Ordering};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;

use crate::{log, mem, rt};

#[derive(Clone, Copy, Default)]
struct Ring {
    capacity: u32,
    samples: u32,
    read: u32,
    write: u32,
    demand: u32,
}

static RING: Mutex<Option<Ring>> = Mutex::new(None);
static STREAM: Mutex<Option<SendStream>> = Mutex::new(None);
static UNDERRUNS: AtomicU32 = AtomicU32::new(0);

struct SendStream(#[allow(dead_code)] cpal::Stream);
// The stream is only kept alive here; it is never touched from another thread.
unsafe impl Send for SendStream {}

pub fn configure(layout: u32) {
    if layout == 0 {
        log::error("KB_AudioRing returned 0; audio disabled");
        return;
    }
    let r = Ring {
        capacity: mem::u32(layout),
        samples: mem::u32(layout + 4),
        read: mem::u32(layout + 8),
        write: mem::u32(layout + 12),
        demand: mem::u32(layout + 24),
    };
    log::info(&format!("audio ring: capacity={} frames", r.capacity));
    *RING.lock() = Some(r);
}

fn wake_mixer(r: &Ring) {
    if r.demand != 0 {
        mem::atomic_i32(r.demand).fetch_add(1, Ordering::SeqCst);
        let _ = rt::rt().memory.atomic_notify(r.demand as u64, 1);
    }
}

/// Pull `frames` stereo frames at the engine's 48 kHz into `out` (interleaved).
fn pull(r: &Ring, out: &mut [f32]) {
    let read_a = mem::atomic_u32(r.read);
    let write = mem::atomic_u32(r.write).load(Ordering::Acquire);
    let mut read = read_a.load(Ordering::Acquire);
    let fill = write.wrapping_sub(read);
    let mut count = 0u32;
    for frame in out.chunks_exact_mut(2) {
        if read == write {
            frame[0] = 0.0;
            frame[1] = 0.0;
        } else {
            let idx = (read % r.capacity) * 8;
            frame[0] = mem::f32(r.samples + idx);
            frame[1] = mem::f32(r.samples + idx + 4);
            read = read.wrapping_add(1);
            count += 1;
        }
    }
    read_a.store(read, Ordering::Release);
    if count < (out.len() / 2) as u32 {
        UNDERRUNS.fetch_add(1, Ordering::Relaxed);
    }
    if fill.saturating_sub(count) < 2048 {
        wake_mixer(r);
    }
}

pub fn start() {
    let Some(ring) = *RING.lock() else { return };
    if STREAM.lock().is_some() {
        return;
    }
    let host = cpal::default_host();
    let Some(dev) = host.default_output_device() else {
        log::error("no audio output device");
        return;
    };
    let wanted = cpal::StreamConfig {
        channels: 2,
        sample_rate: cpal::SampleRate(48_000),
        buffer_size: cpal::BufferSize::Default,
    };
    let supports_48k = dev
        .supported_output_configs()
        .map(|mut it| {
            it.any(|c| {
                c.channels() == 2
                    && c.sample_format() == cpal::SampleFormat::F32
                    && c.min_sample_rate().0 <= 48_000
                    && c.max_sample_rate().0 >= 48_000
            })
        })
        .unwrap_or(false);
    let (config, channels) = if supports_48k {
        (wanted, 2usize)
    } else {
        match dev.default_output_config() {
            Ok(c) => {
                let ch = c.channels() as usize;
                (c.into(), ch)
            }
            Err(e) => {
                log::error(&format!("audio config: {e}"));
                return;
            }
        }
    };
    let rate = config.sample_rate.0 as f64;
    log::info(&format!("audio out: {} Hz, {channels} ch{}", rate, if supports_48k { "" } else { " (resampling)" }));

    // Linear resampler from 48 kHz when the device runs at another rate.
    let step = 48_000.0 / rate;
    let mut phase = 0.0f64;
    let mut prev = [0.0f32; 2];
    let mut cur = [0.0f32; 2];
    let mut scratch: Vec<f32> = Vec::new();
    let cb = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let frames = data.len() / channels;
        if supports_48k && channels == 2 {
            pull(&ring, data);
            return;
        }
        for i in 0..frames {
            phase += step;
            while phase >= 1.0 {
                phase -= 1.0;
                prev = cur;
                scratch.resize(2, 0.0);
                pull(&ring, &mut scratch[..2]);
                cur = [scratch[0], scratch[1]];
            }
            let t = phase as f32;
            let l = prev[0] + (cur[0] - prev[0]) * t;
            let r = prev[1] + (cur[1] - prev[1]) * t;
            let o = &mut data[i * channels..(i + 1) * channels];
            o[0] = l;
            if channels > 1 {
                o[1] = r;
            }
            for x in o.iter_mut().skip(2) {
                *x = 0.0;
            }
        }
    };
    match dev.build_output_stream(&config, cb, |e| log::error(&format!("audio stream: {e}")), None) {
        Ok(s) => {
            if let Err(e) = s.play() {
                log::error(&format!("audio play: {e}"));
            }
            *STREAM.lock() = Some(SendStream(s));
        }
        Err(e) => log::error(&format!("audio stream build: {e}")),
    }
}
