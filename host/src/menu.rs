//! Launch-time map selector. Nothing of a map is touched until one is chosen; then the
//! engine boots with it behind the loading bar.
//!
//! With the site's map-select art in `<game>/menu` (mapselect-*.webp, extracted from
//! vel.gg/bo1z's landing page) this is the site's MISSION SELECT screen: a 1920x1080
//! background, one 450x40 row strip per map (the highlighted strip when selected) and the
//! selected map's 750x720 preview panel, letterboxed to 16:9. Layout numbers are
//! landing.js's (ROW x 352 y 259 pitch 42, panel 850,240). Without the art, plain tiles.
//!
//! Front ends: UIKit image views on iOS; GL framebuffer blits on desktop (no shaders).
//! Input: pad (d-pad / left stick + A or Start), keyboard (arrows + Enter), tap/click.
//!
//! Below the maps, a SETTINGS row opens the settings (settings.rs) in the panel area:
//! up/down picks an option, left/right or A changes it, B (Escape) goes back; a tap cycles one.

use std::path::{Path, PathBuf};

use crate::input::{Btn, PadState};
use crate::maps::MapInfo;
use crate::settings::{Settings, OPTIONS};

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// The site's stage is authored at 1920x1080.
const STAGE_W: f32 = 1920.0;
const STAGE_H: f32 = 1080.0;
const ROW: Rect = Rect { x: 352.0, y: 259.0, w: 450.0, h: 40.0 };
const ROW_PITCH: f32 = 42.0;
const PANEL: Rect = Rect { x: 850.0, y: 240.0, w: 750.0, h: 720.0 };

pub struct Menu {
    pub maps: Vec<MapInfo>,
    /// A map index, or `maps.len()` for the SETTINGS row.
    pub sel: usize,
    pub settings: Settings,
    /// The settings panel is open (input goes to it).
    pub in_settings: bool,
    /// Selected option in the settings panel.
    pub opt: usize,
    /// Directory holding mapselect-*.webp, when every needed image is present.
    pub art: Option<PathBuf>,
    prev: std::collections::HashSet<Btn>,
    stick_ready: bool,
    pub dirty: bool,
}

const COLS: usize = 2;

impl Menu {
    pub fn new(maps: Vec<MapInfo>, last: Option<&str>, art_dir: &Path, settings: Settings) -> Self {
        let sel = last.and_then(|l| maps.iter().position(|m| m.slug == l)).unwrap_or(0);
        let have = |n: &str| art_dir.join(n).is_file();
        let art_ok = have("mapselect-supported.webp")
            && maps.iter().all(|m| {
                have(&format!("mapselect-row-{}.webp", m.slug))
                    && have(&format!("mapselect-hl-{}.webp", m.slug))
                    && have(&format!("mapselect-panel-{}.webp", m.slug))
            });
        crate::log::info(&format!("map select art: {}", if art_ok { art_dir.display().to_string() } else { "missing, plain tiles".into() }));
        Self {
            maps,
            sel,
            settings,
            in_settings: false,
            opt: 0,
            art: art_ok.then(|| art_dir.to_path_buf()),
            prev: Default::default(),
            stick_ready: true,
            dirty: true,
        }
    }

    pub fn art_file(&self, kind: &str, slug: Option<&str>) -> Option<PathBuf> {
        let dir = self.art.as_ref()?;
        Some(dir.join(match slug {
            Some(s) => format!("mapselect-{kind}-{s}.webp"),
            None => format!("mapselect-{kind}.webp"),
        }))
    }

    // ---- layout, in the units of the surface (w, h): pixels or UIKit points ----------------

    /// The 16:9 stage, letterboxed into a w x h surface.
    pub fn stage(w: f32, h: f32) -> Rect {
        let sw = w.min(h * STAGE_W / STAGE_H);
        let sh = sw * STAGE_H / STAGE_W;
        Rect { x: (w - sw) / 2.0, y: (h - sh) / 2.0, w: sw, h: sh }
    }

    fn on_stage(stage: Rect, r: Rect) -> Rect {
        let k = stage.w / STAGE_W;
        Rect { x: stage.x + r.x * k, y: stage.y + r.y * k, w: r.w * k, h: r.h * k }
    }

    /// Art mode: map `i`'s row strip.
    pub fn row_rect(&self, i: usize, w: f32, h: f32) -> Rect {
        Self::on_stage(Self::stage(w, h), Rect { y: ROW.y + i as f32 * ROW_PITCH, ..ROW })
    }

    /// Art mode: the preview panel.
    pub fn panel_rect(&self, w: f32, h: f32) -> Rect {
        Self::on_stage(Self::stage(w, h), PANEL)
    }

    /// The SETTINGS row, under the maps.
    pub fn settings_row_rect(&self, w: f32, h: f32) -> Rect {
        if self.art.is_some() {
            Self::on_stage(Self::stage(w, h), Rect { y: ROW.y + (self.maps.len() as f32 + 0.5) * ROW_PITCH, ..ROW })
        } else {
            let last = self.item_rect(self.maps.len().saturating_sub(1), w, h);
            Rect { x: 0.05 * w, y: (last.y + last.h + 0.01 * h).min(0.95 * h), w: 0.9 * w, h: 0.04 * h }
        }
    }

    /// The settings panel: where the map preview is (art) or over the tiles (plain).
    pub fn settings_panel_rect(&self, w: f32, h: f32) -> Rect {
        if self.art.is_some() {
            self.panel_rect(w, h)
        } else {
            Rect { x: 0.15 * w, y: 0.17 * h, w: 0.7 * w, h: 0.7 * h }
        }
    }

    /// Settings panel parts: title band, option `i`'s row, and the hint line.
    pub fn settings_title_rect(&self, w: f32, h: f32) -> Rect {
        let p = self.settings_panel_rect(w, h);
        Rect { x: p.x, y: p.y + p.h * 0.03, w: p.w, h: p.h * 0.1 }
    }

    pub fn settings_opt_rect(&self, i: usize, w: f32, h: f32) -> Rect {
        let p = self.settings_panel_rect(w, h);
        let rh = p.h * 0.09;
        Rect { x: p.x + p.w * 0.04, y: p.y + p.h * 0.16 + i as f32 * rh * 1.12, w: p.w * 0.92, h: rh }
    }

    pub fn settings_hint_rect(&self, w: f32, h: f32) -> Rect {
        let p = self.settings_panel_rect(w, h);
        Rect { x: p.x, y: p.y + p.h * 0.88, w: p.w, h: p.h * 0.07 }
    }

    pub const SETTINGS_HINT: &'static str = "LEFT / RIGHT CHANGES   B GOES BACK";

    pub fn rows(&self) -> usize {
        self.maps.len().div_ceil(COLS).max(1)
    }

    /// Plain mode: title band.
    pub fn title_rect(&self, w: f32, h: f32) -> Rect {
        Rect { x: 0.05 * w, y: 0.04 * h, w: 0.9 * w, h: 0.1 * h }
    }

    /// Plain mode: tile for map `i`.
    pub fn item_rect(&self, i: usize, w: f32, h: f32) -> Rect {
        let (top, bottom, gap) = (0.17, 0.95, 0.018);
        let rows = self.rows() as f32;
        let th = (bottom - top - gap * (rows - 1.0)) / rows;
        let tw = (0.9 - gap) / COLS as f32;
        let (c, r) = ((i % COLS) as f32, (i / COLS) as f32);
        Rect { x: (0.05 + c * (tw + gap)) * w, y: (top + r * (th + gap)) * h, w: tw * w, h: th * h }
    }

    /// The hit area of map `i` in either mode.
    fn hit_rect(&self, i: usize, w: f32, h: f32) -> Rect {
        if self.art.is_some() {
            // The 2 px gaps between strips belong to the row above.
            Rect { h: ROW_PITCH * Self::stage(w, h).w / STAGE_W, ..self.row_rect(i, w, h) }
        } else {
            self.item_rect(i, w, h)
        }
    }

    // ---- input -----------------------------------------------------------------------------

    fn step(&mut self, dx: i32, dy: i32) {
        if self.in_settings {
            if dy != 0 {
                self.opt = (self.opt as i32 + dy).rem_euclid(OPTIONS.len() as i32) as usize;
            }
            if dx != 0 {
                self.settings.step(self.opt, dx);
            }
            self.dirty = true;
            return;
        }
        // The maps, then the SETTINGS row.
        let n = self.maps.len() as i32 + 1;
        let cur = self.sel as i32;
        // Art mode is a single column: left/right do nothing, up/down move by one.
        let next = if self.art.is_some() || cur == n - 1 {
            cur + dy
        } else if dx != 0 {
            cur + dx
        } else {
            (cur + dy * COLS as i32).min(n - 1)
        };
        if (0..n).contains(&next) && next != cur {
            self.sel = next as usize;
            self.dirty = true;
        }
    }

    /// A / Enter: play the selected map, open the settings, or change the selected option.
    fn accept(&mut self) -> Option<usize> {
        if self.in_settings {
            self.settings.step(self.opt, 1);
        } else if self.sel == self.maps.len() {
            self.in_settings = true;
        } else {
            return Some(self.sel);
        }
        self.dirty = true;
        None
    }

    /// B / Escape: leave the settings.
    fn back(&mut self) {
        if self.in_settings {
            self.in_settings = false;
            self.dirty = true;
        }
    }

    /// Controller tick. Returns the chosen map index on A / Start.
    pub fn pad(&mut self, p: &PadState) -> Option<usize> {
        let prev = std::mem::take(&mut self.prev);
        let pressed = |b: Btn| p.buttons.contains(&b) && !prev.contains(&b);
        let mut chosen = None;
        if pressed(Btn::Up) {
            self.step(0, -1);
        }
        if pressed(Btn::Down) {
            self.step(0, 1);
        }
        if pressed(Btn::Left) {
            self.step(-1, 0);
        }
        if pressed(Btn::Right) {
            self.step(1, 0);
        }
        // Left stick: one step per push, re-armed when it returns to centre.
        let (x, y) = (p.lx, p.ly);
        if x.abs() < 0.3 && y.abs() < 0.3 {
            self.stick_ready = true;
        } else if self.stick_ready && (x.abs() > 0.6 || y.abs() > 0.6) {
            self.stick_ready = false;
            if x.abs() > y.abs() {
                self.step(x.signum() as i32, 0);
            } else {
                self.step(0, -(y.signum() as i32)); // stick up is +y
            }
        }
        if pressed(Btn::A) || (pressed(Btn::Start) && !self.in_settings) {
            chosen = self.accept();
        } else if pressed(Btn::B) || (pressed(Btn::Start) && self.in_settings) {
            self.back();
        }
        self.prev = p.buttons.clone();
        chosen
    }

    /// Keyboard: arrows move, Enter/Space chooses, Escape leaves the settings.
    pub fn key(&mut self, code: winit::keyboard::KeyCode) -> Option<usize> {
        use winit::keyboard::KeyCode as K;
        match code {
            K::ArrowUp | K::KeyW => self.step(0, -1),
            K::ArrowDown | K::KeyS => self.step(0, 1),
            K::ArrowLeft | K::KeyA => self.step(-1, 0),
            K::ArrowRight | K::KeyD => self.step(1, 0),
            K::Enter | K::NumpadEnter | K::Space => return self.accept(),
            K::Escape | K::Backspace => self.back(),
            _ => {}
        }
        None
    }

    /// Tap/click at (x, y) on a w x h surface: plays the map under it; the SETTINGS row
    /// opens or closes the settings; an option row cycles that option.
    pub fn tap(&mut self, x: f32, y: f32, w: f32, h: f32) -> Option<usize> {
        if self.settings_row_rect(w, h).contains(x, y) {
            self.sel = self.maps.len();
            self.in_settings = !self.in_settings;
            self.dirty = true;
            return None;
        }
        if self.in_settings {
            if let Some(i) = (0..OPTIONS.len()).find(|&i| self.settings_opt_rect(i, w, h).contains(x, y)) {
                self.opt = i;
                self.settings.step(i, 1);
                self.dirty = true;
                return None;
            }
            if self.settings_panel_rect(w, h).contains(x, y) {
                return None;
            }
        }
        let hit = (0..self.maps.len()).find(|&i| self.hit_rect(i, w, h).contains(x, y));
        if hit.is_some() {
            self.in_settings = false;
        }
        hit
    }

    /// Mouse hover selects the row under the cursor (the site keeps the last one selected).
    pub fn hover(&mut self, x: f32, y: f32, w: f32, h: f32) {
        if self.in_settings {
            if let Some(i) = (0..OPTIONS.len()).find(|&i| self.settings_opt_rect(i, w, h).contains(x, y)) {
                if i != self.opt {
                    self.opt = i;
                    self.dirty = true;
                }
            }
            return;
        }
        let row = (0..self.maps.len())
            .find(|&i| self.hit_rect(i, w, h).contains(x, y))
            .or_else(|| self.settings_row_rect(w, h).contains(x, y).then_some(self.maps.len()));
        if let Some(i) = row {
            if i != self.sel {
                self.sel = i;
                self.dirty = true;
            }
        }
    }
}

pub const BG: [f32; 3] = [0.04, 0.04, 0.05];
pub const TILE: [f32; 3] = [0.14, 0.14, 0.16];
pub const TILE_SEL: [f32; 3] = [0.62, 0.08, 0.06];
pub const PANEL_BG: [f32; 3] = [0.07, 0.07, 0.08];

// ---------------------------------------------------------------------------------------
// Desktop rendering: every image is opaque, so each one is a framebuffer blit from a
// texture-backed FBO (no shaders, no blending). Plain mode: scissor clears + 5x7 font.

/// Decode a .webp or .png file: (width, height, pixels, has alpha).
#[cfg(not(target_os = "ios"))]
fn decode(path: &Path) -> Option<(u32, u32, Vec<u8>, bool)> {
    let bytes = std::fs::read(path).ok()?;
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")) {
        let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
        dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut r = dec.read_info().ok()?;
        let mut buf = vec![0u8; r.output_buffer_size()?];
        let info = r.next_frame(&mut buf).ok()?;
        buf.truncate(info.buffer_size());
        let alpha = match info.color_type {
            png::ColorType::Rgba => true,
            png::ColorType::Rgb => false,
            _ => return None,
        };
        return Some((info.width, info.height, buf, alpha));
    }
    let mut dec = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let (w, h) = dec.dimensions();
    let mut buf = vec![0u8; dec.output_buffer_size()?];
    dec.read_image(&mut buf).ok()?;
    Some((w, h, buf, dec.has_alpha()))
}

/// The GL images live in the one EGL context, used by the menu (main thread) and then by
/// the loading screen (engine thread, before presents); never both at once.
#[cfg(not(target_os = "ios"))]
static GL_ART: std::sync::Mutex<Option<GlArt>> = std::sync::Mutex::new(None);

/// Run `f` with the shared GL image cache (context must be current).
#[cfg(not(target_os = "ios"))]
pub fn with_gl_art<R>(f: impl FnOnce(&mut GlArt) -> R) -> R {
    let mut g = GL_ART.lock().unwrap();
    f(g.get_or_insert_with(GlArt::new))
}

#[cfg(not(target_os = "ios"))]
pub struct GlArt {
    fbo: u32,
    images: std::collections::HashMap<PathBuf, (u32, i32, i32)>,
}

#[cfg(not(target_os = "ios"))]
impl GlArt {
    pub fn new() -> Self {
        let mut fbo = 0;
        unsafe { gl::GenFramebuffers(1, &mut fbo) };
        Self { fbo, images: Default::default() }
    }

    fn texture(&mut self, path: &Path) -> Option<(u32, i32, i32)> {
        if let Some(t) = self.images.get(path) {
            return Some(*t);
        }
        let (w, h, buf, alpha) = decode(path)?;
        let fmt = if alpha { gl::RGBA } else { gl::RGB };
        let mut tex = 0;
        unsafe {
            gl::GenTextures(1, &mut tex);
            gl::BindTexture(gl::TEXTURE_2D, tex);
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            gl::TexImage2D(gl::TEXTURE_2D, 0, fmt as i32, w as i32, h as i32, 0, fmt, gl::UNSIGNED_BYTE, buf.as_ptr() as *const _);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        }
        let t = (tex, w as i32, h as i32);
        self.images.insert(path.to_path_buf(), t);
        Some(t)
    }

    /// Draw `path` into `r` (top-left origin) on a surface `sh` pixels tall.
    pub fn blit(&mut self, path: &Path, r: Rect, sh: i32) {
        let Some((tex, w, h)) = self.texture(path) else { return };
        unsafe {
            gl::BindFramebuffer(gl::READ_FRAMEBUFFER, self.fbo);
            gl::FramebufferTexture2D(gl::READ_FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, tex, 0);
            gl::BindFramebuffer(gl::DRAW_FRAMEBUFFER, 0);
            // Image rows are top-down: flip vertically by swapping the destination's y.
            let (x0, x1) = (r.x.round() as i32, (r.x + r.w).round() as i32);
            let (y_top, y_bot) = (sh - r.y.round() as i32, sh - (r.y + r.h).round() as i32);
            gl::BlitFramebuffer(0, 0, w, h, x0, y_top, x1, y_bot, gl::COLOR_BUFFER_BIT, gl::LINEAR);
        }
    }

}

#[cfg(not(target_os = "ios"))]
pub fn draw_gl(menu: &Menu, w: i32, h: i32) {
    with_gl_art(|art| draw_gl_with(menu, art, w, h))
}

#[cfg(not(target_os = "ios"))]
fn draw_gl_with(menu: &Menu, art: &mut GlArt, w: i32, h: i32) {
    unsafe {
        gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
        gl::Viewport(0, 0, w, h);
        gl::Disable(gl::SCISSOR_TEST);
        gl::ClearColor(0.0, 0.0, 0.0, 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
    }
    let (fw, fh) = (w as f32, h as f32);
    if menu.art.is_some() {
        if let Some(bg) = menu.art_file("supported", None) {
            art.blit(&bg, Menu::stage(fw, fh), h);
        }
        for (i, m) in menu.maps.iter().enumerate() {
            let kind = if i == menu.sel { "hl" } else { "row" };
            if let Some(p) = menu.art_file(kind, Some(m.slug)) {
                art.blit(&p, menu.row_rect(i, fw, fh), h);
            }
        }
        if let Some(p) = menu.maps.get(menu.sel).and_then(|m| menu.art_file("panel", Some(m.slug))) {
            art.blit(&p, menu.panel_rect(fw, fh), h);
        }
        unsafe { gl::BindFramebuffer(gl::FRAMEBUFFER, 0) };
        draw_gl_settings(menu, w, h);
        return;
    }
    let rect = |x: i32, y_top: i32, rw: i32, rh: i32, c: [f32; 3]| unsafe {
        gl::Scissor(x, h - y_top - rh, rw.max(0), rh.max(0));
        gl::ClearColor(c[0], c[1], c[2], 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
    };
    unsafe {
        gl::ClearColor(BG[0], BG[1], BG[2], 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        gl::Enable(gl::SCISSOR_TEST);
    }
    // One pixel size for every tile, fitted to the longest name.
    let longest = menu.maps.iter().map(|m| m.name.len()).max().unwrap_or(1) as i32;
    let tile = menu.item_rect(0, fw, fh);
    let px = (((tile.w * 0.8) as i32) / (longest * 6)).min((tile.h * 0.5) as i32 / 7).max(1);
    let t = menu.title_rect(fw, fh);
    text(&rect, "CHOOSE A MAP", t, px.max(2) + 1, [0.9, 0.9, 0.9]);
    for (i, m) in menu.maps.iter().enumerate() {
        let r = menu.item_rect(i, fw, fh);
        rect(r.x as i32, r.y as i32, r.w as i32, r.h as i32, if i == menu.sel { TILE_SEL } else { TILE });
        text(&rect, &m.name.to_ascii_uppercase(), r, px, [0.95, 0.95, 0.95]);
    }
    unsafe { gl::Disable(gl::SCISSOR_TEST) };
    draw_gl_settings(menu, w, h);
}

/// The SETTINGS row and, when open, the settings panel (scissor clears + 5x7 font).
#[cfg(not(target_os = "ios"))]
fn draw_gl_settings(menu: &Menu, w: i32, h: i32) {
    let rect = |x: i32, y_top: i32, rw: i32, rh: i32, c: [f32; 3]| unsafe {
        gl::Scissor(x, h - y_top - rh, rw.max(0), rh.max(0));
        gl::ClearColor(c[0], c[1], c[2], 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
    };
    let fill = |r: Rect, c: [f32; 3]| rect(r.x as i32, r.y as i32, r.w as i32, r.h as i32, c);
    let (fw, fh) = (w as f32, h as f32);
    let fit = |r: Rect, chars: usize| ((r.h * 0.6) as i32 / 7).min((r.w * 0.9) as i32 / (chars.max(1) as i32 * 6)).max(1);
    unsafe { gl::Enable(gl::SCISSOR_TEST) };
    let row = menu.settings_row_rect(fw, fh);
    let selected = menu.sel == menu.maps.len();
    fill(row, if selected || menu.in_settings { TILE_SEL } else { TILE });
    text(&rect, "SETTINGS", row, fit(row, 12), [0.95, 0.95, 0.95]);
    if menu.in_settings {
        fill(menu.settings_panel_rect(fw, fh), PANEL_BG);
        let t = menu.settings_title_rect(fw, fh);
        text(&rect, "SETTINGS", t, fit(t, 12), [0.95, 0.95, 0.95]);
        for (i, o) in OPTIONS.iter().enumerate() {
            let r = menu.settings_opt_rect(i, fw, fh);
            fill(r, if i == menu.opt { TILE_SEL } else { TILE });
            let px = fit(r, 32);
            let pad = Rect { x: r.x + r.w * 0.03, w: r.w * 0.94, ..r };
            text_aligned(&rect, o.label, pad, px, [0.95, 0.95, 0.95], -1);
            text_aligned(&rect, &menu.settings.text(i), pad, px, [1.0, 0.75, 0.3], 1);
        }
        let hint = menu.settings_hint_rect(fw, fh);
        text(&rect, Menu::SETTINGS_HINT, hint, fit(hint, 40), [0.6, 0.6, 0.6]);
    }
    unsafe { gl::Disable(gl::SCISSOR_TEST) };
}

/// Centre `s` in the box with `px`-sized font pixels.
#[cfg(not(target_os = "ios"))]
pub fn text(rect: &impl Fn(i32, i32, i32, i32, [f32; 3]), s: &str, b: Rect, px: i32, c: [f32; 3]) {
    text_aligned(rect, s, b, px, c, 0)
}

/// `s` in the box, vertically centred; `align` -1 left, 0 centre, 1 right.
#[cfg(not(target_os = "ios"))]
pub fn text_aligned(rect: &impl Fn(i32, i32, i32, i32, [f32; 3]), s: &str, b: Rect, px: i32, c: [f32; 3], align: i32) {
    let n = s.chars().count().max(1) as i32;
    let tw = n * 6 * px - px;
    let ox = match align {
        a if a < 0 => b.x as i32,
        0 => b.x as i32 + (b.w as i32 - tw) / 2,
        _ => (b.x + b.w) as i32 - tw,
    };
    let oy = b.y as i32 + (b.h as i32 - 7 * px) / 2;
    for (k, ch) in s.chars().enumerate() {
        for (row, bits) in glyph(ch).iter().enumerate() {
            for col in 0..5 {
                if bits & (0x10 >> col) != 0 {
                    rect(ox + (k as i32 * 6 + col) * px, oy + row as i32 * px, px, px, c);
                }
            }
        }
    }
}

/// 5x7 glyphs, one byte per row, bit 4 = leftmost column.
#[cfg(not(target_os = "ios"))]
fn glyph(c: char) -> [u8; 7] {
    match c {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '/' => [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x00],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '\'' => [0x04, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00],
        _ => [0; 7],
    }
}

// ---------------------------------------------------------------------------------------
// iOS rendering: UIKit views over the game view, removed once a map is chosen. Art mode
// uses UIImageViews (UIImage decodes WebP on iOS 14+); plain mode uses labels.

#[cfg(target_os = "ios")]
pub struct Overlay {
    root: objc2::rc::Retained<objc2_ui_kit::UIView>,
    parent: objc2::rc::Retained<objc2_ui_kit::UIView>,
    /// Art mode: (row view, normal image, highlighted image) per map, and the panel.
    rows: Vec<(objc2::rc::Retained<objc2_ui_kit::UIImageView>, Option<objc2::rc::Retained<objc2_ui_kit::UIImage>>, Option<objc2::rc::Retained<objc2_ui_kit::UIImage>>)>,
    panel: Option<objc2::rc::Retained<objc2_ui_kit::UIImageView>>,
    panels: Vec<Option<objc2::rc::Retained<objc2_ui_kit::UIImage>>>,
    background: Option<objc2::rc::Retained<objc2_ui_kit::UIImageView>>,
    /// Plain mode.
    tiles: Vec<objc2::rc::Retained<objc2_ui_kit::UILabel>>,
    title: Option<objc2::rc::Retained<objc2_ui_kit::UILabel>>,
    /// The SETTINGS row and the settings panel: (row background, name, value) per option.
    settings_row: objc2::rc::Retained<objc2_ui_kit::UILabel>,
    settings_panel: objc2::rc::Retained<objc2_ui_kit::UIView>,
    settings_title: objc2::rc::Retained<objc2_ui_kit::UILabel>,
    settings_opts: Vec<(objc2::rc::Retained<objc2_ui_kit::UIView>, objc2::rc::Retained<objc2_ui_kit::UILabel>, objc2::rc::Retained<objc2_ui_kit::UILabel>)>,
    settings_hint: objc2::rc::Retained<objc2_ui_kit::UILabel>,
}

#[cfg(target_os = "ios")]
fn ui_color(c: [f32; 3]) -> objc2::rc::Retained<objc2_ui_kit::UIColor> {
    objc2_ui_kit::UIColor::colorWithRed_green_blue_alpha(c[0] as f64, c[1] as f64, c[2] as f64, 1.0)
}

/// A label with the menu's bold white text; `align` 0 left, 1 centre, 2 right.
#[cfg(target_os = "ios")]
fn ui_label(mtm: objc2::MainThreadMarker, s: &str, size: f64, align: isize) -> objc2::rc::Retained<objc2_ui_kit::UILabel> {
    use objc2::MainThreadOnly;
    use objc2_core_foundation::CGRect;
    use objc2_foundation::NSString;
    use objc2_ui_kit::{NSTextAlignment, UIColor, UIFont, UILabel};
    let l = UILabel::initWithFrame(UILabel::alloc(mtm), CGRect::default());
    l.setText(Some(&NSString::from_str(s)));
    unsafe {
        l.setTextColor(Some(&UIColor::whiteColor()));
        l.setFont(Some(&UIFont::boldSystemFontOfSize(size)));
    }
    l.setTextAlignment(NSTextAlignment(align));
    l.setAdjustsFontSizeToFitWidth(true);
    l
}

#[cfg(target_os = "ios")]
impl Overlay {
    pub fn new(parent: &objc2_ui_kit::UIView, menu: &Menu) -> Option<Self> {
        use objc2::{MainThreadOnly, Message};
        use objc2_core_foundation::CGRect;
        use objc2_foundation::NSString;
        use objc2_ui_kit::{NSTextAlignment, UIColor, UIFont, UIImage, UIImageView, UILabel, UIView};
        let mtm = objc2::MainThreadMarker::new()?;
        let root = UIView::initWithFrame(UIView::alloc(mtm), parent.bounds());
        let mut rows = vec![];
        let mut panels = vec![];
        let mut panel = None;
        let mut background = None;
        let mut tiles = vec![];
        let mut title = None;
        if menu.art.is_some() {
            root.setBackgroundColor(Some(&UIColor::blackColor()));
            let load = |p: Option<PathBuf>| p.and_then(|p| UIImage::imageWithContentsOfFile(&NSString::from_str(&p.to_string_lossy())));
            let view = |img: Option<&UIImage>| {
                let v = UIImageView::initWithImage(UIImageView::alloc(mtm), img);
                root.addSubview(&v);
                v
            };
            let bg = load(menu.art_file("supported", None));
            background = Some(view(bg.as_deref()));
            for m in &menu.maps {
                let row = load(menu.art_file("row", Some(m.slug)));
                let hl = load(menu.art_file("hl", Some(m.slug)));
                let v = view(row.as_deref());
                rows.push((v, row, hl));
                panels.push(load(menu.art_file("panel", Some(m.slug))));
            }
            panel = Some(view(None));
        } else {
            let color = |c: [f32; 3]| UIColor::colorWithRed_green_blue_alpha(c[0] as f64, c[1] as f64, c[2] as f64, 1.0);
            root.setBackgroundColor(Some(&color(BG)));
            let label = |s: &str, size: f64| {
                let l = UILabel::initWithFrame(UILabel::alloc(mtm), CGRect::default());
                l.setText(Some(&NSString::from_str(s)));
                unsafe {
                    l.setTextColor(Some(&UIColor::whiteColor()));
                    l.setFont(Some(&UIFont::boldSystemFontOfSize(size)));
                }
                l.setTextAlignment(NSTextAlignment(1)); // center
                l.setAdjustsFontSizeToFitWidth(true);
                root.addSubview(&l);
                l
            };
            title = Some(label("Choose a map", 34.0));
            tiles = menu.maps.iter().map(|m| label(m.name, 26.0)).collect();
        }
        let settings_row = ui_label(mtm, "SETTINGS", 20.0, 1);
        root.addSubview(&settings_row);
        let settings_panel = UIView::initWithFrame(UIView::alloc(mtm), CGRect::default());
        settings_panel.setBackgroundColor(Some(&ui_color(PANEL_BG)));
        root.addSubview(&settings_panel);
        let settings_title = ui_label(mtm, "SETTINGS", 30.0, 1);
        root.addSubview(&settings_title);
        let settings_opts = OPTIONS
            .iter()
            .map(|o| {
                let bg = UIView::initWithFrame(UIView::alloc(mtm), CGRect::default());
                root.addSubview(&bg);
                let name = ui_label(mtm, o.label, 20.0, 0);
                let value = ui_label(mtm, "", 20.0, 2);
                unsafe { value.setTextColor(Some(&ui_color([1.0, 0.75, 0.3]))) };
                root.addSubview(&name);
                root.addSubview(&value);
                (bg, name, value)
            })
            .collect();
        let settings_hint = ui_label(mtm, "TAP AN OPTION TO CHANGE IT  -  CONTROLLER: LEFT / RIGHT, B TO GO BACK", 14.0, 1);
        unsafe { settings_hint.setTextColor(Some(&ui_color([0.6, 0.6, 0.6]))) };
        root.addSubview(&settings_hint);
        let o = Self {
            root,
            parent: parent.retain(),
            rows,
            panel,
            panels,
            background,
            tiles,
            title,
            settings_row,
            settings_panel,
            settings_title,
            settings_opts,
            settings_hint,
        };
        parent.addSubview(&o.root);
        o.layout(menu);
        Some(o)
    }

    fn layout(&self, menu: &Menu) {
        use objc2_core_foundation::{CGPoint, CGRect, CGSize};
        let b = self.parent.bounds();
        let (w, h) = (b.size.width as f32, b.size.height as f32);
        self.root.setFrame(b);
        let frame = |r: Rect| CGRect::new(CGPoint::new(r.x as f64, r.y as f64), CGSize::new(r.w as f64, r.h as f64));
        if let Some(bg) = &self.background {
            bg.setFrame(frame(Menu::stage(w, h)));
        }
        for (i, (v, _, _)) in self.rows.iter().enumerate() {
            v.setFrame(frame(menu.row_rect(i, w, h)));
        }
        if let Some(p) = &self.panel {
            p.setFrame(frame(menu.panel_rect(w, h)));
        }
        if let Some(t) = &self.title {
            t.setFrame(frame(menu.title_rect(w, h)));
        }
        for (i, t) in self.tiles.iter().enumerate() {
            t.setFrame(frame(menu.item_rect(i, w, h)));
        }
        self.settings_row.setFrame(frame(menu.settings_row_rect(w, h)));
        self.settings_panel.setFrame(frame(menu.settings_panel_rect(w, h)));
        self.settings_title.setFrame(frame(menu.settings_title_rect(w, h)));
        for (i, (bg, name, value)) in self.settings_opts.iter().enumerate() {
            let r = menu.settings_opt_rect(i, w, h);
            bg.setFrame(frame(r));
            let inner = Rect { x: r.x + r.w * 0.03, w: r.w * 0.94, ..r };
            name.setFrame(frame(Rect { w: inner.w * 0.55, ..inner }));
            value.setFrame(frame(Rect { x: inner.x + inner.w * 0.45, w: inner.w * 0.55, ..inner }));
        }
        self.settings_hint.setFrame(frame(menu.settings_hint_rect(w, h)));
        self.update(menu);
    }

    pub fn update(&self, menu: &Menu) {
        use objc2_ui_kit::UIColor;
        for (i, (v, row, hl)) in self.rows.iter().enumerate() {
            let img = if i == menu.sel { hl } else { row };
            v.setImage(img.as_deref());
        }
        if let Some(p) = &self.panel {
            p.setImage(self.panels.get(menu.sel).and_then(|x| x.as_deref()));
        }
        for (i, t) in self.tiles.iter().enumerate() {
            let c = if i == menu.sel { TILE_SEL } else { TILE };
            t.setBackgroundColor(Some(&UIColor::colorWithRed_green_blue_alpha(c[0] as f64, c[1] as f64, c[2] as f64, 1.0)));
        }
        let row_on = menu.sel == menu.maps.len() || menu.in_settings;
        self.settings_row.setBackgroundColor(Some(&ui_color(if row_on { TILE_SEL } else { TILE })));
        let open = menu.in_settings;
        if let Some(p) = &self.panel {
            p.setHidden(open);
        }
        self.settings_panel.setHidden(!open);
        self.settings_title.setHidden(!open);
        self.settings_hint.setHidden(!open);
        for (i, (bg, name, value)) in self.settings_opts.iter().enumerate() {
            bg.setHidden(!open);
            name.setHidden(!open);
            value.setHidden(!open);
            bg.setBackgroundColor(Some(&ui_color(if i == menu.opt { TILE_SEL } else { TILE })));
            value.setText(Some(&objc2_foundation::NSString::from_str(&menu.settings.text(i))));
        }
    }

    /// Point size of the parent view (taps arrive in physical pixels).
    pub fn size(&self) -> (f64, f64) {
        let b = self.parent.bounds();
        (b.size.width, b.size.height)
    }

    pub fn remove(self) {
        self.root.removeFromSuperview();
    }
}
