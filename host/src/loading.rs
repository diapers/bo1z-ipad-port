//! Boot loading bar. The web build shows a loading cinematic (stubbed here as already
//! ended), so the screen would stay black while the fastfiles load. Progress is the share
//! of the pack's zone bytes read so far; the bar disappears once the engine reports the
//! map's own zone as loaded ("KISAK_STARTUP leave=<zone> ...").

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::OnceLock;

static READ: AtomicU64 = AtomicU64::new(0);
static TOTAL: AtomicU64 = AtomicU64::new(0);
static DONE: AtomicBool = AtomicBool::new(false);
static ZONE_MARK: OnceLock<String> = OnceLock::new();

/// Called at startup with the pack directory and the map's zone name.
pub fn init(pack_dir: &Path, zone: &str) {
    fn ff_bytes(dir: &Path) -> u64 {
        let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
        rd.flatten()
            .map(|e| {
                let p = e.path();
                if p.is_dir() {
                    ff_bytes(&p)
                } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("ff")) {
                    e.metadata().map_or(0, |m| m.len())
                } else {
                    0
                }
            })
            .sum()
    }
    TOTAL.store(ff_bytes(&pack_dir.join("zone")).max(1), Ordering::Relaxed);
    let _ = ZONE_MARK.set(format!("leave={zone} "));
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

/// Desktop: draw the bar on framebuffer 0 before a present (the engine presents rarely
/// while it loads, so this only updates now and then; iOS uses `Overlay` instead). GL state the engine relies
/// on (framebuffer, scissor, clear colour, colour mask) is restored afterwards.
#[cfg(not(target_os = "ios"))]
pub fn draw(w: i32, h: i32) {
    if done() || w <= 0 || h <= 0 {
        return;
    }
    let p = progress();
    let bw = (w as f32 * 0.5) as i32;
    let bh = (h / 90).max(4);
    let x = (w - bw) / 2;
    let y = h / 8;
    unsafe {
        let mut fb = 0i32;
        let mut scissor_box = [0i32; 4];
        let mut clear = [0f32; 4];
        let mut mask = [0u8; 4];
        gl::GetIntegerv(gl::DRAW_FRAMEBUFFER_BINDING, &mut fb);
        gl::GetIntegerv(gl::SCISSOR_BOX, scissor_box.as_mut_ptr());
        gl::GetFloatv(gl::COLOR_CLEAR_VALUE, clear.as_mut_ptr());
        gl::GetBooleanv(gl::COLOR_WRITEMASK, mask.as_mut_ptr());
        let scissor_on = gl::IsEnabled(gl::SCISSOR_TEST);

        gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, 0);
        gl::Enable(gl::SCISSOR_TEST);
        gl::ColorMask(1, 1, 1, 1);
        // Track (dark grey, 1 px border) then fill.
        gl::Scissor(x - 1, y - 1, bw + 2, bh + 2);
        gl::ClearColor(0.18, 0.18, 0.18, 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        gl::Scissor(x, y, (bw as f32 * p) as i32, bh);
        gl::ClearColor(0.62, 0.08, 0.06, 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);

        gl::ClearColor(clear[0], clear[1], clear[2], clear[3]);
        gl::ColorMask(mask[0], mask[1], mask[2], mask[3]);
        gl::Scissor(scissor_box[0], scissor_box[1], scissor_box[2], scissor_box[3]);
        if scissor_on == 0 {
            gl::Disable(gl::SCISSOR_TEST);
        }
        gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, fb as u32);
    }
}

/// iOS: the bar as two UIKit views over the game view, driven from the UI thread so it
/// moves even while the engine thread is busy loading and not presenting.
#[cfg(target_os = "ios")]
pub struct Overlay {
    track: objc2::rc::Retained<objc2_ui_kit::UIView>,
    fill: objc2::rc::Retained<objc2_ui_kit::UIView>,
    parent: objc2::rc::Retained<objc2_ui_kit::UIView>,
}

#[cfg(target_os = "ios")]
impl Overlay {
    pub fn new(parent: &objc2_ui_kit::UIView) -> Option<Self> {
        use objc2::{MainThreadOnly, Message};
        use objc2_core_foundation::CGRect;
        use objc2_ui_kit::{UIColor, UIView};
        let mtm = objc2::MainThreadMarker::new()?;
        let track = UIView::initWithFrame(UIView::alloc(mtm), CGRect::default());
        let fill = UIView::initWithFrame(UIView::alloc(mtm), CGRect::default());
        track.setBackgroundColor(Some(&UIColor::colorWithRed_green_blue_alpha(0.18, 0.18, 0.18, 1.0)));
        fill.setBackgroundColor(Some(&UIColor::colorWithRed_green_blue_alpha(0.62, 0.08, 0.06, 1.0)));
        parent.addSubview(&track);
        parent.addSubview(&fill);
        let o = Self { track, fill, parent: parent.retain() };
        o.update();
        Some(o)
    }

    /// Resize to the current progress; returns false (and removes the views) when done.
    pub fn update(&self) -> bool {
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        if done() {
            self.track.removeFromSuperview();
            self.fill.removeFromSuperview();
            return false;
        }
        let b = self.parent.bounds();
        let (w, h) = (b.size.width, b.size.height);
        let bw = w * 0.5;
        let bh = (h / 90.0).max(4.0);
        let x = (w - bw) / 2.0;
        let y = h - h / 8.0 - bh;
        self.track.setFrame(CGRect::new(CGPoint::new(x - 1.0, y - 1.0), CGSize::new(bw + 2.0, bh + 2.0)));
        self.fill.setFrame(CGRect::new(CGPoint::new(x, y), CGSize::new(bw * progress() as f64, bh)));
        true
    }
}
