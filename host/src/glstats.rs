//! Debug (BO1Z_GLSTATS=1): GL usage counts logged every few seconds, for comparing GL
//! backends (ANGLE D3D11 on PC vs Metal on iPad): draws by depth state, feedback loops
//! (a draw sampling a texture attached to the framebuffer it renders into), program
//! queries, and GL errors per frame. Also keeps ANGLE's validation on (no no-error context).

use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Instant;

use parking_lot::Mutex;

use crate::log;

pub fn on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("BO1Z_GLSTATS").is_some())
}

#[derive(Default)]
struct Stats {
    /// Native texture -> (width, height, internal format).
    tex: HashMap<u32, (i32, i32, u32)>,
    /// Native framebuffer -> attachment -> native texture.
    fbo: HashMap<u32, HashMap<u32, u32>>,
    /// TEXTURE_2D binding per unit.
    units: [u32; 32],
    /// Native program -> sampler units it reads.
    samplers: HashMap<u32, Vec<u32>>,
    counts: HashMap<String, u32>,
    last: Option<Instant>,
}

static S: Mutex<Option<Stats>> = Mutex::new(None);

fn get_i(p: u32) -> u32 {
    let mut v = 0i32;
    unsafe { gl::GetIntegerv(p, &mut v) };
    v as u32
}

fn with<R>(f: impl FnOnce(&mut Stats) -> R) -> R {
    f(S.lock().get_or_insert_with(Default::default))
}

fn bump(s: &mut Stats, key: String) {
    *s.counts.entry(key).or_default() += 1;
    let now = Instant::now();
    let last = *s.last.get_or_insert(now);
    if now.duration_since(last).as_secs_f32() > 3.0 {
        let mut v: Vec<_> = s.counts.drain().collect();
        v.sort();
        log::info(&format!("glstats: {}", v.iter().map(|(k, n)| format!("{k}={n}")).collect::<Vec<_>>().join(" ")));
        s.last = Some(now);
    }
}

pub fn tex_storage(target: u32, fmt: u32, w: i32, h: i32) {
    if target == gl::TEXTURE_2D {
        let tex = get_i(gl::TEXTURE_BINDING_2D);
        with(|s| s.tex.insert(tex, (w, h, fmt)));
    }
}

pub fn attach(target: u32, att: u32, tex_native: u32) {
    let fbo = get_i(if target == gl::READ_FRAMEBUFFER { gl::READ_FRAMEBUFFER_BINDING } else { gl::DRAW_FRAMEBUFFER_BINDING });
    with(|s| s.fbo.entry(fbo).or_default().insert(att, tex_native));
}

pub fn bind(unit: u32, tex_native: u32) {
    with(|s| {
        if let Some(u) = s.units.get_mut(unit as usize) {
            *u = tex_native;
        }
    });
}

/// Sampler uniforms changed: forget the cached units.
pub fn uniforms_changed(program: u32) {
    with(|s| s.samplers.remove(&program));
}

unsafe fn sampler_units(program: u32) -> Vec<u32> {
    let mut n = 0;
    gl::GetProgramiv(program, gl::ACTIVE_UNIFORMS, &mut n);
    let mut units = Vec::new();
    for i in 0..n as u32 {
        let mut name = [0u8; 128];
        let (mut len, mut size, mut ty) = (0, 0, 0);
        gl::GetActiveUniform(program, i, 128, &mut len, &mut size, &mut ty, name.as_mut_ptr() as *mut _);
        if ty != gl::SAMPLER_2D {
            continue;
        }
        let loc = gl::GetUniformLocation(program, name.as_ptr() as *const _);
        if loc >= 0 {
            let mut unit = 0;
            gl::GetUniformiv(program, loc, &mut unit);
            units.push(unit as u32);
        }
    }
    units
}

pub fn draw() {
    let program = get_i(gl::CURRENT_PROGRAM);
    let fbo = get_i(gl::DRAW_FRAMEBUFFER_BINDING);
    let depth_write = {
        let mut m = 0u8;
        unsafe { gl::GetBooleanv(gl::DEPTH_WRITEMASK, &mut m) };
        m != 0
    };
    let cached = with(|s| s.samplers.contains_key(&program));
    if !cached {
        let units = unsafe { sampler_units(program) };
        with(|s| s.samplers.insert(program, units));
    }
    with(|s| {
        let mut hits = Vec::new();
        if let Some(atts) = s.fbo.get(&fbo) {
            for &unit in &s.samplers[&program] {
                let tex = s.units.get(unit as usize).copied().unwrap_or(0);
                if tex == 0 {
                    continue;
                }
                if let Some((&att, _)) = atts.iter().find(|(_, &t)| t == tex) {
                    let (w, h, fmt) = s.tex.get(&tex).copied().unwrap_or_default();
                    hits.push(format!("feedback(unit{unit},{w}x{h},fmt=0x{fmt:x},att=0x{att:x},zwrite={depth_write})"));
                }
            }
        }
        let (func, test) = unsafe { (get_i(gl::DEPTH_FUNC), gl::IsEnabled(gl::DEPTH_TEST) != 0) };
        let mut cm = [0u8; 4];
        unsafe { gl::GetBooleanv(gl::COLOR_WRITEMASK, cm.as_mut_ptr()) };
        bump(s, format!("draw(fbo{fbo},ztest={test},func=0x{func:x},zwrite={depth_write},color={})", cm.iter().any(|&c| c != 0)));
        for h in hits {
            bump(s, h);
        }
    });
}

/// glGetProgramiv: which queries the engine makes and how long they block (a link status
/// query waits for the program to finish compiling).
pub fn program_query(pname: u32, value: i32, took: std::time::Duration) {
    with(|s| {
        let ms = took.as_secs_f64() * 1000.0;
        let bucket = if ms < 1.0 { "<1ms" } else if ms < 10.0 { "<10ms" } else if ms < 50.0 { "<50ms" } else { ">=50ms" };
        bump(s, format!("getprogram(0x{pname:x}={value},{bucket})"));
    });
}

/// Once per frame: GL errors raised since the last frame, counted by code.
pub fn frame_errors() {
    loop {
        let e = unsafe { gl::GetError() };
        if e == gl::NO_ERROR {
            break;
        }
        with(|s| bump(s, format!("glerror(0x{e:x})")));
    }
}
