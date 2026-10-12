//! Desktop loading cinematic: the map's web/video/<zone>_load video (converted to
//! <game>/menu/load-<slug>.mp4 by convert-videos.mjs), played in a child window laid over
//! the game window with its own EGL surface and context, so it runs smoothly while the
//! engine thread owns the game window's context and barely presents.
//!
//! Decoding is ffmpeg on PATH (development only; iOS plays the same MP4 with AVPlayer):
//! one process for rgb24 frames, one for f32 audio at the output device's rate. The
//! loading bar and status line are drawn over the video; after the video ends its last
//! frame stays until the player starts.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::gl_ctx::Aux;
use crate::log;

const W: i32 = 854;
const H: i32 = 480;
const FPS: f64 = 29.97;

pub struct LoadVideo {
    aux: Aux,
    frames: Receiver<Vec<u8>>,
    video_proc: Child,
    audio_proc: Option<Child>,
    _audio: Option<SendStream>,
    tex: u32,
    fbo: u32,
    start: Instant,
    shown: u64,
    ended: bool,
}

struct SendStream(#[allow(dead_code)] cpal::Stream);
// Kept alive only; never touched from another thread.
unsafe impl Send for SendStream {}

impl LoadVideo {
    /// `window` is the child window's HWND.
    pub fn start(window: *mut std::ffi::c_void, video: &Path) -> anyhow::Result<Self> {
        let mut video_proc = Command::new("ffmpeg")
            .args(["-loglevel", "error", "-i"])
            .arg(video)
            .args(["-an", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", &format!("{W}x{H}"), "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut out = video_proc.stdout.take().ok_or_else(|| anyhow::anyhow!("no ffmpeg stdout"))?;
        let (tx, frames) = sync_channel::<Vec<u8>>(8);
        std::thread::Builder::new().name("loadvideo-frames".into()).spawn(move || loop {
            let mut buf = vec![0u8; (W * H * 3) as usize];
            if out.read_exact(&mut buf).is_err() || tx.send(buf).is_err() {
                break;
            }
        })?;
        let (audio_proc, audio) = match start_audio(video) {
            Ok((p, s)) => (Some(p), Some(SendStream(s))),
            Err(e) => {
                log::error(&format!("loading video audio: {e}"));
                (None, None)
            }
        };

        let aux = Aux::new(window)?;
        aux.make_current()?;
        let (mut tex, mut fbo) = (0, 0);
        unsafe {
            gl::GenTextures(1, &mut tex);
            gl::BindTexture(gl::TEXTURE_2D, tex);
            gl::TexImage2D(gl::TEXTURE_2D, 0, gl::RGB as i32, W, H, 0, gl::RGB, gl::UNSIGNED_BYTE, std::ptr::null());
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::GenFramebuffers(1, &mut fbo);
        }
        log::info(&format!("loading video: {}", video.display()));
        Ok(Self { aux, frames, video_proc, audio_proc, _audio: audio, tex, fbo, start: Instant::now(), shown: 0, ended: false })
    }

    /// Advance to the frame due now and redraw (UI thread, every tick).
    pub fn tick(&mut self) {
        if self.aux.make_current().is_err() {
            return;
        }
        let due = (self.start.elapsed().as_secs_f64() * FPS) as u64 + 1;
        let mut latest = None;
        while !self.ended && self.shown < due {
            match self.frames.try_recv() {
                Ok(f) => {
                    self.shown += 1;
                    latest = Some(f);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.ended = true,
            }
        }
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.tex);
            if let Some(f) = latest {
                gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
                gl::TexSubImage2D(gl::TEXTURE_2D, 0, 0, 0, W, H, gl::RGB, gl::UNSIGNED_BYTE, f.as_ptr() as *const _);
            }
        }
        let (w, h) = self.aux.size();
        let fbo = self.fbo;
        crate::loading::draw_over(w, h, || unsafe {
            gl::BindFramebuffer(gl::READ_FRAMEBUFFER, fbo);
            gl::FramebufferTexture2D(gl::READ_FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, self.tex, 0);
            gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, 0);
            let s = crate::menu::Menu::stage(w as f32, h as f32);
            let (x0, x1) = (s.x.round() as i32, (s.x + s.w).round() as i32);
            let (y_top, y_bot) = (h - s.y.round() as i32, h - (s.y + s.h).round() as i32);
            gl::BlitFramebuffer(0, 0, W, H, x0, y_top, x1, y_bot, gl::COLOR_BUFFER_BIT, gl::LINEAR);
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
        });
        self.aux.swap();
    }
}

impl Drop for LoadVideo {
    fn drop(&mut self) {
        let _ = self.video_proc.kill();
        if let Some(p) = self.audio_proc.as_mut() {
            let _ = p.kill();
        }
        if self.aux.make_current().is_ok() {
            unsafe {
                gl::DeleteTextures(1, &self.tex);
                gl::DeleteFramebuffers(1, &self.fbo);
            }
        }
    }
}

/// ffmpeg -> f32 samples at the default device's rate/channels -> a cpal stream.
fn start_audio(video: &Path) -> anyhow::Result<(Child, cpal::Stream)> {
    let dev = cpal::default_host().default_output_device().ok_or_else(|| anyhow::anyhow!("no output device"))?;
    let cfg = dev.default_output_config()?;
    if cfg.sample_format() != cpal::SampleFormat::F32 {
        anyhow::bail!("output device is not f32");
    }
    let (rate, ch) = (cfg.sample_rate().0, cfg.channels() as usize);
    let mut proc = Command::new("ffmpeg")
        .args(["-loglevel", "error", "-i"])
        .arg(video)
        .args(["-vn", "-f", "f32le", "-ar", &rate.to_string(), "-ac", &ch.to_string(), "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut out = proc.stdout.take().ok_or_else(|| anyhow::anyhow!("no ffmpeg stdout"))?;
    let queue: Arc<Mutex<std::collections::VecDeque<f32>>> = Arc::default();
    let q = queue.clone();
    std::thread::Builder::new().name("loadvideo-audio".into()).spawn(move || {
        let mut buf = [0u8; 16384];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            // Keep at most ~1 s queued so the decoder cannot run far ahead.
            while q.lock().unwrap().len() > rate as usize * ch {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let mut g = q.lock().unwrap();
            for c in buf[..n - n % 4].chunks_exact(4) {
                g.push_back(f32::from_le_bytes([c[0], c[1], c[2], c[3]]) * 0.6);
            }
        }
    })?;
    let stream = dev.build_output_stream(
        &cfg.into(),
        move |data: &mut [f32], _| {
            let mut g = queue.lock().unwrap();
            for s in data.iter_mut() {
                *s = g.pop_front().unwrap_or(0.0);
            }
        },
        |e| log::error(&format!("loading video audio stream: {e}")),
        None,
    )?;
    stream.play()?;
    Ok((proc, stream))
}
