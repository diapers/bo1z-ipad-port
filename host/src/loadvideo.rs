//! Desktop loading cinematic: the map's <menu>/load-<slug>.mp4 (convert-videos.mjs)
//! decoded by ffmpeg on PATH (development only; iOS plays the same file with AVPlayer) and
//! drawn with GDI into a borderless child window over the game window. GDI, not a second
//! GL context: ANGLE crashes when the UI thread and the engine thread render through two
//! contexts at once. Audio goes to its own cpal stream.

use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{sync_channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::log;

const W: i32 = 854;
const H: i32 = 480;
const FPS: f64 = 29.97;

pub struct LoadVideo {
    hwnd: isize,
    frames: Receiver<Vec<u8>>,
    current: Option<Vec<u8>>,
    video_proc: Child,
    audio_proc: Option<Child>,
    _audio: Option<SendStream>,
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
            .args(["-an", "-f", "rawvideo", "-pix_fmt", "bgra", "-s", &format!("{W}x{H}"), "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut out = video_proc.stdout.take().ok_or_else(|| anyhow::anyhow!("no ffmpeg stdout"))?;
        let (tx, frames) = sync_channel::<Vec<u8>>(8);
        std::thread::Builder::new().name("loadvideo-frames".into()).spawn(move || loop {
            let mut buf = vec![0u8; (W * H * 4) as usize];
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
        log::info(&format!("loading video: {}", video.display()));
        Ok(Self {
            hwnd: window as isize,
            frames,
            current: None,
            video_proc,
            audio_proc,
            _audio: audio,
            start: Instant::now(),
            shown: 0,
            ended: false,
        })
    }

    /// Advance to the frame due now and paint it (UI thread, every tick). Returns true once
    /// the video has played to its end.
    pub fn tick(&mut self) -> bool {
        let due = (self.start.elapsed().as_secs_f64() * FPS) as u64 + 1;
        while !self.ended && self.shown < due {
            match self.frames.try_recv() {
                Ok(f) => {
                    self.shown += 1;
                    self.current = Some(f);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => self.ended = true,
            }
        }
        self.paint();
        self.ended
    }

    fn paint(&self) {
        use windows_sys::Win32::Foundation::{HWND, RECT};
        use windows_sys::Win32::Graphics::Gdi::*;
        use windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect;
        let hwnd = self.hwnd as HWND;
        unsafe {
            let mut rc: RECT = std::mem::zeroed();
            GetClientRect(hwnd, &mut rc);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            let hdc = GetDC(hwnd);
            if hdc.is_null() {
                return;
            }
            // 16:9 letterbox, black bars.
            let s = crate::menu::Menu::stage(w as f32, h as f32);
            let (x, y, sw, sh) = (s.x as i32, s.y as i32, s.w as i32, s.h as i32);
            let black = GetStockObject(BLACK_BRUSH);
            for r in [
                RECT { left: 0, top: 0, right: w, bottom: y },
                RECT { left: 0, top: y + sh, right: w, bottom: h },
                RECT { left: 0, top: 0, right: x, bottom: h },
                RECT { left: x + sw, top: 0, right: w, bottom: h },
            ] {
                FillRect(hdc, &r, black as HBRUSH);
            }
            if let Some(f) = &self.current {
                let mut bmi: BITMAPINFO = std::mem::zeroed();
                bmi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
                bmi.bmiHeader.biWidth = W;
                bmi.bmiHeader.biHeight = -H; // top-down rows
                bmi.bmiHeader.biPlanes = 1;
                bmi.bmiHeader.biBitCount = 32;
                bmi.bmiHeader.biCompression = BI_RGB as u32;
                SetStretchBltMode(hdc, HALFTONE);
                StretchDIBits(hdc, x, y, sw, sh, 0, 0, W, H, f.as_ptr() as *const _, &bmi, DIB_RGB_COLORS, SRCCOPY);
            } else {
                let r = RECT { left: x, top: y, right: x + sw, bottom: y + sh };
                FillRect(hdc, &r, black as HBRUSH);
            }
            ReleaseDC(hwnd, hdc);
        }
    }
}

impl Drop for LoadVideo {
    fn drop(&mut self) {
        let _ = self.video_proc.kill();
        if let Some(p) = self.audio_proc.as_mut() {
            let _ = p.kill();
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
