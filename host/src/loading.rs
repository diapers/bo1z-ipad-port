//! Loading screen, from the moment a map is picked until the player starts.
//!
//! The web build shows a loading cinematic (stubbed here as already ended), so the engine
//! would show black while its fastfiles load. Instead this shows the map's own BO1 loading
//! screen (loadscreen_<zone>, extracted by extract-loadscreens.mjs to
//! <game>/menu/loadscreen-<slug>.png), stretched full screen as the engine draws it, with a
//! progress bar and a status line; once the map's own
//! zone has loaded ("KISAK_STARTUP leave=<zone> ...") it asks the player to start, and it
//! disappears when the intro gate is released (KB_Input(7), see rt::post_input).
//! Progress is the share of the pack's zone bytes read so far.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

static READ: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);
static DONE: AtomicBool = AtomicBool::new(false);
static RELEASED: AtomicBool = AtomicBool::new(false);
static ZONE_MARK: OnceLock<String> = OnceLock::new();
/// The chosen map's loading screen image, if the art exists.
static PANEL: Mutex<Option<PathBuf>> = Mutex::new(None);
/// The chosen map's loading cinematic (load-<slug>.mp4), if converted.
static VIDEO: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Desktop: the video window covers the game, so the engine-thread draw can skip work.
static VIDEO_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn set_video(p: Option<PathBuf>) {
    *VIDEO.lock().unwrap() = p;
}

pub fn video() -> Option<PathBuf> {
    VIDEO.lock().unwrap().clone()
}

pub fn set_video_active(on: bool) {
    VIDEO_ACTIVE.store(on, Ordering::Relaxed);
}

/// Called when a map is chosen: the bytes of its zone (.ff) files and its zone name.
pub fn init(total_ff_bytes: u64, zone: &str) {
    TOTAL.store(total_ff_bytes.max(1), Ordering::Relaxed);
    let _ = ZONE_MARK.set(format!("leave={zone} "));
}

pub fn set_panel(p: Option<PathBuf>) {
    *PANEL.lock().unwrap() = p;
}

fn panel() -> Option<PathBuf> {
    PANEL.lock().unwrap().clone()
}

/// File bytes the engine read (fs.rs).
pub fn add_read(n: usize) {
    if !DONE.load(Ordering::Relaxed) {
        READ.fetch_add(n as u64, Ordering::Relaxed);
    }
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

/// The intro gate was released: the player is in, the loading screen goes away.
pub fn released() {
    RELEASED.store(true, Ordering::Relaxed);
}

pub fn is_released() -> bool {
    RELEASED.load(Ordering::Relaxed)
}

/// Load progress in [0, 0.99]; 1 once the map is loaded.
pub fn progress() -> f32 {
    if DONE.load(Ordering::Relaxed) {
        return 1.0;
    }
    (READ.load(Ordering::Relaxed) as f64 / TOTAL.load(Ordering::Relaxed) as f64).clamp(0.0, 0.99) as f32
}

pub fn done() -> bool {
    DONE.load(Ordering::Relaxed)
}

fn status() -> &'static str {
    if done() {
        if cfg!(target_os = "ios") { "PRESS A TO START" } else { "PRESS A, ENTER OR CLICK TO START" }
    } else {
        "LOADING"
    }
}

// Layout on the 1920x1080 stage (letterboxed like the map select): the loading screen
// fills the stage; the bar and status sit along the bottom edge.
const PANEL_RECT: crate::menu::Rect = crate::menu::Rect { x: 0.0, y: 0.0, w: 1920.0, h: 1080.0 };
const BAR_RECT: crate::menu::Rect = crate::menu::Rect { x: 360.0, y: 1036.0, w: 1200.0, h: 10.0 };
const TEXT_RECT: crate::menu::Rect = crate::menu::Rect { x: 360.0, y: 990.0, w: 1200.0, h: 36.0 };

fn place(r: crate::menu::Rect, w: f32, h: f32) -> crate::menu::Rect {
    let s = crate::menu::Menu::stage(w, h);
    let k = s.w / 1920.0;
    crate::menu::Rect { x: s.x + r.x * k, y: s.y + r.y * k, w: r.w * k, h: r.h * k }
}

/// Desktop: draw the whole loading screen on framebuffer 0, over whatever the engine drew.
/// Called before each present (engine thread) and once when the map is picked (main
/// thread). GL state the engine relies on is restored afterwards.
#[cfg(not(target_os = "ios"))]
pub fn draw(w: i32, h: i32) {
    if VIDEO_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    draw_over(w, h, || {
        if let Some(p) = panel() {
            crate::menu::with_gl_art(|art| art.blit(&p, place(PANEL_RECT, w as f32, h as f32), h));
        }
    });
}

/// The loading screen with `background` drawn first (a still or a video frame), in
/// whichever context is current.
#[cfg(not(target_os = "ios"))]
pub fn draw_over(w: i32, h: i32, background: impl FnOnce()) {
    if is_released() || w <= 0 || h <= 0 {
        return;
    }
    let (fw, fh) = (w as f32, h as f32);
    unsafe {
        let mut draw_fb = 0i32;
        let mut read_fb = 0i32;
        let mut scissor_box = [0i32; 4];
        let mut clear = [0f32; 4];
        let mut mask = [0u8; 4];
        gl::GetIntegerv(gl::DRAW_FRAMEBUFFER_BINDING, &mut draw_fb);
        gl::GetIntegerv(gl::READ_FRAMEBUFFER_BINDING, &mut read_fb);
        gl::GetIntegerv(gl::SCISSOR_BOX, scissor_box.as_mut_ptr());
        gl::GetFloatv(gl::COLOR_CLEAR_VALUE, clear.as_mut_ptr());
        gl::GetBooleanv(gl::COLOR_WRITEMASK, mask.as_mut_ptr());
        let scissor_on = gl::IsEnabled(gl::SCISSOR_TEST);

        gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, 0);
        gl::ColorMask(1, 1, 1, 1);
        gl::Disable(gl::SCISSOR_TEST);
        gl::ClearColor(0.0, 0.0, 0.0, 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        background();
        gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, 0);
        gl::Enable(gl::SCISSOR_TEST);
        let rect = |x: i32, y_top: i32, rw: i32, rh: i32, c: [f32; 3]| {
            gl::Scissor(x, h - y_top - rh, rw.max(0), rh.max(0));
            gl::ClearColor(c[0], c[1], c[2], 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        };
        let b = place(BAR_RECT, fw, fh);
        rect(b.x as i32 - 1, b.y as i32 - 1, b.w as i32 + 2, b.h as i32 + 2, [0.18, 0.18, 0.18]);
        rect(b.x as i32, b.y as i32, (b.w * progress()) as i32, b.h as i32, [0.62, 0.08, 0.06]);
        let t = place(TEXT_RECT, fw, fh);
        let px = ((t.h * 0.7) as i32 / 7).max(1);
        // Dark band behind the text so it reads over any loading screen.
        rect(t.x as i32, t.y as i32, t.w as i32, t.h as i32, [0.0, 0.0, 0.0]);
        crate::menu::text(&rect, status(), t, px, [0.9, 0.9, 0.9]);

        gl::ClearColor(clear[0], clear[1], clear[2], clear[3]);
        gl::ColorMask(mask[0], mask[1], mask[2], mask[3]);
        gl::Scissor(scissor_box[0], scissor_box[1], scissor_box[2], scissor_box[3]);
        if scissor_on == 0 {
            gl::Disable(gl::SCISSOR_TEST);
        }
        gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, draw_fb as u32);
        gl::BindFramebuffer(gl::READ_FRAMEBUFFER, read_fb as u32);
    }
}

/// iOS: the loading screen as UIKit views over the game view, driven from the UI thread
/// so it moves even while the engine thread is busy loading and not presenting.
#[cfg(target_os = "ios")]
pub struct Overlay {
    root: objc2::rc::Retained<objc2_ui_kit::UIView>,
    panel: objc2::rc::Retained<objc2_ui_kit::UIImageView>,
    track: objc2::rc::Retained<objc2_ui_kit::UIView>,
    fill: objc2::rc::Retained<objc2_ui_kit::UIView>,
    label: objc2::rc::Retained<objc2_ui_kit::UILabel>,
    parent: objc2::rc::Retained<objc2_ui_kit::UIView>,
    /// The loading cinematic, playing under the bar.
    player: Option<(objc2::rc::Retained<objc2_av_foundation::AVPlayer>, objc2::rc::Retained<objc2_av_foundation::AVPlayerLayer>)>,
}

#[cfg(target_os = "ios")]
impl Overlay {
    pub fn new(parent: &objc2_ui_kit::UIView) -> Option<Self> {
        use objc2::{MainThreadOnly, Message};
        use objc2_core_foundation::CGRect;
        use objc2_foundation::NSString;
        use objc2_ui_kit::{NSTextAlignment, UIColor, UIFont, UIImage, UIImageView, UILabel, UIView};
        let mtm = objc2::MainThreadMarker::new()?;
        let root = UIView::initWithFrame(UIView::alloc(mtm), parent.bounds());
        root.setBackgroundColor(Some(&UIColor::blackColor()));
        // The cinematic when there is one (AVPlayer keeps its last frame after it ends),
        // else the map's still loading screen.
        let player = video().map(|v| unsafe {
            use objc2_av_foundation::{AVPlayer, AVPlayerLayer};
            let url = objc2_foundation::NSURL::fileURLWithPath(&NSString::from_str(&v.to_string_lossy()));
            let player = AVPlayer::playerWithURL(&url, mtm);
            player.setVolume(0.6);
            let layer = AVPlayerLayer::playerLayerWithPlayer(Some(&player));
            root.layer().addSublayer(&layer);
            player.play();
            (player, layer)
        });
        let img = if player.is_some() {
            None
        } else {
            panel().and_then(|p| UIImage::imageWithContentsOfFile(&NSString::from_str(&p.to_string_lossy())))
        };
        let panel_view = UIImageView::initWithImage(UIImageView::alloc(mtm), img.as_deref());
        let track = UIView::initWithFrame(UIView::alloc(mtm), CGRect::default());
        let fill = UIView::initWithFrame(UIView::alloc(mtm), CGRect::default());
        track.setBackgroundColor(Some(&UIColor::colorWithRed_green_blue_alpha(0.18, 0.18, 0.18, 1.0)));
        fill.setBackgroundColor(Some(&UIColor::colorWithRed_green_blue_alpha(0.62, 0.08, 0.06, 1.0)));
        let label = UILabel::initWithFrame(UILabel::alloc(mtm), CGRect::default());
        unsafe {
            label.setTextColor(Some(&UIColor::whiteColor()));
            label.setFont(Some(&UIFont::boldSystemFontOfSize(20.0)));
            label.setBackgroundColor(Some(&UIColor::colorWithRed_green_blue_alpha(0.0, 0.0, 0.0, 0.6)));
        }
        label.setTextAlignment(NSTextAlignment(1)); // center
        root.addSubview(&panel_view);
        root.addSubview(&track);
        root.addSubview(&fill);
        root.addSubview(&label);
        parent.addSubview(&root);
        let o = Self { root, panel: panel_view, track, fill, label, parent: parent.retain(), player };
        o.update();
        Some(o)
    }

    /// Refresh; returns false (and removes the views) once the player has started.
    pub fn update(&self) -> bool {
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        use objc2_foundation::NSString;
        if is_released() {
            if let Some((p, l)) = &self.player {
                unsafe { p.pause() };
                l.removeFromSuperlayer();
            }
            self.root.removeFromSuperview();
            return false;
        }
        let b = self.parent.bounds();
        let (w, h) = (b.size.width as f32, b.size.height as f32);
        let frame = |r: crate::menu::Rect| CGRect::new(CGPoint::new(r.x as f64, r.y as f64), CGSize::new(r.w as f64, r.h as f64));
        self.root.setFrame(b);
        self.panel.setFrame(frame(place(PANEL_RECT, w, h)));
        if let Some((_, l)) = &self.player {
            l.setFrame(frame(place(PANEL_RECT, w, h)));
        }
        let bar = place(BAR_RECT, w, h);
        self.track.setFrame(frame(crate::menu::Rect { x: bar.x - 1.0, y: bar.y - 1.0, w: bar.w + 2.0, h: bar.h + 2.0 }));
        self.fill.setFrame(frame(crate::menu::Rect { w: bar.w * progress(), ..bar }));
        self.label.setFrame(frame(place(TEXT_RECT, w, h)));
        self.label.setText(Some(&NSString::from_str(status())));
        true
    }
}
