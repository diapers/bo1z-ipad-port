//! EGL (ANGLE) context: created on the OS main thread against the native window,
//! made current on whichever engine thread owns the canvas (the main pthread).

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use khronos_egl as egl;

use crate::log;

type Egl = egl::DynamicInstance<egl::EGL1_4>;

struct Ctx {
    egl: Egl,
    display: egl::Display,
    config: egl::Config,
    surface: egl::Surface,
    context: egl::Context,
}
// EGL handles are opaque pointers that EGL itself lets any thread use.
unsafe impl Send for Ctx {}
unsafe impl Sync for Ctx {}

static CTX: OnceLock<Ctx> = OnceLock::new();
static LOADED: AtomicBool = AtomicBool::new(false);
static REQ_W: AtomicI32 = AtomicI32::new(0);
static REQ_H: AtomicI32 = AtomicI32::new(0);

/// Load ANGLE's libEGL from `lib_dir` (or the default search path) and create a GLES 3.0
/// context and window surface. `window` is an HWND on Windows, a CAMetalLayer* on iOS.
pub fn create(lib: &Path, window: *mut std::ffi::c_void) -> Result<()> {
    let egl = unsafe { Egl::load_required_from_filename(lib) }
        .map_err(|e| anyhow!("cannot load {}: {e}", lib.display()))?;
    let display = unsafe { egl.get_display(egl::DEFAULT_DISPLAY) }.ok_or_else(|| anyhow!("eglGetDisplay failed"))?;
    let (major, minor) = egl.initialize(display)?;
    log::info(&format!(
        "EGL {major}.{minor} vendor={} version={}",
        egl.query_string(Some(display), egl::VENDOR).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
        egl.query_string(Some(display), egl::VERSION).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    ));
    egl.bind_api(egl::OPENGL_ES_API)?;
    const OPENGL_ES3_BIT: egl::Int = 0x40;
    let attrs = [
        egl::RED_SIZE, 8,
        egl::GREEN_SIZE, 8,
        egl::BLUE_SIZE, 8,
        egl::ALPHA_SIZE, 8,
        egl::DEPTH_SIZE, 24,
        egl::STENCIL_SIZE, 8,
        egl::RENDERABLE_TYPE, OPENGL_ES3_BIT,
        egl::SURFACE_TYPE, egl::WINDOW_BIT,
        egl::NONE,
    ];
    let config = egl.choose_first_config(display, &attrs)?.ok_or_else(|| anyhow!("no RGBA8/D24S8 ES3 EGL config"))?;
    let surface = unsafe { egl.create_window_surface(display, config, window as egl::NativeWindowType, None)? };
    // WebGL zero-fills every new texture/buffer/renderbuffer; native GLES leaves them
    // undefined. The engine relies on the WebGL behaviour (it allocates full mip chains
    // and streams levels in later), and on Metal the undefined memory is garbage that
    // samples as blocky BC-decoded noise. ANGLE's robust resource init restores WebGL's rule.
    const ROBUST_RESOURCE_INITIALIZATION_ANGLE: egl::Int = 0x3453;
    const CONTEXT_OPENGL_NO_ERROR_KHR: egl::Int = 0x31B3;
    let egl_exts = egl.query_string(Some(display), egl::EXTENSIONS).map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let has_ext = |name: &str| egl_exts.split(' ').any(|e| e == name);
    let robust_init = has_ext("EGL_ANGLE_robust_resource_initialization");
    // The engine makes no GL errors (checked per frame with BO1Z_GLSTATS across loading and
    // play), so ANGLE's per-call validation is pure CPU cost: skip it, except when debugging.
    let no_error = has_ext("EGL_KHR_create_context_no_error") && !crate::glstats::on();
    let mut ctx_attrs = vec![egl::CONTEXT_MAJOR_VERSION, 3, egl::CONTEXT_MINOR_VERSION, 0];
    if robust_init {
        ctx_attrs.extend([ROBUST_RESOURCE_INITIALIZATION_ANGLE, egl::TRUE as egl::Int]);
    }
    if no_error {
        ctx_attrs.extend([CONTEXT_OPENGL_NO_ERROR_KHR, egl::TRUE as egl::Int]);
    }
    ctx_attrs.push(egl::NONE);
    let context = egl.create_context(display, config, None, &ctx_attrs)?;
    log::info(&format!("robust resource initialization: {robust_init}, no-error context: {no_error}"));
    // Paint the window black right away: the engine draws nothing until its core
    // fastfiles are loaded, and an untouched surface shows as white.
    egl.make_current(display, Some(surface), Some(surface), Some(context))?;
    if let Some(clear_color) = egl.get_proc_address("glClearColor") {
        let clear = egl.get_proc_address("glClear");
        unsafe {
            let cc: extern "system" fn(f32, f32, f32, f32) = std::mem::transmute(clear_color);
            cc(0.0, 0.0, 0.0, 1.0);
            if let Some(c) = clear {
                let c: extern "system" fn(u32) = std::mem::transmute(c);
                c(0x4000);
            }
        }
    }
    let _ = egl.swap_buffers(display, surface);
    egl.make_current(display, None, None, None)?;
    CTX.set(Ctx { egl, display, config, surface, context }).map_err(|_| anyhow!("EGL context created twice"))?;
    Ok(())
}

fn ctx() -> Result<&'static Ctx> {
    CTX.get().ok_or_else(|| anyhow!("no EGL context"))
}

/// Make the context current on the calling thread. Returns true the first time GL
/// function pointers were loaded.
pub fn make_current() -> Result<bool> {
    let c = ctx()?;
    c.egl.make_current(c.display, Some(c.surface), Some(c.surface), Some(c.context))?;
    // No vsync wait: a frame that misses a vblank by a little would otherwise wait a whole
    // extra refresh (60 -> 30 fps judder). The engine paces itself with com_maxfps.
    c.egl.swap_interval(c.display, 0)?;
    if LOADED.swap(true, Ordering::AcqRel) {
        return Ok(false);
    }
    gl::load_with(|name| c.egl.get_proc_address(name).map_or(std::ptr::null(), |f| f as *const _));
    unsafe {
        let s = |e| {
            let p = gl::GetString(e);
            if p.is_null() { String::new() } else { std::ffi::CStr::from_ptr(p as *const _).to_string_lossy().into_owned() }
        };
        log::info(&format!("GL_RENDERER={} GL_VERSION={}", s(gl::RENDERER), s(gl::VERSION)));
    }
    Ok(true)
}

/// A second window surface with its own (unshared) context, driven from the UI thread
/// while the engine thread owns the main one (desktop loading video).
pub struct Aux {
    surface: egl::Surface,
    context: egl::Context,
}

impl Aux {
    pub fn new(window: *mut std::ffi::c_void) -> Result<Self> {
        let c = ctx()?;
        let surface = unsafe { c.egl.create_window_surface(c.display, c.config, window as egl::NativeWindowType, None)? };
        let attrs = [egl::CONTEXT_MAJOR_VERSION, 3, egl::CONTEXT_MINOR_VERSION, 0, egl::NONE];
        let context = c.egl.create_context(c.display, c.config, None, &attrs)?;
        Ok(Self { surface, context })
    }

    pub fn make_current(&self) -> Result<()> {
        let c = ctx()?;
        c.egl.make_current(c.display, Some(self.surface), Some(self.surface), Some(self.context))?;
        let _ = c.egl.swap_interval(c.display, 0);
        Ok(())
    }

    pub fn size(&self) -> (i32, i32) {
        let Ok(c) = ctx() else { return (0, 0) };
        (
            c.egl.query_surface(c.display, self.surface, egl::WIDTH).unwrap_or(0),
            c.egl.query_surface(c.display, self.surface, egl::HEIGHT).unwrap_or(0),
        )
    }

    pub fn swap(&self) {
        if let Ok(c) = ctx() {
            let _ = c.egl.swap_buffers(c.display, self.surface);
        }
    }
}

impl Drop for Aux {
    fn drop(&mut self) {
        if let Ok(c) = ctx() {
            let _ = c.egl.make_current(c.display, None, None, None);
            let _ = c.egl.destroy_context(c.display, self.context);
            let _ = c.egl.destroy_surface(c.display, self.surface);
        }
    }
}

pub fn release_current() {
    if let Ok(c) = ctx() {
        let _ = c.egl.make_current(c.display, None, None, None);
    }
}

pub fn swap() {
    if let Ok(c) = ctx() {
        if let Err(e) = c.egl.swap_buffers(c.display, c.surface) {
            log::error(&format!("eglSwapBuffers: {e}"));
        }
    }
}

pub fn surface_size() -> (i32, i32) {
    let Ok(c) = ctx() else { return (0, 0) };
    let w = c.egl.query_surface(c.display, c.surface, egl::WIDTH).unwrap_or(0);
    let h = c.egl.query_surface(c.display, c.surface, egl::HEIGHT).unwrap_or(0);
    (w, h)
}

/// emscripten_set_canvas_element_size: the drawing buffer is the window surface, so
/// only remember what the engine asked for.
pub fn set_requested_size(w: i32, h: i32) {
    REQ_W.store(w, Ordering::Relaxed);
    REQ_H.store(h, Ordering::Relaxed);
}

static FRAME: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

static SHOT_REQUEST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Debug: save the next presented frame (BO1Z_FAKEPAD "<t>:shot").
pub fn request_shot() {
    SHOT_REQUEST.store(true, Ordering::Relaxed);
}

static SHOT_T: std::sync::Mutex<Option<(std::time::Instant, Vec<f32>)>> = std::sync::Mutex::new(None);

/// Debug: BO1Z_SHOT="600,1200" saves those presented frames as BMP into the data dir;
/// BO1Z_SHOT_AT="45,60.5" does the same at those seconds after the first frame.
pub fn before_present(data_dir: &Path) {
    let n = FRAME.fetch_add(1, Ordering::Relaxed) + 1;
    let by_frame = std::env::var("BO1Z_SHOT").is_ok_and(|spec| spec.split(',').any(|s| s.trim().parse::<u64>().ok() == Some(n)));
    let by_time = {
        let mut g = SHOT_T.lock().unwrap();
        let (t0, due) = g.get_or_insert_with(|| {
            let list = std::env::var("BO1Z_SHOT_AT").unwrap_or_default();
            (std::time::Instant::now(), list.split(',').filter_map(|s| s.trim().parse().ok()).collect())
        });
        let now = t0.elapsed().as_secs_f32();
        let before = due.len();
        due.retain(|&t| t > now);
        due.len() != before
    };
    let by_request = SHOT_REQUEST.swap(false, Ordering::Relaxed);
    if !by_frame && !by_time && !by_request {
        return;
    }
    let (w, h) = surface_size();
    if w <= 0 || h <= 0 {
        return;
    }
    let mut px = vec![0u8; (w * h * 4) as usize];
    unsafe {
        let mut prev = 0i32;
        gl::GetIntegerv(gl::READ_FRAMEBUFFER_BINDING, &mut prev);
        gl::BindFramebuffer(gl::READ_FRAMEBUFFER, 0);
        gl::PixelStorei(gl::PACK_ALIGNMENT, 1);
        gl::ReadPixels(0, 0, w, h, gl::RGBA, gl::UNSIGNED_BYTE, px.as_mut_ptr() as *mut _);
        gl::BindFramebuffer(gl::READ_FRAMEBUFFER, prev as u32);
    }
    // 32-bit BMP, bottom-up rows match GL's origin.
    let mut bmp = Vec::with_capacity(54 + px.len());
    let size = 54 + px.len() as u32;
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&size.to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&w.to_le_bytes());
    bmp.extend_from_slice(&h.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&32u16.to_le_bytes());
    bmp.extend_from_slice(&[0u8; 24]);
    for p in px.chunks_exact(4) {
        bmp.extend_from_slice(&[p[2], p[1], p[0], 255]);
    }
    let path = data_dir.join(format!("shot_{n}.bmp"));
    match std::fs::write(&path, bmp) {
        Ok(()) => log::info(&format!("screenshot {}", path.display())),
        Err(e) => log::error(&format!("screenshot: {e}")),
    }
}
