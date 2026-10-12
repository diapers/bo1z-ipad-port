//! Host implementations of the module's `env` / `wasi_snapshot_preview1` imports that
//! are not GL, filesystem or embind (those live in gl.rs, fs.rs, emval.rs).
//! Each one follows its JS counterpart in KisakBlack-web.mjs.

use std::sync::atomic::Ordering;

use wasmtime::{format_err as anyhow, Result};
use wasmtime::*;

use crate::rt::{self, rt, Ctx, ExitStatus, MainLoop};
use crate::{emval, fs, gl, log, mem, video};

type Cx<'a> = Caller<'a, Ctx>;

pub fn register(l: &mut Linker<Ctx>) -> Result<()> {
    fs::register(l)?;
    emval::register(l)?;
    gl::register(l)?;
    register_runtime(l)?;
    register_time(l)?;
    register_wasi(l)?;
    register_webgpu_stubs(l)?;
    Ok(())
}

fn now_ms() -> f64 {
    let r = rt();
    r.epoch_ms + r.started.elapsed().as_secs_f64() * 1000.0
}

fn register_runtime(l: &mut Linker<Ctx>) -> Result<()> {
    // --- logging / aborts -------------------------------------------------------------
    l.func_wrap("env", "emscripten_out", |_: Cx, s: i32| log::engine(&mem::cstr(s as u32), false))?;
    l.func_wrap("env", "emscripten_err", |_: Cx, s: i32| log::engine(&mem::cstr(s as u32), true))?;
    l.func_wrap("env", "emscripten_console_error", |_: Cx, s: i32| {
        log::engine(&mem::cstr(s as u32), true)
    })?;
    l.func_wrap("env", "_abort_js", |_: Cx| -> Result<()> { Err(anyhow!("abort()")) })?;
    l.func_wrap("env", "__assert_fail", |_: Cx, c: i32, f: i32, line: i32, func: i32| -> Result<()> {
        Err(anyhow!(
            "Assertion failed: {}, at: {}:{} {}",
            mem::cstr(c as u32),
            mem::cstr(f as u32),
            line,
            mem::cstr(func as u32)
        ))
    })?;
    l.func_wrap("env", "__call_sighandler", |mut cx: Cx, fp: i32, sig: i32| -> Result<()> {
        let f = rt::table_func::<i32, ()>(&mut cx, fp as u32)?;
        f.call(&mut cx, sig)
    })?;
    l.func_wrap("env", "emscripten_get_callstack", |_: Cx, _flags: i32, s: i32, max: i32| -> i32 {
        if s == 0 || max <= 0 {
            return 1;
        }
        mem::write_cstr(s as u32, "(native host)", max as usize) as i32
    })?;
    l.func_wrap("env", "emscripten_return_address", |_: Cx, _level: i32| 0i32)?;
    l.func_wrap("env", "emscripten_check_blocking_allowed", |_: Cx| {})?;
    l.func_wrap("env", "emscripten_get_heap_max", |_: Cx| -> i32 { mem::len() as i32 })?;
    l.func_wrap("env", "emscripten_resize_heap", |_: Cx, _req: i32| -> Result<i32> {
        Err(anyhow!("OOM: emscripten_resize_heap (fixed 2 GiB heap exhausted)"))
    })?;

    // --- exit / keepalive ---------------------------------------------------------------
    l.func_wrap("env", "exit", |_: Cx, code: i32| -> Result<()> {
        Err(wasmtime::Error::new(ExitStatus(code)))
    })?;
    l.func_wrap("wasi_snapshot_preview1", "proc_exit", |_: Cx, code: i32| -> Result<()> {
        Err(wasmtime::Error::new(ExitStatus(code)))
    })?;
    l.func_wrap("env", "emscripten_exit_with_live_runtime", |mut cx: Cx| -> Result<()> {
        cx.data_mut().keepalive += 1;
        rt::unwind()
    })?;
    l.func_wrap("env", "emscripten_unwind_to_js_event_loop", |_: Cx| -> Result<()> { rt::unwind() })?;
    l.func_wrap("env", "emscripten_runtime_keepalive_check", |cx: Cx| -> i32 {
        cx.data().keep_alive() as i32
    })?;
    l.func_wrap("env", "_emscripten_runtime_keepalive_clear", |mut cx: Cx| {
        cx.data_mut().no_exit = false;
        cx.data_mut().keepalive = 0;
    })?;

    // --- main loop ------------------------------------------------------------------------
    l.func_wrap(
        "env",
        "emscripten_set_main_loop_arg",
        |mut cx: Cx, func: i32, arg: i32, fps: i32, simulate: i32| -> Result<()> {
            let (mode, value) = if fps > 0 { (0, 1000 / fps) } else { (1, 1) };
            let d = cx.data_mut();
            if d.main_loop.is_none() {
                d.keepalive += 1; // scheduler installed: runtimeKeepalivePush
            }
            d.main_loop = Some(MainLoop { func: func as u32, arg: arg as u32, mode, value });
            log::info(&format!("main loop set on thread {:#x} (fps={fps})", d.tid));
            if simulate != 0 {
                return rt::unwind();
            }
            Ok(())
        },
    )?;
    l.func_wrap("env", "emscripten_set_main_loop_timing", |mut cx: Cx, mode: i32, value: i32| -> i32 {
        match cx.data_mut().main_loop.as_mut() {
            Some(ml) => {
                ml.mode = mode;
                ml.value = value;
                0
            }
            None => 1,
        }
    })?;

    // --- threads ----------------------------------------------------------------------------
    l.func_wrap("env", "_emscripten_init_main_thread_js", |mut cx: Cx, tb: i32| -> Result<()> {
        // Browser main thread: Atomics.wait is unavailable, so can_block = 0. The C
        // runtime then busy-waits with _emscripten_yield, which services the mailbox.
        rt().main_tid.store(tb as u32, Ordering::Release);
        cx.data_mut().tid = tb as u32;
        let ex = cx.data().ex().clone();
        ex.thread_init.call(&mut cx, (tb, 1, 1, 0, 8388608, 0))?;
        ex.tls_init.call(&mut cx, ())?;
        Ok(())
    })?;
    l.func_wrap("env", "__pthread_create_js", |_: Cx, ptr: i32, attr: i32, start: i32, arg: i32| -> i32 {
        if attr != 0 {
            let names = mem::u32(attr as u32 + 40);
            if names != 0 {
                let label = if names == u32::MAX { "#game".to_string() } else { mem::cstr(names) };
                log::info(&format!("pthread {ptr:#x} receives canvas {label}"));
            }
        }
        rt::spawn_pthread(ptr as u32, start as u32, arg as u32)
    })?;
    l.func_wrap("env", "_emscripten_thread_mailbox_await", |_: Cx, _ptr: i32| {
        // Behave like the waitAsync polyfill: notifications always arrive via
        // _emscripten_notify_mailbox_postmessage.
    })?;
    l.func_wrap("env", "_emscripten_notify_mailbox_postmessage", |cx: Cx, target: i32, cur: i32| {
        rt::notify_mailbox(target as u32, cur as u32, &cx.data().inbox);
    })?;
    l.func_wrap("env", "_emscripten_thread_cleanup", |mut cx: Cx, thread: i32| -> Result<()> {
        rt::thread_cleanup(&mut cx, thread as u32)
    })?;
    l.func_wrap("env", "_emscripten_thread_set_strongref", |_: Cx, _t: i32| {})?;
    l.func_wrap("env", "_wasmfs_thread_utils_heartbeat", |_: Cx, queue: i32| {
        // setInterval(() => emscripten_proxy_execute_queue(queue), 50) on this thread.
        // Our worker threads never sit idle in a JS event loop, so the queue is serviced
        // by the normal mailbox path; log it in case the engine depends on it.
        log::info(&format!("wasmfs heartbeat requested for queue {queue:#x} (serviced via mailbox)"));
    })?;
    l.func_wrap(
        "env",
        "_emscripten_receive_on_main_thread_js",
        |mut cx: Cx, func_index: i32, em_asm: i32, _calling: i32, buf_size: i32, args: i32, ctx: i32, ctx_args: i32|
         -> Result<f64> {
            let mut vals = Vec::new();
            let mut b = args as u32;
            let end = b + buf_size as u32;
            while b < end {
                let is_big = mem::u64(b) != 0;
                vals.push(if is_big { Arg::I64(mem::u64(b + 8) as i64) } else { Arg::F64(mem::f64(b + 8)) });
                b += 16;
            }
            let rtn = if em_asm != 0 {
                asm_const(&mut cx, em_asm as u32, &vals)?
            } else {
                proxied(&mut cx, func_index, &vals)?
            };
            if ctx != 0 {
                let ex = cx.data().ex().clone();
                ex.run_js_done.call(&mut cx, (ctx, ctx_args, rtn))?;
                return Ok(0.0);
            }
            Ok(rtn)
        },
    )?;

    // --- EM_ASM -------------------------------------------------------------------------------
    for name in ["emscripten_asm_const_int", "emscripten_asm_const_int_sync_on_main_thread"] {
        l.func_wrap("env", name, |mut cx: Cx, code: i32, sig: i32, buf: i32| -> Result<i32> {
            let args = read_em_asm_args(sig as u32, buf as u32);
            Ok(asm_const(&mut cx, code as u32, &args)? as i32)
        })?;
    }
    l.func_wrap(
        "env",
        "emscripten_asm_const_async_on_main_thread",
        |mut cx: Cx, code: i32, sig: i32, buf: i32| -> Result<()> {
            let args = read_em_asm_args(sig as u32, buf as u32);
            asm_const(&mut cx, code as u32, &args)?;
            Ok(())
        },
    )?;

    // --- canvas / D3D context watch -------------------------------------------------------------
    l.func_wrap("env", "emscripten_set_canvas_element_size", |_: Cx, _t: i32, w: i32, h: i32| -> i32 {
        log::info(&format!("canvas size request {w}x{h}"));
        crate::gl_ctx::set_requested_size(w, h);
        0
    })?;
    l.func_wrap("env", "WatchD3DContext", |_: Cx, _h: i32, _user: i32, _cb: i32| {})?;
    l.func_wrap("env", "UnwatchD3DContext", |_: Cx, _user: i32| {})?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// EM_ASM / proxied JS

#[derive(Clone, Copy, Debug)]
pub enum Arg {
    I64(i64),
    F64(f64),
}

impl Arg {
    fn i(self) -> i32 {
        match self {
            Arg::I64(v) => v as i32,
            Arg::F64(v) => v as i64 as i32,
        }
    }
    fn f(self) -> f64 {
        match self {
            Arg::I64(v) => v as f64,
            Arg::F64(v) => v,
        }
    }
}

/// readEmAsmArgs
fn read_em_asm_args(mut sig: u32, mut buf: u32) -> Vec<Arg> {
    let mut out = Vec::new();
    loop {
        let ch = mem::u8(sig);
        sig += 1;
        if ch == 0 {
            break;
        }
        let wide = ch != b'i' && ch != b'p';
        if wide && buf % 8 != 0 {
            buf += 4;
        }
        out.push(match ch {
            b'p' => Arg::I64(mem::u32(buf) as i64),
            b'j' => Arg::I64(mem::u64(buf) as i64),
            b'i' => Arg::I64(mem::i32(buf) as i64),
            _ => Arg::F64(mem::f64(buf)),
        });
        buf += if wide { 8 } else { 4 };
    }
    out
}

/// The ASM_CONSTS table of this build. Addresses are specific to KisakBlack-web.wasm
/// version that ships with the mirrored packs.
fn asm_const(_cx: &mut Cx, addr: u32, a: &[Arg]) -> Result<f64> {
    let arg = |i: usize| a.get(i).copied().unwrap_or(Arg::I64(0));
    Ok(match addr {
        1462644 => {
            video::open(arg(0).i(), &mem::cstr(arg(1).i() as u32), arg(2).i() as u32);
            0.0
        }
        1462713 => {
            video::rewind(arg(0).i());
            0.0
        }
        1462762 => {
            video::close(arg(0).i());
            0.0
        }
        1462810 | 1462862 => 0.0, // video pause / volume
        1462923 => 0.0,           // KISAK_P8_OFF & 31
        1462970 => 0.0,           // reset multi-draw state
        1463018 => 0.0,           // kisakGPUPipe && KISAK_P14
        1463086 => (args_json().len() + 1) as f64,
        1463278 => {
            mem::write_cstr(arg(0).i() as u32, &args_json(), arg(1).i() as usize);
            0.0
        }
        1463464 => {
            log::engine("KISAK_FLUSH_ROWS (omitted)", false);
            0.0
        }
        1463660 | 1463713 | 1463775 | 1464366 | 1464408 | 1464453 | 1464790 | 1464830 => 0.0, // profiling marks
        1464503 => 1.0, // KISAK_P16 default on
        1464742 => 0.0, // KISAK_SHADER_LOG
        _ => {
            log::error(&format!("unknown EM_ASM {addr} args={a:?}"));
            0.0
        }
    })
}

/// args.map(JSON.stringify).join(" ")
fn args_json() -> String {
    rt().args
        .iter()
        .map(|s| {
            let mut o = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => o.push_str("\\\""),
                    '\\' => o.push_str("\\\\"),
                    '\n' => o.push_str("\\n"),
                    c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                    c => o.push(c),
                }
            }
            o.push('"');
            o
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// proxiedFunctionTable
fn proxied(cx: &mut Cx, index: i32, a: &[Arg]) -> Result<f64> {
    let arg = |i: usize| a.get(i).copied().unwrap_or(Arg::I64(0));
    match index {
        0 | 1 => Err(wasmtime::Error::new(ExitStatus(arg(0).i()))),
        2 => Ok(rt::spawn_pthread(arg(0).i() as u32, arg(2).i() as u32, arg(3).i() as u32) as f64),
        3 => {
            crate::gl_ctx::set_requested_size(arg(1).i(), arg(2).i());
            Ok(0.0)
        }
        4 => Ok(environ_get(arg(0).i() as u32, arg(1).i() as u32) as f64),
        5 => Ok(environ_sizes_get(arg(0).i() as u32, arg(1).i() as u32) as f64),
        _ => {
            let _ = cx;
            Err(anyhow!("unknown proxied function {index} ({:?})", a.iter().map(|x| x.f()).collect::<Vec<_>>()))
        }
    }
}

// ---------------------------------------------------------------------------------------
// Time

fn register_time(l: &mut Linker<Ctx>) -> Result<()> {
    l.func_wrap("env", "emscripten_get_now", |_: Cx| -> f64 { now_ms() })?;
    l.func_wrap("env", "emscripten_date_now", |_: Cx| -> f64 { now_ms() })?;
    l.func_wrap("env", "_tzset_js", |_: Cx, timezone: i32, daylight: i32, std_name: i32, dst_name: i32| {
        // Treat the device as UTC: the engine only uses local time for log stamps.
        mem::set_i32(timezone as u32, 0);
        mem::set_i32(daylight as u32, 0);
        mem::write_cstr(std_name as u32, "UTC", 17);
        mem::write_cstr(dst_name as u32, "UTC", 17);
    })?;
    l.func_wrap("env", "_gmtime_js", |_: Cx, t: i64, tm: i32| -> i32 {
        write_tm(t, tm as u32);
        0
    })?;
    l.func_wrap("env", "_localtime_js", |_: Cx, t: i64, tm: i32| -> i32 {
        write_tm(t, tm as u32);
        0
    })?;
    l.func_wrap("env", "_timegm_js", |_: Cx, tm: i32| -> i64 {
        let tm = tm as u32;
        let year = mem::i32(tm + 20) as i64 + 1900;
        let mon = mem::i32(tm + 16) as i64;
        let days = days_from_civil(year, mon + 1, 1) + mem::i32(tm + 12) as i64 - 1;
        let secs = days * 86400
            + mem::i32(tm + 8) as i64 * 3600
            + mem::i32(tm + 4) as i64 * 60
            + mem::i32(tm) as i64;
        write_tm(secs, tm);
        secs
    })?;
    Ok(())
}

/// Howard Hinnant's days_from_civil.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn write_tm(t: i64, tm: u32) {
    let days = t.div_euclid(86400);
    let secs = t.rem_euclid(86400);
    // civil_from_days
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let yday = days - days_from_civil(y, 1, 1);
    mem::set_i32(tm, (secs % 60) as i32);
    mem::set_i32(tm + 4, ((secs / 60) % 60) as i32);
    mem::set_i32(tm + 8, (secs / 3600) as i32);
    mem::set_i32(tm + 12, d as i32);
    mem::set_i32(tm + 16, (m - 1) as i32);
    mem::set_i32(tm + 20, (y - 1900) as i32);
    mem::set_i32(tm + 24, (days + 4).rem_euclid(7) as i32);
    mem::set_i32(tm + 28, yday as i32);
    mem::set_i32(tm + 32, 0);
    mem::set_i32(tm + 36, 0);
}

// ---------------------------------------------------------------------------------------
// WASI

const ENVIRON: &[&str] = &[
    "USER=web_user",
    "LOGNAME=web_user",
    "PATH=/",
    "PWD=/",
    "HOME=/home/web_user",
    "LANG=en_US.UTF-8",
    "_=./this.program",
];

fn environ_get(environ: u32, buf: u32) -> i32 {
    let mut p = buf;
    for (i, s) in ENVIRON.iter().enumerate() {
        mem::set_u32(environ + 4 * i as u32, p);
        mem::write_cstr(p, s, s.len() + 1);
        p += s.len() as u32 + 1;
    }
    0
}

fn environ_sizes_get(count: u32, size: u32) -> i32 {
    mem::set_u32(count, ENVIRON.len() as u32);
    mem::set_u32(size, ENVIRON.iter().map(|s| s.len() as u32 + 1).sum());
    0
}

fn register_wasi(l: &mut Linker<Ctx>) -> Result<()> {
    const W: &str = "wasi_snapshot_preview1";
    l.func_wrap(W, "environ_get", |_: Cx, e: i32, b: i32| -> i32 { environ_get(e as u32, b as u32) })?;
    l.func_wrap(W, "environ_sizes_get", |_: Cx, c: i32, s: i32| -> i32 {
        environ_sizes_get(c as u32, s as u32)
    })?;
    l.func_wrap(W, "clock_time_get", |_: Cx, id: i32, _prec: i64, out: i32| -> i32 {
        if !(0..=3).contains(&id) {
            return 28;
        }
        mem::set_i64(out as u32, (now_ms() * 1e6).round() as i64);
        0
    })?;
    l.func_wrap(W, "random_get", |_: Cx, buf: i32, len: i32| -> i32 {
        // xorshift seeded from the clock; the engine uses this for seeds, not crypto.
        let mut s = (now_ms() * 1e3) as u64 ^ 0x9E37_79B9_7F4A_7C15 ^ (buf as u64) << 17;
        for b in mem::bytes_mut(buf as u32, len as usize) {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            *b = s as u8;
        }
        0
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------------------
// WebGPU path: never used. InitializeWebGPU returning 0 makes the engine log
// WEB_GPU_FALLBACK and use its WebGL2 renderer (r_webgpu is not set anyway).

fn register_webgpu_stubs(l: &mut Linker<Ctx>) -> Result<()> {
    l.func_wrap("env", "__asyncjs__InitializeWebGPU", |_: Cx, _p: i32| -> i32 { 0 })?;
    l.func_wrap("env", "RegisterWebGPUContext", |_: Cx| -> i32 { 0 })?;
    l.func_wrap("env", "WebGPUPendingPipelinesJS", |_: Cx| -> i32 { 0 })?;
    l.func_wrap("env", "WebGPUReadbackStateJS", |_: Cx, _a: i32| -> i32 { 0 })?;
    l.func_wrap("env", "SetWebGPUReadyTableJS", |_: Cx, _a: i32, _b: i32| {})?;
    l.func_wrap("env", "SetWebGPUProgramJS", |_: Cx, _a: i32, _b: i32, _c: i32, _d: i32, _e: i32| {})?;
    l.func_wrap("env", "RegisterWebGPUShaderJS", |_: Cx, _a: i32, _b: i32, _c: i32| {})?;
    l.func_wrap("env", "BeginWebGPUReadbackJS", |_: Cx, _a: i32, _b: i32, _c: i32, _d: i32, _e: i32, _f: i32| -> i32 {
        0
    })?;
    l.func_wrap("env", "FinishWebGPUReadbackJS", |_: Cx, _a: i32, _b: i32, _c: i32| -> i32 { 0 })?;
    l.func_wrap(
        "env",
        "__asyncjs__ReadWebGPUPixelsJS",
        |_: Cx, _a: i32, _b: i32, _c: i32, _d: i32, _e: i32, _f: i32, _g: i32| {},
    )?;
    l.func_wrap("env", "emwgpuDelete", |_: Cx, _a: i32| {})?;
    l.func_wrap("env", "emwgpuDeviceDestroy", |_: Cx, _a: i32| {})?;
    l.func_wrap("env", "wgpuDeviceHasFeature", |_: Cx, _a: i32, _b: i32| -> i32 { 0 })?;
    Ok(())
}
