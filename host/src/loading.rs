//! Loading flow, from the moment a map is picked until the game starts.
//!
//! As on the site, the map's loading cinematic (web/video/<zone>_load, converted to
//! <menu>/load-<slug>.mp4 by convert-videos.mjs) plays over the game view while the engine
//! loads. When it ends first, the engine's own loading screen shows once loading is done.
//! Once the map's zone has loaded ("KISAK_STARTUP leave=<zone> ...") and the video is gone,
//! the intro gate (KB_Input 7) opens and the game starts; a press skips a video that is
//! still running after load. Nothing is drawn over the game's own screens.
//!
//! The start also waits (platform.rs, at most 90 s after load) until the game would run
//! smoothly: background shader builds done, no engine program still compiling, texture
//! streaming quiet and the held first frame rendering without hitches. A video that ends
//! first keeps its last frame up with a status line meanwhile.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

static DONE: AtomicBool = AtomicBool::new(false);
static RELEASED: AtomicBool = AtomicBool::new(false);
static ZONE_MARK: OnceLock<String> = OnceLock::new();
/// The chosen map's loading cinematic, if converted.
static VIDEO: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Called when a map is chosen, with its zone name.
pub fn init(zone: &str) {
    let _ = ZONE_MARK.set(format!("leave={zone} "));
}

pub fn set_video(p: Option<PathBuf>) {
    *VIDEO.lock().unwrap() = p;
}

pub fn video() -> Option<PathBuf> {
    VIDEO.lock().unwrap().clone()
}

/// File bytes the engine read (fs.rs). Kept for the call sites; no progress bar any more.
static READ_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// File data the engine read (for the frame stats log line).
pub fn add_read(n: usize) {
    READ_BYTES.fetch_add(n as u64, Ordering::Relaxed);
    READ_TOTAL.fetch_add(n as u64, Ordering::Relaxed);
}

static READ_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// All file data read so far (the start gate waits for texture streaming to calm down).
pub fn read_total() -> u64 {
    READ_TOTAL.load(Ordering::Relaxed)
}

pub fn take_read_bytes() -> u64 {
    READ_BYTES.swap(0, Ordering::Relaxed)
}

/// Engine log lines (log.rs) mark the end of loading.
pub fn engine_line(msg: &str) {
    if !DONE.load(Ordering::Relaxed) && msg.contains("KISAK_STARTUP") {
        if let Some(mark) = ZONE_MARK.get() {
            if msg.contains(mark.as_str()) {
                DONE.store(true, Ordering::Relaxed);
                crate::log::info("loading finished");
            }
        }
    }
}

/// The intro gate was released: the game has started.
pub fn released() {
    RELEASED.store(true, Ordering::Relaxed);
}

pub fn is_released() -> bool {
    RELEASED.load(Ordering::Relaxed)
}

pub fn done() -> bool {
    DONE.load(Ordering::Relaxed)
}

/// iOS: the cinematic as an AVPlayerLayer in a black view over the game view.
#[cfg(target_os = "ios")]
pub struct VideoOverlay {
    root: objc2::rc::Retained<objc2_ui_kit::UIView>,
    player: objc2::rc::Retained<objc2_av_foundation::AVPlayer>,
    layer: objc2::rc::Retained<objc2_av_foundation::AVPlayerLayer>,
    parent: objc2::rc::Retained<objc2_ui_kit::UIView>,
    started: bool,
    status: objc2::rc::Retained<objc2_ui_kit::UILabel>,
}

#[cfg(target_os = "ios")]
impl VideoOverlay {
    pub fn new(parent: &objc2_ui_kit::UIView, video: &std::path::Path) -> Option<Self> {
        use objc2::{MainThreadOnly, Message};
        use objc2_av_foundation::{AVPlayer, AVPlayerLayer};
        use objc2_foundation::{NSString, NSURL};
        use objc2_ui_kit::{UIColor, UIView};
        let mtm = objc2::MainThreadMarker::new()?;
        let root = UIView::initWithFrame(UIView::alloc(mtm), parent.bounds());
        root.setBackgroundColor(Some(&UIColor::blackColor()));
        let url = NSURL::fileURLWithPath(&NSString::from_str(&video.to_string_lossy()));
        let (player, layer) = unsafe {
            let player = AVPlayer::playerWithURL(&url, mtm);
            player.setVolume(0.6);
            let layer = AVPlayerLayer::playerLayerWithPlayer(Some(&player));
            (player, layer)
        };
        layer.setFrame(parent.bounds());
        root.layer().addSublayer(&layer);
        let status = {
            use objc2_core_foundation::CGRect;
            use objc2_ui_kit::{NSTextAlignment, UIFont, UILabel};
            let l = UILabel::initWithFrame(UILabel::alloc(mtm), CGRect::default());
            unsafe {
                l.setTextColor(Some(&UIColor::colorWithRed_green_blue_alpha(0.85, 0.85, 0.85, 1.0)));
                l.setFont(Some(&UIFont::boldSystemFontOfSize(15.0)));
            }
            l.setTextAlignment(NSTextAlignment(2)); // right
            root.addSubview(&l);
            l
        };
        parent.addSubview(&root);
        unsafe { player.play() };
        crate::log::info(&format!("loading video: {}", video.display()));
        Some(Self { root, player, layer, parent: parent.retain(), started: false, status })
    }

    /// A short line in the bottom-right corner ("" hides it).
    pub fn set_status(&self, text: &str) {
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        let b = self.parent.bounds();
        self.status.setFrame(CGRect::new(
            CGPoint::new(b.size.width * 0.4, b.size.height - 44.0),
            CGSize::new(b.size.width * 0.6 - 24.0, 28.0),
        ));
        self.status.setText(Some(&objc2_foundation::NSString::from_str(text)));
    }

    /// Keep the layout current; returns true once the video has played to its end.
    pub fn tick(&mut self) -> bool {
        use objc2_av_foundation::AVPlayerTimeControlStatus;
        let b = self.parent.bounds();
        self.root.setFrame(b);
        self.layer.setFrame(b);
        let status = unsafe { self.player.timeControlStatus() };
        if status == AVPlayerTimeControlStatus::Playing {
            self.started = true;
        }
        // AVPlayer pauses at the end of the item (actionAtItemEnd = pause).
        self.started && status == AVPlayerTimeControlStatus::Paused
    }

    pub fn remove(self) {
        unsafe { self.player.pause() };
        self.layer.removeFromSuperlayer();
        self.root.removeFromSuperview();
    }
}
