//! WebGL2 imports and the engine's packet interpreter (ExecuteGLPackets), on native
//! OpenGL ES 3 (ANGLE).
//!
//! Emscripten semantics kept exactly:
//!  * one object-id counter shared by every GL object type (GL.counter / getNewId);
//!  * virtual uniform locations per program (webglPrepareUniformLocationsBeforeFirstUse);
//!  * glGet special cases and object-valued queries returning emscripten ids.

use std::collections::HashMap;
use std::ffi::CString;

use wasmtime::Result;
use parking_lot::Mutex;
use wasmtime::*;

use crate::rt::Ctx;
use crate::{gl_ctx, log, mem};

type Cx<'a> = Caller<'a, Ctx>;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Kind {
    Buffer,
    Program,
    Framebuffer,
    Renderbuffer,
    Texture,
    Shader,
    Vao,
    Query,
    Sampler,
}

#[derive(Clone, Copy)]
enum Loc {
    /// Array element index not yet resolved to a native location.
    Index(u32),
    Native(i32),
}

#[derive(Default)]
struct ProgramInfo {
    prepared: bool,
    id_counter: u32,
    locs_by_id: HashMap<u32, Loc>,
    array_names_by_id: HashMap<u32, String>,
    size_and_ids_by_name: HashMap<String, (u32, u32)>,
}

struct State {
    counter: u32,
    objs: HashMap<(Kind, u32), u32>,
    rev: HashMap<(Kind, u32), u32>,
    programs: HashMap<u32, ProgramInfo>,
    current_program: u32,
    last_error: u32,
    extensions: Option<Vec<String>>,
}

static ST: Mutex<Option<State>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = ST.lock();
    let s = g.get_or_insert_with(|| State {
        counter: 1,
        objs: HashMap::new(),
        rev: HashMap::new(),
        programs: HashMap::new(),
        current_program: 0,
        last_error: 0,
        extensions: None,
    });
    f(s)
}

impl State {
    fn new_id(&mut self, kind: Kind, native: u32) -> u32 {
        let id = self.counter;
        self.counter += 1;
        self.objs.insert((kind, id), native);
        self.rev.insert((kind, native), id);
        id
    }
    fn n(&self, kind: Kind, id: u32) -> u32 {
        if id == 0 {
            0
        } else {
            self.objs.get(&(kind, id)).copied().unwrap_or(0)
        }
    }
    fn em(&self, kind: Kind, native: u32) -> u32 {
        if native == 0 {
            0
        } else {
            self.rev.get(&(kind, native)).copied().unwrap_or(0)
        }
    }
    fn forget(&mut self, kind: Kind, id: u32) -> Option<u32> {
        let native = self.objs.remove(&(kind, id))?;
        self.rev.remove(&(kind, native));
        Some(native)
    }
    fn record_error(&mut self, e: u32) {
        if self.last_error == 0 {
            self.last_error = e;
        }
    }
}

// ---------------------------------------------------------------------------------------
// Uniform locations

fn prepare_uniforms(program_native: u32, info: &mut ProgramInfo) {
    if info.prepared {
        return;
    }
    info.prepared = true;
    if info.id_counter == 0 {
        info.id_counter = 1;
    }
    let mut count = 0i32;
    unsafe { gl::GetProgramiv(program_native, gl::ACTIVE_UNIFORMS, &mut count) };
    let mut buf = vec![0u8; 512];
    for i in 0..count.max(0) as u32 {
        let (mut len, mut size, mut ty) = (0i32, 0i32, 0u32);
        unsafe {
            gl::GetActiveUniform(program_native, i, buf.len() as i32, &mut len, &mut size, &mut ty, buf.as_mut_ptr() as *mut _)
        };
        let name = String::from_utf8_lossy(&buf[..len.max(0) as usize]).into_owned();
        let array_name = match left_brace(&name) {
            Some(lb) if lb > 0 => name[..lb].to_string(),
            _ => name.clone(),
        };
        let sz = size.max(1) as u32;
        let mut id = info.id_counter;
        info.id_counter += sz;
        info.size_and_ids_by_name.insert(array_name.clone(), (sz, id));
        for j in 0..sz {
            info.locs_by_id.insert(id, Loc::Index(j));
            info.array_names_by_id.insert(id, array_name.clone());
            id += 1;
        }
    }
}

fn left_brace(name: &str) -> Option<usize> {
    if name.ends_with(']') {
        name.rfind('[')
    } else {
        None
    }
}

fn native_uniform_location(program_native: u32, name: &str) -> i32 {
    let c = CString::new(name).unwrap_or_default();
    unsafe { gl::GetUniformLocation(program_native, c.as_ptr()) }
}

/// webglGetUniformLocation(location) against the current program.
fn uniform_loc(s: &mut State, location: i32) -> i32 {
    let prog = s.current_program;
    let native_prog = s.n(Kind::Program, prog);
    if prog == 0 {
        s.record_error(gl::INVALID_OPERATION);
        return -1;
    }
    let Some(info) = s.programs.get_mut(&prog) else { return -1 };
    match info.locs_by_id.get(&(location as u32)).copied() {
        Some(Loc::Native(n)) => n,
        Some(Loc::Index(j)) => {
            let base = info.array_names_by_id.get(&(location as u32)).cloned().unwrap_or_default();
            let name = if j > 0 { format!("{base}[{j}]") } else { base };
            let n = native_uniform_location(native_prog, &name);
            info.locs_by_id.insert(location as u32, Loc::Native(n));
            n
        }
        None => -1,
    }
}

// ---------------------------------------------------------------------------------------
// Extensions

fn native_extensions(s: &mut State) -> &Vec<String> {
    s.extensions.get_or_insert_with(|| {
        let mut n = 0i32;
        let mut v = Vec::new();
        unsafe {
            gl::GetIntegerv(gl::NUM_EXTENSIONS, &mut n);
            for i in 0..n.max(0) as u32 {
                let p = gl::GetStringi(gl::EXTENSIONS, i);
                if !p.is_null() {
                    v.push(std::ffi::CStr::from_ptr(p as *const _).to_string_lossy().into_owned());
                }
            }
        }
        log::info(&format!("native GL extensions: {}", v.len()));
        v
    })
}

/// WebGL extension name -> available on this GLES implementation?
pub fn webgl_extension_supported(name: &str) -> bool {
    let name = name.strip_prefix("GL_").unwrap_or(name);
    with(|s| {
        let exts = native_extensions(s);
        let has = |e: &str| exts.iter().any(|x| x == e);
        match name {
            "WEBGL_multi_draw" | "WEBGL_lose_context" | "WEBGL_debug_renderer_info" | "WEBGL_debug_shaders" => true,
            "WEBGL_compressed_texture_s3tc" => {
                has("GL_EXT_texture_compression_s3tc")
                    || (has("GL_EXT_texture_compression_dxt1")
                        && has("GL_ANGLE_texture_compression_dxt3")
                        && has("GL_ANGLE_texture_compression_dxt5"))
            }
            "WEBGL_compressed_texture_s3tc_srgb" => has("GL_EXT_texture_compression_s3tc_srgb"),
            "EXT_disjoint_timer_query_webgl2" => has("GL_EXT_disjoint_timer_query"),
            "WEBGL_clip_cull_distance" => has("GL_EXT_clip_cull_distance") || has("GL_ANGLE_clip_cull_distance"),
            "WEBGL_polygon_mode" => has("GL_ANGLE_polygon_mode") || has("GL_NV_polygon_mode"),
            "WEBGL_blend_func_extended" => has("GL_EXT_blend_func_extended"),
            "WEBGL_draw_instanced_base_vertex_base_instance" => has("GL_ANGLE_base_vertex_base_instance"),
            "WEBGL_multi_draw_instanced_base_vertex_base_instance" => false,
            "WEBGL_compressed_texture_etc" => has("GL_ANGLE_compressed_texture_etc") || has("GL_OES_compressed_ETC2_RGB8_texture"),
            "WEBGL_compressed_texture_astc" => has("GL_KHR_texture_compression_astc_ldr"),
            other => has(&format!("GL_{other}")),
        }
    })
}

const WEBGL_EXT_LIST: &[&str] = &[
    "EXT_color_buffer_float",
    "EXT_conservative_depth",
    "EXT_disjoint_timer_query_webgl2",
    "EXT_texture_norm16",
    "NV_shader_noperspective_interpolation",
    "WEBGL_clip_cull_distance",
    "EXT_clip_control",
    "EXT_color_buffer_half_float",
    "EXT_depth_clamp",
    "EXT_float_blend",
    "EXT_polygon_offset_clamp",
    "EXT_texture_compression_bptc",
    "EXT_texture_compression_rgtc",
    "EXT_texture_filter_anisotropic",
    "KHR_parallel_shader_compile",
    "OES_texture_float_linear",
    "WEBGL_blend_func_extended",
    "WEBGL_compressed_texture_astc",
    "WEBGL_compressed_texture_etc",
    "WEBGL_compressed_texture_etc1",
    "WEBGL_compressed_texture_s3tc",
    "WEBGL_compressed_texture_s3tc_srgb",
    "WEBGL_debug_renderer_info",
    "WEBGL_debug_shaders",
    "WEBGL_lose_context",
    "WEBGL_multi_draw",
    "WEBGL_polygon_mode",
];

pub fn log_capabilities() {
    let mut have = Vec::new();
    let mut missing = Vec::new();
    for e in WEBGL_EXT_LIST {
        if webgl_extension_supported(e) { have.push(*e) } else { missing.push(*e) }
    }
    log::info(&format!("WebGL2 extensions available: {}", have.join(" ")));
    log::info(&format!("WebGL2 extensions missing:   {}", missing.join(" ")));
    for e in ["WEBGL_compressed_texture_s3tc", "EXT_color_buffer_float"] {
        if !webgl_extension_supported(e) {
            log::error(&format!("REQUIRED extension {e} is missing; rendering will be wrong"));
        }
    }
}

// ---------------------------------------------------------------------------------------
// glGet

/// Queries whose WebGL result is an object (returned to C as its emscripten id).
fn binding_kind(pname: u32) -> Option<Kind> {
    Some(match pname {
        34964 | 34965 | 36662 | 36663 | 35053 | 35055 | 35983 | 35368 => Kind::Buffer,
        35725 => Kind::Program,
        36006 | 36010 => Kind::Framebuffer,
        36007 => Kind::Renderbuffer,
        32873 | 34068 | 32874 | 35869 => Kind::Texture,
        34229 => Kind::Vao,
        35097 => Kind::Sampler,
        _ => return None,
    })
}

/// emscriptenWebGLGet; ty: 0 int, 2 float, 4 bool.
fn webgl_get(pname: u32, p: u32, ty: u32) {
    if p == 0 {
        with(|s| s.record_error(gl::INVALID_VALUE));
        return;
    }
    let special: Option<i64> = match pname {
        36346 => Some(1),                         // SHADER_COMPILER
        36344 => return,                          // SHADER_BINARY_FORMATS
        34814 | 36345 => Some(0),                 // NUM_PROGRAM/SHADER_BINARY_FORMATS
        33309 => Some(2 * WEBGL_EXT_LIST.iter().filter(|e| webgl_extension_supported(e)).count() as i64),
        33307 => Some(3),                         // MAJOR_VERSION
        33308 => Some(0),                         // MINOR_VERSION
        _ => None,
    };
    let write_scalar = |v: i64| match ty {
        0 => mem::set_i32(p, v as i32),
        2 => mem::set_f32(p, v as f32),
        _ => mem::set_u8(p, (v != 0) as u8),
    };
    if let Some(v) = special {
        write_scalar(v);
        return;
    }
    if let Some(kind) = binding_kind(pname) {
        let mut native = 0i32;
        unsafe { gl::GetIntegerv(pname, &mut native) };
        let id = with(|s| s.em(kind, native as u32));
        write_scalar(id as i64);
        return;
    }
    unsafe {
        match ty {
            0 => gl::GetIntegerv(pname, mem::ptr(p) as *mut i32),
            2 => gl::GetFloatv(pname, mem::ptr(p) as *mut f32),
            _ => gl::GetBooleanv(pname, mem::ptr(p)),
        }
    }
}

// ---------------------------------------------------------------------------------------
// Packets

/// ExecuteGLPackets(start, count, data, uploads, uploadCount, bufferUploads, bufferCount, ...)
fn execute_packets(start: u32, count: u32, data: u32, uploads: u32, upload_count: u32, buf_uploads: u32, buf_count: u32) {
    let mut s = ST.lock();
    let s = s.as_mut().expect("GL used before init");
    unsafe {
        if buf_count > 0 {
            let mut prev = 0i32;
            gl::GetIntegerv(gl::COPY_WRITE_BUFFER_BINDING, &mut prev);
            for j in 0..buf_count {
                let u = buf_uploads + j * 16;
                gl::BindBuffer(gl::COPY_WRITE_BUFFER, s.n(Kind::Buffer, mem::u32(u) & 0x7fff_ffff));
                gl::BufferSubData(
                    gl::COPY_WRITE_BUFFER,
                    mem::u32(u + 4) as isize,
                    mem::u32(u + 8) as isize,
                    mem::ptr(mem::u32(u + 12)) as *const _,
                );
            }
            gl::BindBuffer(gl::COPY_WRITE_BUFFER, prev as u32);
        }
        if upload_count > 0 {
            let mut prev = 0i32;
            gl::GetIntegerv(gl::UNIFORM_BUFFER_BINDING, &mut prev);
            for j in 0..upload_count {
                let u = uploads + j * 16;
                gl::BindBuffer(gl::UNIFORM_BUFFER, s.n(Kind::Buffer, mem::u32(u)));
                gl::BufferSubData(
                    gl::UNIFORM_BUFFER,
                    mem::u32(u + 4) as isize,
                    mem::u32(u + 8) as isize,
                    mem::ptr(mem::u32(u + 12)) as *const _,
                );
            }
            gl::BindBuffer(gl::UNIFORM_BUFFER, prev as u32);
        }
        for j in 0..count {
            let p = start + j * 32;
            let op = mem::u32(p);
            let a = mem::u32(p + 4);
            let b = mem::u32(p + 8);
            let c = mem::u32(p + 12);
            let d = mem::u32(p + 16);
            let e = mem::u32(p + 20);
            let f = mem::u32(p + 24);
            match op {
                0 => gl::Enable(a),
                1 => gl::Disable(a),
                2 => {
                    gl::UseProgram(s.n(Kind::Program, a));
                    s.current_program = a;
                }
                3 => gl::BindBuffer(a, s.n(Kind::Buffer, b)),
                4 => gl::BindBufferRange(a, b, s.n(Kind::Buffer, c), d as isize, e as isize),
                5 => gl::BindVertexArray(s.n(Kind::Vao, a)),
                6 => gl::VertexAttribPointer(a, b as i32, c, (d != 0) as u8, e as i32, f as usize as *const _),
                7 => gl::EnableVertexAttribArray(a),
                8 => gl::DisableVertexAttribArray(a),
                9 => gl::ActiveTexture(a),
                10 => gl::BindTexture(a, s.n(Kind::Texture, b)),
                11 => gl::BindSampler(a, s.n(Kind::Sampler, b)),
                12 => gl::SamplerParameteri(s.n(Kind::Sampler, a), b, c as i32),
                13 => gl::SamplerParameterf(s.n(Kind::Sampler, a), b, mem::f32(p + 12)),
                14 => gl::DepthFunc(a),
                15 => gl::DepthMask((a != 0) as u8),
                16 => gl::BlendFuncSeparate(a, b, c, d),
                17 => gl::BlendEquationSeparate(a, b),
                18 => gl::BlendColor(mem::f32(p + 4), mem::f32(p + 8), mem::f32(p + 12), mem::f32(p + 16)),
                19 => gl::FrontFace(a),
                20 => gl::CullFace(a),
                21 => gl::ColorMask((a != 0) as u8, (b != 0) as u8, (c != 0) as u8, (d != 0) as u8),
                22 => gl::Scissor(a as i32, b as i32, c as i32, d as i32),
                23 => gl::Viewport(a as i32, b as i32, c as i32, d as i32),
                24 => gl::DepthRangef(mem::f32(p + 4), mem::f32(p + 8)),
                25 => gl::StencilFuncSeparate(a, b, c as i32, d),
                26 => gl::StencilMaskSeparate(a, b),
                27 => gl::StencilOpSeparate(a, b, c, d),
                28 => gl::PolygonOffset(mem::f32(p + 4), mem::f32(p + 8)),
                29 => gl::Uniform1i(uniform_loc(s, a as i32), b as i32),
                30 => gl::Uniform1f(uniform_loc(s, a as i32), mem::f32(p + 8)),
                31 => gl::Uniform2f(uniform_loc(s, a as i32), mem::f32(p + 8), mem::f32(p + 12)),
                32 => gl::DrawArrays(a, b as i32, c as i32),
                33 => gl::DrawElements(a, b as i32, c, d as usize as *const _),
                34 => {
                    let mut prev = 0i32;
                    gl::GetIntegerv(gl::COPY_WRITE_BUFFER_BINDING, &mut prev);
                    gl::BindBuffer(gl::COPY_WRITE_BUFFER, s.n(Kind::Buffer, a));
                    gl::BufferSubData(gl::COPY_WRITE_BUFFER, b as isize, c as isize, mem::ptr(data + d) as *const _);
                    gl::BindBuffer(gl::COPY_WRITE_BUFFER, prev as u32);
                }
                35 => gl::BindFramebuffer(a, s.n(Kind::Framebuffer, b)),
                36 => {
                    for i in 0..b {
                        let t = data + a + i * 24;
                        let unit = mem::u32(t);
                        let mask = mem::u32(t + 4);
                        let target = mem::u32(t + 8);
                        let tex = s.n(Kind::Texture, mem::u32(t + 12));
                        let sampler = s.n(Kind::Sampler, mem::u32(t + 16));
                        if mask & 7 != 0 {
                            gl::ActiveTexture(gl::TEXTURE0 + unit);
                            if mask & 1 != 0 {
                                gl::BindTexture(gl::TEXTURE_2D, if target == gl::TEXTURE_2D { tex } else { 0 });
                            }
                            if mask & 2 != 0 {
                                gl::BindTexture(gl::TEXTURE_CUBE_MAP, if target == gl::TEXTURE_CUBE_MAP { tex } else { 0 });
                            }
                            if mask & 4 != 0 {
                                gl::BindTexture(gl::TEXTURE_3D, if target == gl::TEXTURE_3D { tex } else { 0 });
                            }
                        }
                        if mask & 8 != 0 {
                            gl::BindSampler(unit, sampler);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// Imports

fn gen(kind: Kind, n: i32, out: i32, make: unsafe fn(i32, *mut u32)) {
    let n = n.max(0) as usize;
    let mut v = vec![0u32; n];
    unsafe { make(n as i32, v.as_mut_ptr()) };
    with(|s| {
        for (i, native) in v.into_iter().enumerate() {
            let id = if native != 0 {
                s.new_id(kind, native)
            } else {
                s.record_error(gl::INVALID_OPERATION);
                0
            };
            mem::set_u32(out as u32 + 4 * i as u32, id);
        }
    });
}

fn delete(kind: Kind, n: i32, ids: i32, del: unsafe fn(i32, *const u32)) {
    let natives: Vec<u32> = with(|s| {
        (0..n.max(0) as u32)
            .filter_map(|i| {
                let id = mem::u32(ids as u32 + 4 * i);
                if kind == Kind::Program {
                    s.programs.remove(&id);
                }
                s.forget(kind, id)
            })
            .collect()
    });
    if !natives.is_empty() {
        unsafe { del(natives.len() as i32, natives.as_ptr()) };
    }
}

fn source_string(count: i32, strings: u32, lengths: u32) -> String {
    let mut src = String::new();
    for i in 0..count.max(0) as u32 {
        let sp = mem::u32(strings + 4 * i);
        if lengths != 0 {
            let len = mem::u32(lengths + 4 * i);
            src.push_str(&String::from_utf8_lossy(mem::bytes(sp, len as usize)));
        } else {
            src.push_str(&mem::cstr(sp));
        }
    }
    src
}

// Handles of contexts created with emscripten_webgl_create_context.
thread_local! {
    static CURRENT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
static CONTEXT_ATTRS: Mutex<Vec<(u32, u8)>> = Mutex::new(Vec::new());

pub fn register(l: &mut Linker<Ctx>) -> Result<()> {
    // --- context ------------------------------------------------------------------------------
    l.func_wrap("env", "emscripten_webgl_create_context", |mut cx: Cx, _target: i32, attrs: i32| -> Result<i32> {
        let ex = cx.data().ex().clone();
        let handle = ex.malloc.call(&mut cx, 8)? as u32;
        let me = ex.pthread_self.call(&mut cx, ())?;
        mem::set_u32(handle, 0);
        mem::set_u32(handle + 4, me as u32);
        let ext_default = mem::u8(attrs as u32 + 24);
        CONTEXT_ATTRS.lock().push((handle, ext_default));
        log::info(&format!(
            "webgl context {handle:#x} created on thread {me:#x} (alpha={} depth={} stencil={} aa={} v={}.{})",
            mem::u8(attrs as u32),
            mem::u8(attrs as u32 + 1),
            mem::u8(attrs as u32 + 2),
            mem::u8(attrs as u32 + 3),
            mem::i32(attrs as u32 + 16),
            mem::i32(attrs as u32 + 20)
        ));
        Ok(handle as i32)
    })?;
    l.func_wrap("env", "emscripten_webgl_make_context_current", |mut cx: Cx, h: i32| -> i32 {
        if h == 0 {
            gl_ctx::release_current();
            cx.data_mut().gl_current = false;
            CURRENT.with(|c| c.set(0));
            return 0;
        }
        match gl_ctx::make_current() {
            Ok(first) => {
                cx.data_mut().gl_current = true;
                CURRENT.with(|c| c.set(h as u32));
                if first {
                    log_capabilities();
                }
                0
            }
            Err(e) => {
                log::error(&format!("eglMakeCurrent failed: {e}"));
                -5
            }
        }
    })?;
    l.func_wrap("env", "emscripten_webgl_get_current_context", |_: Cx| -> i32 { CURRENT.with(|c| c.get()) as i32 })?;
    l.func_wrap("env", "emscripten_webgl_get_drawing_buffer_size", |_: Cx, _h: i32, w: i32, hp: i32| -> i32 {
        if w == 0 || hp == 0 {
            return -5;
        }
        let (sw, sh) = gl_ctx::surface_size();
        mem::set_i32(w as u32, sw);
        mem::set_i32(hp as u32, sh);
        0
    })?;
    l.func_wrap("env", "emscripten_webgl_get_context_attributes", |_: Cx, c: i32, a: i32| -> i32 {
        if a == 0 {
            return -5;
        }
        let Some(ext) = CONTEXT_ATTRS.lock().iter().find(|x| x.0 == c as u32).map(|x| x.1) else { return -3 };
        let a = a as u32;
        for (off, v) in [(0, 1u8), (1, 1), (2, 1), (3, 0), (4, 1), (5, 0), (12, 0), (33, 0)] {
            mem::set_u8(a + off, v);
        }
        mem::set_i32(a + 8, 2);
        mem::set_i32(a + 16, 2);
        mem::set_i32(a + 20, 0);
        mem::set_u8(a + 24, ext);
        0
    })?;
    l.func_wrap("env", "emscripten_webgl_enable_extension", |_: Cx, _c: i32, name: i32| -> i32 {
        let n = mem::cstr(name as u32);
        let ok = webgl_extension_supported(&n);
        if !ok {
            log::info(&format!("extension {n} not available"));
        }
        ok as i32
    })?;
    l.func_wrap("env", "emscripten_is_webgl_context_lost", |_: Cx, _h: i32| -> i32 { 0 })?;

    // --- packets -------------------------------------------------------------------------------
    l.func_wrap(
        "env",
        "ExecuteGLPackets",
        |_: Cx, start: i32, count: i32, data: i32, uploads: i32, upload_count: i32, buf_uploads: i32, buf_count: i32, _he: i32, _ht: i32, _hs: i32| -> i32 {
            execute_packets(
                start as u32,
                count as u32,
                data as u32,
                uploads as u32,
                upload_count as u32,
                buf_uploads as u32,
                buf_count as u32,
            );
            0
        },
    )?;

    // --- objects ---------------------------------------------------------------------------------
    l.func_wrap("env", "glGenBuffers", |_: Cx, n: i32, o: i32| gen(Kind::Buffer, n, o, |n, p| unsafe { gl::GenBuffers(n, p) }))?;
    l.func_wrap("env", "glGenFramebuffers", |_: Cx, n: i32, o: i32| {
        gen(Kind::Framebuffer, n, o, |n, p| unsafe { gl::GenFramebuffers(n, p) })
    })?;
    l.func_wrap("env", "glGenQueries", |_: Cx, n: i32, o: i32| gen(Kind::Query, n, o, |n, p| unsafe { gl::GenQueries(n, p) }))?;
    l.func_wrap("env", "glGenSamplers", |_: Cx, n: i32, o: i32| gen(Kind::Sampler, n, o, |n, p| unsafe { gl::GenSamplers(n, p) }))?;
    l.func_wrap("env", "glGenTextures", |_: Cx, n: i32, o: i32| gen(Kind::Texture, n, o, |n, p| unsafe { gl::GenTextures(n, p) }))?;
    l.func_wrap("env", "glGenVertexArrays", |_: Cx, n: i32, o: i32| {
        gen(Kind::Vao, n, o, |n, p| unsafe { gl::GenVertexArrays(n, p) })
    })?;
    l.func_wrap("env", "glDeleteBuffers", |_: Cx, n: i32, ids: i32| {
        delete(Kind::Buffer, n, ids, |n, p| unsafe { gl::DeleteBuffers(n, p) })
    })?;
    l.func_wrap("env", "glDeleteFramebuffers", |_: Cx, n: i32, ids: i32| {
        delete(Kind::Framebuffer, n, ids, |n, p| unsafe { gl::DeleteFramebuffers(n, p) })
    })?;
    l.func_wrap("env", "glDeleteQueries", |_: Cx, n: i32, ids: i32| {
        delete(Kind::Query, n, ids, |n, p| unsafe { gl::DeleteQueries(n, p) })
    })?;
    l.func_wrap("env", "glDeleteSamplers", |_: Cx, n: i32, ids: i32| {
        delete(Kind::Sampler, n, ids, |n, p| unsafe { gl::DeleteSamplers(n, p) })
    })?;
    l.func_wrap("env", "glDeleteTextures", |_: Cx, n: i32, ids: i32| {
        delete(Kind::Texture, n, ids, |n, p| unsafe { gl::DeleteTextures(n, p) })
    })?;
    l.func_wrap("env", "glDeleteVertexArrays", |_: Cx, n: i32, ids: i32| {
        delete(Kind::Vao, n, ids, |n, p| unsafe { gl::DeleteVertexArrays(n, p) })
    })?;
    l.func_wrap("env", "glCreateProgram", |_: Cx| -> i32 {
        let native = unsafe { gl::CreateProgram() };
        with(|s| {
            let id = s.new_id(Kind::Program, native);
            s.programs.insert(id, ProgramInfo { id_counter: 1, ..Default::default() });
            id as i32
        })
    })?;
    l.func_wrap("env", "glCreateShader", |_: Cx, ty: i32| -> i32 {
        let native = unsafe { gl::CreateShader(ty as u32) };
        with(|s| s.new_id(Kind::Shader, native) as i32)
    })?;
    l.func_wrap("env", "glDeleteProgram", |_: Cx, id: i32| {
        if id == 0 {
            return;
        }
        let native = with(|s| {
            s.programs.remove(&(id as u32));
            if s.current_program == id as u32 {
                s.current_program = 0;
            }
            let n = s.forget(Kind::Program, id as u32);
            if n.is_none() {
                s.record_error(gl::INVALID_VALUE);
            }
            n
        });
        if let Some(n) = native {
            unsafe { gl::DeleteProgram(n) };
        }
    })?;
    l.func_wrap("env", "glDeleteShader", |_: Cx, id: i32| {
        if id == 0 {
            return;
        }
        if let Some(n) = with(|s| s.forget(Kind::Shader, id as u32)) {
            unsafe { gl::DeleteShader(n) };
        }
    })?;

    // --- shaders / programs ------------------------------------------------------------------------
    l.func_wrap("env", "glShaderSource", |_: Cx, sh: i32, count: i32, strings: i32, lengths: i32| {
        let src = source_string(count, strings as u32, lengths as u32);
        let c = CString::new(src.replace('\0', " ")).unwrap_or_default();
        let native = with(|s| s.n(Kind::Shader, sh as u32));
        unsafe { gl::ShaderSource(native, 1, &c.as_ptr(), std::ptr::null()) };
    })?;
    l.func_wrap("env", "glCompileShader", |_: Cx, sh: i32| {
        let native = with(|s| s.n(Kind::Shader, sh as u32));
        unsafe {
            gl::CompileShader(native);
            let mut ok = 0;
            gl::GetShaderiv(native, gl::COMPILE_STATUS, &mut ok);
            if ok == 0 {
                let mut buf = vec![0u8; 4096];
                let mut len = 0;
                gl::GetShaderInfoLog(native, buf.len() as i32, &mut len, buf.as_mut_ptr() as *mut _);
                log::error(&format!("shader compile failed: {}", String::from_utf8_lossy(&buf[..len.max(0) as usize])));
            }
        }
    })?;
    l.func_wrap("env", "glAttachShader", |_: Cx, prog: i32, sh: i32| {
        let (p, s) = with(|st| (st.n(Kind::Program, prog as u32), st.n(Kind::Shader, sh as u32)));
        unsafe { gl::AttachShader(p, s) };
    })?;
    l.func_wrap("env", "glLinkProgram", |_: Cx, prog: i32| {
        let p = with(|s| {
            if let Some(info) = s.programs.get_mut(&(prog as u32)) {
                let counter = info.id_counter.max(1);
                *info = ProgramInfo { id_counter: counter, ..Default::default() };
            }
            s.n(Kind::Program, prog as u32)
        });
        unsafe { gl::LinkProgram(p) };
    })?;
    l.func_wrap("env", "glGetProgramiv", |_: Cx, prog: i32, pname: i32, out: i32| {
        if out == 0 {
            with(|s| s.record_error(gl::INVALID_VALUE));
            return;
        }
        let p = with(|s| s.n(Kind::Program, prog as u32));
        unsafe { gl::GetProgramiv(p, pname as u32, mem::ptr(out as u32) as *mut i32) };
    })?;
    l.func_wrap("env", "glGetProgramInfoLog", |_: Cx, prog: i32, max: i32, len: i32, log_ptr: i32| {
        let p = with(|s| s.n(Kind::Program, prog as u32));
        let mut buf = vec![0u8; 8192];
        let mut n = 0i32;
        unsafe { gl::GetProgramInfoLog(p, buf.len() as i32, &mut n, buf.as_mut_ptr() as *mut _) };
        let text = String::from_utf8_lossy(&buf[..n.max(0) as usize]).into_owned();
        let written = if max > 0 && log_ptr != 0 { mem::write_cstr(log_ptr as u32, &text, max as usize) } else { 0 };
        if len != 0 {
            mem::set_i32(len as u32, written as i32);
        }
    })?;
    l.func_wrap("env", "glGetAttribLocation", |_: Cx, prog: i32, name: i32| -> i32 {
        let p = with(|s| s.n(Kind::Program, prog as u32));
        let c = CString::new(mem::cstr(name as u32)).unwrap_or_default();
        unsafe { gl::GetAttribLocation(p, c.as_ptr()) }
    })?;
    l.func_wrap("env", "glGetUniformBlockIndex", |_: Cx, prog: i32, name: i32| -> i32 {
        let p = with(|s| s.n(Kind::Program, prog as u32));
        let c = CString::new(mem::cstr(name as u32)).unwrap_or_default();
        unsafe { gl::GetUniformBlockIndex(p, c.as_ptr()) as i32 }
    })?;
    l.func_wrap("env", "glUniformBlockBinding", |_: Cx, prog: i32, idx: i32, binding: i32| {
        let p = with(|s| s.n(Kind::Program, prog as u32));
        unsafe { gl::UniformBlockBinding(p, idx as u32, binding as u32) };
    })?;
    l.func_wrap("env", "glGetUniformLocation", |_: Cx, prog: i32, name: i32| -> i32 {
        let name = mem::cstr(name as u32);
        with(|s| {
            let native = s.n(Kind::Program, prog as u32);
            let Some(info) = s.programs.get_mut(&(prog as u32)) else {
                s.record_error(gl::INVALID_VALUE);
                return -1;
            };
            prepare_uniforms(native, info);
            let (base, mut index) = match left_brace(&name) {
                Some(lb) if lb > 0 => {
                    let idx = name[lb + 1..name.len() - 1].trim().parse::<i64>().unwrap_or(0) as u32;
                    (name[..lb].to_string(), idx)
                }
                _ => (name.clone(), 0),
            };
            let Some(&(size, id)) = info.size_and_ids_by_name.get(&base) else { return -1 };
            if index >= size {
                return -1;
            }
            index += id;
            let existing = info.locs_by_id.get(&index).copied();
            let resolved = match existing {
                Some(Loc::Index(j)) if j > 0 => Some(Loc::Index(j)),
                Some(Loc::Native(n)) if n >= 0 => Some(Loc::Native(n)),
                _ => {
                    let n = native_uniform_location(native, &name);
                    if n >= 0 { Some(Loc::Native(n)) } else { None }
                }
            };
            match resolved {
                Some(l) => {
                    info.locs_by_id.insert(index, l);
                    index as i32
                }
                None => -1,
            }
        })
    })?;
    l.func_wrap("env", "glUniform1iv", |_: Cx, loc: i32, count: i32, v: i32| {
        if count == 0 {
            return;
        }
        let n = with(|s| uniform_loc(s, loc));
        unsafe { gl::Uniform1iv(n, count, mem::ptr(v as u32) as *const i32) };
    })?;
    l.func_wrap("env", "glUniform4iv", |_: Cx, loc: i32, count: i32, v: i32| {
        if count == 0 {
            return;
        }
        let n = with(|s| uniform_loc(s, loc));
        unsafe { gl::Uniform4iv(n, count, mem::ptr(v as u32) as *const i32) };
    })?;
    l.func_wrap("env", "glUniform4fv", |_: Cx, loc: i32, count: i32, v: i32| {
        if count == 0 {
            return;
        }
        let n = with(|s| uniform_loc(s, loc));
        unsafe { gl::Uniform4fv(n, count, mem::ptr(v as u32) as *const f32) };
    })?;

    // --- buffers / framebuffers ---------------------------------------------------------------------
    l.func_wrap("env", "glBufferData", |_: Cx, target: i32, size: i32, data: i32, usage: i32| unsafe {
        let ptr = if data != 0 && size != 0 { mem::ptr(data as u32) as *const _ } else { std::ptr::null() };
        gl::BufferData(target as u32, size as u32 as isize, ptr, usage as u32);
    })?;
    l.func_wrap("env", "glBufferSubData", |_: Cx, target: i32, off: i32, size: i32, data: i32| unsafe {
        if size != 0 {
            gl::BufferSubData(target as u32, off as u32 as isize, size as u32 as isize, mem::ptr(data as u32) as *const _);
        }
    })?;
    l.func_wrap("env", "glBindFramebuffer", |_: Cx, target: i32, fb: i32| {
        let n = with(|s| s.n(Kind::Framebuffer, fb as u32));
        unsafe { gl::BindFramebuffer(target as u32, n) };
    })?;
    l.func_wrap("env", "glFramebufferTexture2D", |_: Cx, target: i32, att: i32, textarget: i32, tex: i32, level: i32| {
        let n = with(|s| s.n(Kind::Texture, tex as u32));
        unsafe { gl::FramebufferTexture2D(target as u32, att as u32, textarget as u32, n, level) };
    })?;
    l.func_wrap("env", "glCheckFramebufferStatus", |_: Cx, t: i32| -> i32 { unsafe { gl::CheckFramebufferStatus(t as u32) as i32 } })?;
    l.func_wrap(
        "env",
        "glGetFramebufferAttachmentParameteriv",
        |_: Cx, target: i32, att: i32, pname: i32, out: i32| unsafe {
            let mut v = 0i32;
            gl::GetFramebufferAttachmentParameteriv(target as u32, att as u32, pname as u32, &mut v);
            if pname as u32 == gl::FRAMEBUFFER_ATTACHMENT_OBJECT_NAME {
                let mut ty = 0i32;
                gl::GetFramebufferAttachmentParameteriv(target as u32, att as u32, gl::FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE, &mut ty);
                let kind = if ty as u32 == gl::RENDERBUFFER { Kind::Renderbuffer } else { Kind::Texture };
                v = with(|s| s.em(kind, v as u32)) as i32;
            }
            mem::set_i32(out as u32, v);
        },
    )?;
    l.func_wrap(
        "env",
        "glBlitFramebuffer",
        |_: Cx, a: i32, b: i32, c: i32, d: i32, e: i32, f: i32, g: i32, h: i32, mask: i32, filter: i32| unsafe {
            gl::BlitFramebuffer(a, b, c, d, e, f, g, h, mask as u32, filter as u32)
        },
    )?;
    l.func_wrap("env", "glDrawBuffers", |_: Cx, n: i32, bufs: i32| unsafe {
        gl::DrawBuffers(n, mem::ptr(bufs as u32) as *const u32)
    })?;
    l.func_wrap("env", "glReadBuffer", |_: Cx, b: i32| unsafe { gl::ReadBuffer(b as u32) })?;
    l.func_wrap("env", "glClearBufferfv", |_: Cx, buf: i32, db: i32, v: i32| unsafe {
        gl::ClearBufferfv(buf as u32, db, mem::ptr(v as u32) as *const f32)
    })?;
    l.func_wrap("env", "glClearBufferiv", |_: Cx, buf: i32, db: i32, v: i32| unsafe {
        gl::ClearBufferiv(buf as u32, db, mem::ptr(v as u32) as *const i32)
    })?;
    l.func_wrap("env", "glClearBufferfi", |_: Cx, buf: i32, db: i32, depth: f32, stencil: i32| unsafe {
        gl::ClearBufferfi(buf as u32, db, depth, stencil)
    })?;
    l.func_wrap("env", "glReadPixels", |_: Cx, x: i32, y: i32, w: i32, h: i32, fmt: i32, ty: i32, px: i32| unsafe {
        gl::ReadPixels(x, y, w, h, fmt as u32, ty as u32, mem::ptr(px as u32) as *mut _)
    })?;

    // --- textures ----------------------------------------------------------------------------------
    l.func_wrap("env", "glTexParameteri", |_: Cx, t: i32, p: i32, v: i32| unsafe { gl::TexParameteri(t as u32, p as u32, v) })?;
    l.func_wrap("env", "glTexStorage2D", |_: Cx, t: i32, levels: i32, fmt: i32, w: i32, h: i32| unsafe {
        gl::TexStorage2D(t as u32, levels, fmt as u32, w, h)
    })?;
    l.func_wrap("env", "glTexStorage3D", |_: Cx, t: i32, levels: i32, fmt: i32, w: i32, h: i32, d: i32| unsafe {
        gl::TexStorage3D(t as u32, levels, fmt as u32, w, h, d)
    })?;
    l.func_wrap(
        "env",
        "glTexSubImage2D",
        |_: Cx, t: i32, lvl: i32, x: i32, y: i32, w: i32, h: i32, fmt: i32, ty: i32, px: i32| unsafe {
            gl::TexSubImage2D(t as u32, lvl, x, y, w, h, fmt as u32, ty as u32, mem::ptr_or_null(px as u32))
        },
    )?;
    l.func_wrap(
        "env",
        "glTexSubImage3D",
        |_: Cx, t: i32, lvl: i32, x: i32, y: i32, z: i32, w: i32, h: i32, d: i32, fmt: i32, ty: i32, px: i32| unsafe {
            gl::TexSubImage3D(t as u32, lvl, x, y, z, w, h, d, fmt as u32, ty as u32, mem::ptr_or_null(px as u32))
        },
    )?;
    l.func_wrap(
        "env",
        "glCompressedTexSubImage2D",
        |_: Cx, t: i32, lvl: i32, x: i32, y: i32, w: i32, h: i32, fmt: i32, size: i32, data: i32| unsafe {
            gl::CompressedTexSubImage2D(t as u32, lvl, x, y, w, h, fmt as u32, size, mem::ptr_or_null(data as u32))
        },
    )?;
    l.func_wrap("env", "glPixelStorei", |_: Cx, p: i32, v: i32| {
        // WebGL-only unpack flags (37440..37443) have no GLES equivalent.
        if !(37440..=37443).contains(&p) {
            unsafe { gl::PixelStorei(p as u32, v) }
        }
    })?;

    // --- queries / state -----------------------------------------------------------------------------
    l.func_wrap("env", "glBeginQuery", |_: Cx, target: i32, id: i32| {
        let n = with(|s| s.n(Kind::Query, id as u32));
        unsafe { gl::BeginQuery(target as u32, n) };
    })?;
    l.func_wrap("env", "glEndQuery", |_: Cx, target: i32| unsafe { gl::EndQuery(target as u32) })?;
    l.func_wrap("env", "glGetQueryObjectuiv", |_: Cx, id: i32, pname: i32, out: i32| {
        if out == 0 {
            with(|s| s.record_error(gl::INVALID_VALUE));
            return;
        }
        let n = with(|s| s.n(Kind::Query, id as u32));
        unsafe { gl::GetQueryObjectuiv(n, pname as u32, mem::ptr(out as u32) as *mut u32) };
    })?;
    l.func_wrap("env", "glIsEnabled", |_: Cx, cap: i32| -> i32 { unsafe { gl::IsEnabled(cap as u32) as i32 } })?;
    l.func_wrap("env", "glGetError", |_: Cx| -> i32 {
        let native = unsafe { gl::GetError() };
        with(|s| {
            let e = if native != 0 { native } else { s.last_error };
            s.last_error = 0;
            e as i32
        })
    })?;
    l.func_wrap("env", "glGetIntegerv", |_: Cx, p: i32, out: i32| webgl_get(p as u32, out as u32, 0))?;
    l.func_wrap("env", "glGetFloatv", |_: Cx, p: i32, out: i32| webgl_get(p as u32, out as u32, 2))?;
    l.func_wrap("env", "glGetBooleanv", |_: Cx, p: i32, out: i32| webgl_get(p as u32, out as u32, 4))?;
    Ok(())
}
