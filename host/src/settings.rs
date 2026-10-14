//! Player settings, chosen on the map select screen and applied when a map starts: engine
//! dvars (the in-game video options are greyed out because this engine build cannot restart
//! its renderer; set before boot they need no restart) and host behaviour (render scale, the
//! frame counter). Saved as <data>/settings.json.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::log;

pub struct Opt {
    pub key: &'static str,
    pub label: &'static str,
    /// (shown text, value)
    pub choices: &'static [(&'static str, f32)],
    pub default: usize,
}

pub const FRAME_COUNTER: usize = 0;
pub const RESOLUTION: usize = 1;
pub const FRAME_LIMIT: usize = 2;
pub const RENDER_THREAD: usize = 3;

pub const OPTIONS: &[Opt] = &[
    Opt { key: "frame_counter", label: "FRAME COUNTER", choices: &[("OFF", 0.0), ("ON", 1.0)], default: 0 },
    // Multiples of the screen's point size: 1x is half the iPad's native pixels per side.
    Opt {
        key: "resolution",
        label: "RESOLUTION",
        choices: &[("75%", 0.75), ("100%", 1.0), ("125%", 1.25), ("150%", 1.5), ("175%", 1.75), ("200%", 2.0)],
        default: 1,
    },
    Opt { key: "frame_limit", label: "FRAME RATE LIMIT", choices: &[("30", 30.0), ("60", 60.0), ("UNLIMITED", 0.0)], default: 1 },
    // The site's route on every browser but Firefox: the renderer on its own thread.
    // Off by default: on PC it paces frames less evenly (56 vs 62 fps, more hitches).
    Opt { key: "render_thread", label: "MULTITHREADED RENDERING", choices: &[("OFF", 0.0), ("ON", 1.0)], default: 0 },
];

pub struct Settings {
    path: PathBuf,
    pub sel: Vec<usize>,
}

impl Settings {
    pub fn load(data: &Path) -> Self {
        let path = data.join("settings.json");
        let saved: serde_json::Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let sel = OPTIONS
            .iter()
            .map(|o| {
                saved[o.key]
                    .as_str()
                    .and_then(|v| o.choices.iter().position(|c| c.0 == v))
                    .unwrap_or(o.default)
            })
            .collect();
        Self { path, sel }
    }

    pub fn save(&self) {
        let v: serde_json::Map<String, serde_json::Value> =
            OPTIONS.iter().zip(&self.sel).map(|(o, &i)| (o.key.to_string(), o.choices[i].0.into())).collect();
        if let Err(e) = std::fs::write(&self.path, serde_json::to_vec_pretty(&v).unwrap_or_default()) {
            log::error(&format!("cannot save {}: {e}", self.path.display()));
        }
    }

    pub fn value(&self, opt: usize) -> f32 {
        OPTIONS[opt].choices[self.sel[opt]].1
    }

    pub fn on(&self, opt: usize) -> bool {
        self.value(opt) != 0.0
    }

    /// Step option `opt` by `d` (wrapping).
    pub fn step(&mut self, opt: usize, d: i32) {
        let n = OPTIONS[opt].choices.len() as i32;
        self.sel[opt] = (self.sel[opt] as i32 + d).rem_euclid(n) as usize;
        self.save();
    }

    /// Text for option `opt`'s current value; the resolution shows the pixel size it gives.
    pub fn text(&self, opt: usize) -> String {
        let (name, v) = OPTIONS[opt].choices[self.sel[opt]];
        let (w, h) = (SCREEN_W.load(Ordering::Relaxed), SCREEN_H.load(Ordering::Relaxed));
        if opt == RESOLUTION && w > 0 {
            format!("{} X {}  ({name})", (w as f32 * v).round(), (h as f32 * v).round())
        } else {
            name.to_string()
        }
    }

    /// Engine arguments for these settings.
    pub fn engine_args(&self) -> Vec<String> {
        let mut a = vec![];
        let mut set = |k: &str, v: String| a.extend(["+set".to_string(), k.to_string(), v]);
        set("com_maxfps", format!("{}", self.value(FRAME_LIMIT) as u32));
        set("r_smp_backend", if self.on(RENDER_THREAD) { "1" } else { "0" }.into());
        a
    }

    pub fn log(&self) {
        let s: Vec<String> = OPTIONS.iter().enumerate().map(|(i, o)| format!("{}={}", o.key, self.text(i))).collect();
        log::info(&format!("settings: {}", s.join(", ")));
    }
}

/// The screen size in points (iOS) or the window size (desktop), for the resolution labels.
static SCREEN_W: AtomicU32 = AtomicU32::new(0);
static SCREEN_H: AtomicU32 = AtomicU32::new(0);

pub fn set_screen(w: u32, h: u32) {
    SCREEN_W.store(w, Ordering::Relaxed);
    SCREEN_H.store(h, Ordering::Relaxed);
}
