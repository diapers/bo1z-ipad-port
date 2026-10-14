//! The native replacement for Emscripten's pthread/worker runtime.
//!
//! Browser model -> native model:
//!   page main thread (JS event loop)  -> OS main thread (winit loop), wasm instance #0
//!   em-pthread worker                 -> OS thread with its own wasmtime Store + Instance
//!   SharedArrayBuffer heap            -> one wasmtime SharedMemory imported by every instance
//!   postMessage({cmd: 4}) mailbox     -> Inbox (mutex + condvar) per thread
//!   throw "unwind"                    -> host error `Unwind`, caught where JS would catch it
//!   requestAnimationFrame main loop   -> loop in the thread's event loop, paced by eglSwapBuffers
//!
//! Sequences mirror KisakBlack-web.mjs (handleMessage cmd 1/2, invokeEntryPoint, callMain).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use wasmtime::{format_err as anyhow, Result};
use parking_lot::{Condvar, Mutex};
use wasmtime::*;

use crate::{emval, gl_ctx, imports, log, mem};

pub const PAGES: u32 = 32767;

// ---------------------------------------------------------------------------------------
// Control-flow "exceptions" thrown by host functions.

#[derive(Debug)]
pub struct Unwind;
impl std::fmt::Display for Unwind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("unwind")
    }
}
impl std::error::Error for Unwind {}

#[derive(Debug)]
pub struct ExitStatus(pub i32);
impl std::fmt::Display for ExitStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit({})", self.0)
    }
}
impl std::error::Error for ExitStatus {}

pub fn unwind<T>() -> Result<T> {
    Err(wasmtime::Error::new(Unwind))
}

pub enum Outcome<T> {
    Ok(T),
    Unwound,
}

/// What JS's callUserCallback/handleException does with an error.
fn settle<T>(r: Result<T>, what: &str) -> Outcome<T> {
    match r {
        Ok(v) => Outcome::Ok(v),
        Err(e) if e.is::<Unwind>() => Outcome::Unwound,
        Err(e) => {
            if let Some(ExitStatus(code)) = e.downcast_ref::<ExitStatus>() {
                log::info(&format!("engine exited with code {code}"));
                crate::platform::quit(*code);
            }
            log::error(&format!("FATAL in {what}: {e:?}"));
            crate::platform::fatal(&format!("The engine crashed in {what}:\n{e}"));
        }
    }
}

// ---------------------------------------------------------------------------------------
// Per-thread message queue (the worker's onmessage).

pub enum Msg {
    /// cmd 4: run _emscripten_check_mailbox.
    Mailbox,
    /// cmd 6 (main thread only): a pthread finished; free its data.
    Cleanup(u32),
    /// Page input forwarded from the UI thread: KB_Input(kind, a, b).
    Input(i32, i32, i32),
}

pub struct Inbox {
    q: Mutex<VecDeque<Msg>>,
    cv: Condvar,
}

impl Inbox {
    pub fn new(is_main: bool) -> Arc<Self> {
        let _ = is_main;
        Arc::new(Self { q: Mutex::new(VecDeque::new()), cv: Condvar::new() })
    }

    pub fn push(&self, m: Msg) {
        self.q.lock().push_back(m);
        self.cv.notify_one();
    }

    /// Take everything queued, waiting up to `timeout` if empty (None = forever).
    pub fn take(&self, timeout: Option<Duration>) -> Vec<Msg> {
        let mut q = self.q.lock();
        if q.is_empty() {
            match timeout {
                Some(t) if t.is_zero() => {}
                Some(t) => {
                    self.cv.wait_for(&mut q, t);
                }
                None => self.cv.wait(&mut q),
            }
        }
        q.drain(..).collect()
    }
}

// ---------------------------------------------------------------------------------------
// Store data.

#[derive(Clone, Copy)]
pub struct MainLoop {
    pub func: u32,
    pub arg: u32,
    pub mode: i32,
    pub value: i32,
}

pub struct Ctx {
    pub tid: u32,
    pub is_main: bool,
    pub ex: Option<Exports>,
    pub keepalive: i32,
    pub no_exit: bool,
    pub exit_status: i32,
    pub main_loop: Option<MainLoop>,
    pub inbox: Arc<Inbox>,
    pub emval: emval::Handles,
    /// emscripten_webgl_make_context_current succeeded on this thread.
    pub gl_current: bool,
}

impl Ctx {
    pub fn ex(&self) -> &Exports {
        self.ex.as_ref().expect("exports not ready")
    }
    pub fn keep_alive(&self) -> bool {
        self.no_exit || self.keepalive > 0
    }
}

/// Exports the host calls, resolved once per instance.
#[derive(Clone)]
pub struct Exports {
    pub table: Table,
    pub malloc: TypedFunc<i32, i32>,
    pub free: TypedFunc<i32, ()>,
    pub pthread_self: TypedFunc<(), i32>,
    pub call_ctors: TypedFunc<(), ()>,
    pub embind_init: TypedFunc<(), ()>,
    pub get_type_name: TypedFunc<i32, i32>,
    pub tls_init: TypedFunc<(), i32>,
    pub proxy_main: TypedFunc<(i32, i32), i32>,
    pub thread_init: TypedFunc<(i32, i32, i32, i32, i32, i32), ()>,
    pub thread_exit: TypedFunc<i32, ()>,
    pub thread_free_data: TypedFunc<i32, ()>,
    pub check_mailbox: TypedFunc<(), ()>,
    pub proxy_finish: TypedFunc<i32, ()>,
    pub proxy_execute_queue: TypedFunc<i32, ()>,
    pub run_js_done: TypedFunc<(i32, i32, f64), ()>,
    pub stack_set_limits: TypedFunc<(i32, i32), ()>,
    pub stack_restore: TypedFunc<i32, ()>,
    pub stack_alloc: TypedFunc<i32, i32>,
    pub stack_current: TypedFunc<(), i32>,
    pub record_entry: TypedFunc<(i32, i32, i32), ()>,
    pub kb_input: TypedFunc<(i32, i32, i32), i32>,
    pub kb_ui_state: TypedFunc<(), i32>,
    pub kb_audio_ring: TypedFunc<(), i32>,
    pub kb_set_program_manifest: Option<TypedFunc<(i32, i32), ()>>,
}

impl Exports {
    fn resolve(store: &mut Store<Ctx>, i: &Instance) -> Result<Self> {
        macro_rules! f {
            ($name:literal) => {
                i.get_typed_func(&mut *store, $name)
                    .map_err(|e| anyhow!("export {}: {e}", $name))?
            };
        }
        Ok(Self {
            table: i
                .get_table(&mut *store, "__indirect_function_table")
                .ok_or_else(|| anyhow!("no function table export"))?,
            malloc: f!("malloc"),
            free: f!("free"),
            pthread_self: f!("pthread_self"),
            call_ctors: f!("__wasm_call_ctors"),
            embind_init: f!("_embind_initialize_bindings"),
            get_type_name: f!("__getTypeName"),
            tls_init: f!("_emscripten_tls_init"),
            proxy_main: f!("_emscripten_proxy_main"),
            thread_init: f!("_emscripten_thread_init"),
            thread_exit: f!("_emscripten_thread_exit"),
            thread_free_data: f!("_emscripten_thread_free_data"),
            check_mailbox: f!("_emscripten_check_mailbox"),
            proxy_finish: f!("emscripten_proxy_finish"),
            proxy_execute_queue: f!("emscripten_proxy_execute_queue"),
            run_js_done: f!("_emscripten_run_js_on_main_thread_done"),
            stack_set_limits: f!("emscripten_stack_set_limits"),
            stack_restore: f!("_emscripten_stack_restore"),
            stack_alloc: f!("_emscripten_stack_alloc"),
            stack_current: f!("emscripten_stack_get_current"),
            record_entry: f!("_wasmfs_opfs_record_entry"),
            kb_input: f!("KB_Input"),
            kb_ui_state: f!("KB_UIState"),
            kb_audio_ring: f!("KB_AudioRing"),
            kb_set_program_manifest: i.get_typed_func(&mut *store, "KB_SetProgramManifest").ok(),
        })
    }
}

/// getWasmTableEntry(ptr) as a typed function.
pub fn table_func<P: WasmParams, R: WasmResults>(
    mut cx: impl AsContextMut<Data = Ctx>,
    idx: u32,
) -> Result<TypedFunc<P, R>> {
    let table = cx.as_context().data().ex().table;
    match table.get(&mut cx, idx as u64) {
        Some(Ref::Func(Some(f))) => f.typed::<P, R>(&cx),
        _ => Err(anyhow!("null/invalid function table entry {idx}")),
    }
}

// ---------------------------------------------------------------------------------------
// Process-wide runtime.

pub struct Paths {
    /// Directory that plays the role of navigator.storage.getDirectory().
    pub opfs_root: std::path::PathBuf,
    /// Virtual OPFS path -> read-only map files (e.g. "pack/files" -> Five's tree).
    pub mounts: Vec<(String, crate::fs::MountSrc)>,
}

pub struct Rt {
    pub engine: Engine,
    pub pre: InstancePre<Ctx>,
    pub memory: SharedMemory,
    pub inboxes: Mutex<HashMap<u32, Arc<Inbox>>>,
    pub main_tid: AtomicU32,
    pub main_inbox: Arc<Inbox>,
    /// Engine arguments (callMain args, without argv[0]).
    pub args: Vec<String>,
    pub paths: Paths,
    pub started: Instant,
    pub epoch_ms: f64,
    pub shutting_down: AtomicBool,
}

static RT: OnceLock<Rt> = OnceLock::new();

pub fn rt() -> &'static Rt {
    RT.get().expect("runtime not initialised")
}

/// The one engine configuration. `--precompile` and the device must agree on every
/// setting, which wasmtime checks when the AOT artifact is loaded.
pub fn engine_config() -> Config {
    let mut c = Config::new();
    c.wasm_threads(true);
    c.shared_memory(true);
    c.wasm_bulk_memory(true);
    c.wasm_reference_types(true);
    c.wasm_multi_value(true);
    c.wasm_simd(true);
    c.wasm_exceptions(true);
    c.wasm_tail_call(true);
    c.wasm_extended_const(true);
    c.cranelift_opt_level(OptLevel::Speed);
    // Engine code can recurse deeply (script VM, BSP traversal). Native threads are
    // spawned with a larger stack to match.
    c.max_wasm_stack(16 << 20);
    // One fixed 2 GiB shared memory: reserve 4 GiB + guard so bounds checks are elided,
    // and never move it (iOS address space is tight; needs extended-virtual-addressing).
    c.memory_reservation(4 << 30);
    c.memory_guard_size(32 << 20);
    c.guard_before_linear_memory(false);
    c.memory_may_move(false);
    c.async_stack_size(20 << 20);
    c
}

pub fn init(wasm_path: &std::path::Path, args: Vec<String>, paths: Paths) -> Result<()> {
    let t0 = Instant::now();
    let (engine, module) = if let Some(code) = crate::aot::embedded() {
        log::info(&format!("loading embedded AOT engine code ({:.1} MB, no JIT needed)", code.len() as f64 / 1e6));
        let mut cfg = engine_config();
        cfg.with_custom_code_memory(Some(crate::aot::code_memory()));
        let engine = Engine::new(&cfg)?;
        let module = crate::aot::load(&engine, code)?;
        (engine, module)
    } else {
        crate::platform::check_jit();
        log::info(&format!("compiling {} ...", wasm_path.display()));
        let engine = Engine::new(&engine_config())?;
        let module = if wasm_path.extension().map_or(false, |e| e == "cwasm") {
            unsafe { Module::deserialize_file(&engine, wasm_path)? }
        } else {
            Module::from_file(&engine, wasm_path)?
        };
        (engine, module)
    };
    log::info(&format!("module ready in {:.1?}", t0.elapsed()));

    let memory = SharedMemory::new(&engine, MemoryType::shared(PAGES, PAGES))?;
    {
        let data = memory.data();
        mem::init(data.as_ptr() as *mut u8, data.len());
    }

    let mut linker: Linker<Ctx> = Linker::new(&engine);
    // The memory definition does not bind to a store; a throwaway store satisfies the API.
    let mut tmp = Store::new(&engine, blank_ctx(0, false));
    linker.define(&mut tmp, "env", "memory", memory.clone())?;
    imports::register(&mut linker)?;
    // Anything not implemented traps with its name, so gaps show up loudly.
    for imp in module.imports() {
        if linker.get_by_import(&mut tmp, &imp).is_none() {
            if let ExternType::Func(ft) = imp.ty() {
                let name = format!("{}.{}", imp.module(), imp.name());
                log::info(&format!("UNIMPLEMENTED import {name}"));
                linker.func_new(imp.module(), imp.name(), ft, move |_, _, _| {
                    Err(anyhow!("unimplemented import {name}"))
                })?;
            }
        }
    }
    let pre = linker.instantiate_pre(&module)?;

    let epoch_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(0.0);
    RT.set(Rt {
        engine,
        pre,
        memory,
        inboxes: Mutex::new(HashMap::new()),
        main_tid: AtomicU32::new(0),
        main_inbox: Inbox::new(true),
        args,
        paths,
        started: Instant::now(),
        epoch_ms,
        shutting_down: AtomicBool::new(false),
    })
    .map_err(|_| anyhow!("runtime initialised twice"))?;
    Ok(())
}

fn blank_ctx(tid: u32, is_main: bool) -> Ctx {
    Ctx {
        tid,
        is_main,
        ex: None,
        keepalive: 0,
        no_exit: false,
        exit_status: 0,
        main_loop: None,
        inbox: match RT.get() {
            Some(r) if is_main => r.main_inbox.clone(),
            _ => Inbox::new(false),
        },
        emval: emval::Handles::new(),
        gl_current: false,
    }
}

fn new_store(tid: u32, is_main: bool, inbox: Option<Arc<Inbox>>) -> Result<Store<Ctx>> {
    let r = rt();
    let mut ctx = blank_ctx(tid, is_main);
    if let Some(i) = inbox {
        ctx.inbox = i;
    }
    let mut store = Store::new(&r.engine, ctx);
    let inst = r.pre.instantiate(&mut store)?;
    let ex = Exports::resolve(&mut store, &inst)?;
    store.data_mut().ex = Some(ex);
    Ok(store)
}

// ---------------------------------------------------------------------------------------
// Main browser thread.

pub struct MainThread {
    pub store: Store<Ctx>,
}

/// createWasm + run() + callMain() on the OS main thread.
pub fn boot_main() -> Result<MainThread> {
    let r = rt();
    let mut store = new_store(0, true, None)?;
    let ex = store.data().ex().clone();

    // initRuntime(): ctors call _emscripten_init_main_thread_js(tb), which records tid.
    ex.call_ctors.call(&mut store, ())?;
    let tid = r.main_tid.load(Ordering::Acquire);
    store.data_mut().tid = tid;
    log::info(&format!("main runtime thread tb={tid:#x}"));

    // engine.js: KB_AudioRing before callMain; the ring lives in static data.
    let ring = ex.kb_audio_ring.call(&mut store, ())? as u32;
    crate::audio::configure(ring);

    // engine.js: the map's program manifest (<map>-programs.json) before callMain, so the
    // engine links those programs while the map loads instead of on first sight.
    if let (Some(set), Some(bytes)) = (ex.kb_set_program_manifest.clone(), crate::shadercache::manifest()) {
        let p = ex.malloc.call(&mut store, bytes.len() as i32)?;
        if p != 0 {
            mem::write(p as u32, &bytes);
            settle(set.call(&mut store, (p, bytes.len() as i32)), "KB_SetProgramManifest");
            ex.free.call(&mut store, p)?;
        }
    }

    // callMain(args): argv on the main thread's stack, then _emscripten_proxy_main,
    // which creates the main pthread (canvas transferred to it) and returns.
    let mut argv_strs = vec!["./this.program".to_string()];
    argv_strs.extend(r.args.iter().cloned());
    log::info(&format!("args: {}", r.args.join(" ")));
    let argc = argv_strs.len() as i32;
    let argv = ex.stack_alloc.call(&mut store, (argc + 1) * 4)? as u32;
    for (i, s) in argv_strs.iter().enumerate() {
        let size = s.len() as i32 + 1;
        let p = ex.stack_alloc.call(&mut store, size)? as u32;
        mem::write_cstr(p, s, size as usize);
        mem::set_u32(argv + 4 * i as u32, p);
    }
    mem::set_u32(argv + 4 * argc as u32, 0);
    store.data_mut().keepalive += 1; // runtimeKeepalivePush in callMain
    if let Outcome::Ok(ret) = settle(ex.proxy_main.call(&mut store, (argc, argv as i32)), "callMain") {
        log::info(&format!("_emscripten_proxy_main returned {ret}"));
    }
    Ok(MainThread { store })
}

impl MainThread {
    /// Drain the main inbox (called whenever the platform loop wakes).
    pub fn pump(&mut self) {
        for m in rt().main_inbox.take(Some(Duration::ZERO)) {
            self.handle(m);
        }
    }

    fn handle(&mut self, m: Msg) {
        let ex = self.store.data().ex().clone();
        match m {
            Msg::Mailbox => {
                settle(ex.check_mailbox.call(&mut self.store, ()), "main mailbox");
            }
            Msg::Cleanup(t) => {
                rt().inboxes.lock().remove(&t);
                settle(ex.thread_free_data.call(&mut self.store, t as i32), "thread cleanup");
            }
            Msg::Input(k, a, b) => {
                self.kb_input(k, a, b);
            }
        }
    }

    /// The browser-main event loop: mailbox, forwarded input, and the UI-state probe the
    /// UI thread reads for the controller mapper.
    fn run(mut self) -> ! {
        loop {
            for m in rt().main_inbox.take(Some(Duration::from_millis(8))) {
                self.handle(m);
            }
            let ui = self.ui_state();
            UI_STATE.store(ui as u32, Ordering::Relaxed);
        }
    }

    pub fn kb_input(&mut self, kind: i32, a: i32, b: i32) -> i32 {
        let ex = self.store.data().ex().clone();
        match settle(ex.kb_input.call(&mut self.store, (kind, a, b)), "KB_Input") {
            Outcome::Ok(v) => v,
            Outcome::Unwound => 0,
        }
    }

    pub fn ui_state(&mut self) -> i32 {
        let ex = self.store.data().ex().clone();
        match settle(ex.kb_ui_state.call(&mut self.store, ()), "KB_UIState") {
            Outcome::Ok(v) => v,
            Outcome::Unwound => 0,
        }
    }
}

static UI_STATE: AtomicU32 = AtomicU32::new(0);

/// KB_UIState() as last sampled by the browser-main thread.
pub fn ui_state() -> i32 {
    UI_STATE.load(Ordering::Relaxed) as i32
}

/// Queue KB_Input on the browser-main thread (the page's main thread in the web build).
pub fn post_input(kind: i32, a: i32, b: i32) {
    // The intro gate opens only once the map has loaded (the loading screen asks for it).
    if kind == crate::input::INTRO {
        if !crate::loading::done() || crate::loading::is_released() {
            return;
        }
        crate::loading::released();
        crate::log::info("player started");
    }
    if let Some(r) = RT.get() {
        r.main_inbox.push(Msg::Input(kind, a, b));
    }
}

/// Everything needed to compile and start the engine.
pub struct Boot {
    pub wasm: std::path::PathBuf,
    pub args: Vec<String>,
    pub paths: Paths,
}

/// Run the browser-main role on its own big-stack thread (iOS gives the UI thread 1 MiB):
/// compile the module, boot the runtime, then service the main mailbox forever.
pub fn start_browser_main(boot: Boot) {
    std::thread::Builder::new()
        .name("browser-main".into())
        .stack_size(32 << 20)
        .spawn(move || {
            if let Err(e) = init(&boot.wasm, boot.args, boot.paths) {
                crate::platform::fatal(&format!("engine load failed: {e:?}"));
            }
            match boot_main() {
                Ok(m) => {
                    crate::audio::start();
                    m.run()
                }
                Err(e) => crate::platform::fatal(&format!("engine boot failed: {e:?}")),
            }
        })
        .expect("spawn browser-main thread");
}

/// Whether the UI thread may forward input yet (the runtime exists once init finished).
pub fn ready() -> bool {
    RT.get().is_some()
}

// ---------------------------------------------------------------------------------------
// pthreads.

/// __pthread_create_js: native threads can be spawned from any thread, so nothing is
/// proxied to the main thread.
pub fn spawn_pthread(tid: u32, start: u32, arg: u32) -> i32 {
    let r = rt();
    let inbox = Inbox::new(false);
    r.inboxes.lock().insert(tid, inbox.clone());
    let res = std::thread::Builder::new()
        .name(format!("em-pthread-{tid:x}"))
        .stack_size(32 << 20)
        .spawn(move || {
            if let Err(e) = pthread_main(tid, start, arg, inbox) {
                log::error(&format!("pthread {tid:#x} failed: {e:?}"));
                crate::platform::fatal(&format!("A game thread failed to start:\n{e}"));
            }
        });
    match res {
        Ok(_) => 0,
        Err(e) => {
            log::error(&format!("cannot spawn thread: {e}"));
            r.inboxes.lock().remove(&tid);
            6 // EAGAIN
        }
    }
}

fn pthread_main(tid: u32, start: u32, arg: u32, inbox: Arc<Inbox>) -> Result<()> {
    let mut store = new_store(tid, false, Some(inbox))?;
    let ex = store.data().ex().clone();

    // handleMessage cmd 2
    let stack_high = mem::u32(tid + 48);
    let stack_size = mem::u32(tid + 52);
    ex.stack_set_limits.call(&mut store, (stack_high as i32, (stack_high - stack_size) as i32))?;
    ex.stack_restore.call(&mut store, stack_high as i32)?;
    ex.thread_init.call(&mut store, (tid as i32, 0, 0, 1, 0, 0))?;
    ex.tls_init.call(&mut store, ())?;
    ex.embind_init.call(&mut store, ())?;

    // invokeEntryPoint
    store.data_mut().keepalive = 0;
    store.data_mut().no_exit = false;
    let entry = table_func::<i32, i32>(&mut store, start)?;
    let what = format!("thread {tid:#x} entry");
    if let Outcome::Ok(result) = settle(entry.call(&mut store, arg as i32), &what) {
        store.data_mut().exit_status = result;
    }
    if !maybe_exit(&mut store) {
        event_loop(&mut store);
    }
    Ok(())
}

/// maybeExit(): returns true when the thread has exited.
fn maybe_exit(store: &mut Store<Ctx>) -> bool {
    if store.data().keep_alive() {
        return false;
    }
    let ex = store.data().ex().clone();
    let status = store.data().exit_status;
    settle(ex.thread_exit.call(&mut *store, status), "thread exit");
    true
}

fn event_loop(store: &mut Store<Ctx>) {
    let inbox = store.data().inbox.clone();
    let mut next_tick = Instant::now();
    let mut stats = FrameStats::default();
    loop {
        if rt().shutting_down.load(Ordering::Relaxed) {
            return;
        }
        let timeout = match store.data().main_loop {
            None => None,
            Some(ml) if ml.mode != 1 => Some(next_tick.saturating_duration_since(Instant::now())),
            Some(_) => Some(Duration::ZERO),
        };
        for m in inbox.take(timeout) {
            match m {
                Msg::Mailbox => {
                    let ex = store.data().ex().clone();
                    settle(ex.check_mailbox.call(&mut *store, ()), "mailbox");
                }
                Msg::Cleanup(_) | Msg::Input(..) => {}
            }
            if maybe_exit(store) {
                return;
            }
        }
        let Some(ml) = store.data().main_loop else { continue };
        if ml.mode != 1 && Instant::now() < next_tick {
            continue;
        }
        let tick_start = Instant::now();
        match table_func::<i32, ()>(&mut *store, ml.func) {
            Ok(f) => {
                settle(f.call(&mut *store, ml.arg as i32), "main loop");
            }
            Err(e) => {
                log::error(&format!("main loop function: {e}"));
                return;
            }
        }
        // OffscreenCanvas commit: a frame drawn in this iteration presents when the worker yields.
        if store.data().gl_current && crate::gl::take_drew() {
            let work_end = Instant::now();
            stats.frame(work_end);
            if let Some(dir) = rt().paths.opfs_root.parent() {
                gl_ctx::before_present(dir);
            }
            if crate::glstats::on() {
                crate::glstats::frame_errors();
            }
            gl_ctx::swap();
            PRESENTED.fetch_add(1, Ordering::Relaxed);
            stats.split(work_end - tick_start, work_end.elapsed());
        } else if ml.mode == 1 {
            std::thread::sleep(Duration::from_millis(16));
        }
        if ml.mode == 0 {
            next_tick = tick_start + Duration::from_millis(ml.value.max(0) as u64);
        } else if ml.mode == 2 {
            // setImmediate: the browser runs this back-to-back, but natively that spins a
            // whole core (thermal throttling on iPad). 1 ms keeps ~1000 iterations/s.
            next_tick = tick_start + Duration::from_millis(1);
        }
        if maybe_exit(store) {
            return;
        }
    }
}

/// _emscripten_notify_mailbox_postmessage
pub fn notify_mailbox(target: u32, current: u32, own: &Arc<Inbox>) {
    let r = rt();
    if target == current {
        own.push(Msg::Mailbox);
    } else if target == r.main_tid.load(Ordering::Relaxed) {
        r.main_inbox.push(Msg::Mailbox);
    } else if let Some(i) = r.inboxes.lock().get(&target) {
        i.push(Msg::Mailbox);
    }
}

/// _emscripten_thread_cleanup
pub fn thread_cleanup(cx: &mut Caller<'_, Ctx>, thread: u32) -> Result<()> {
    if cx.data().is_main {
        rt().inboxes.lock().remove(&thread);
        let ex = cx.data().ex().clone();
        ex.thread_free_data.call(&mut *cx, thread as i32)?;
    } else {
        rt().main_inbox.push(Msg::Cleanup(thread));
    }
    Ok(())
}

static PRESENTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// ms since `rt().started` of the last frame that took over 50 ms (the start gate).
static LAST_HITCH_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How long frames have been presenting without a hitch (> 50 ms).
pub fn smooth_for() -> Duration {
    let now = rt().started.elapsed().as_millis() as u64;
    Duration::from_millis(now.saturating_sub(LAST_HITCH_MS.load(Ordering::Relaxed)))
}

/// Frames presented so far (the frame counter).
pub fn presented_frames() -> u64 {
    PRESENTED.load(Ordering::Relaxed)
}

/// Logs frame pacing every 5 s: fps, average and worst frame-to-frame time, how many frames
/// took longer than 1.5x the average (the hitches that read as choppiness), the average time
/// in the engine's frame ("game") and in presenting it ("present": waits on the GPU), and
/// the file data read meanwhile.
#[derive(Default)]
struct FrameStats {
    last: Option<Instant>,
    window_start: Option<Instant>,
    times: Vec<f32>,
    work_ms: f32,
    present_ms: f32,
    split_n: u32,
}

impl FrameStats {
    fn split(&mut self, work: Duration, present: Duration) {
        self.work_ms += work.as_secs_f32() * 1000.0;
        self.present_ms += present.as_secs_f32() * 1000.0;
        self.split_n += 1;
    }

    fn frame(&mut self, now: Instant) {
        if let Some(last) = self.last {
            let ms = (now - last).as_secs_f32() * 1000.0;
            self.times.push(ms);
            if ms > 50.0 {
                LAST_HITCH_MS.store(rt().started.elapsed().as_millis() as u64, Ordering::Relaxed);
            }
        }
        self.last = Some(now);
        let start = *self.window_start.get_or_insert(now);
        let span = (now - start).as_secs_f32();
        if span >= 5.0 && !self.times.is_empty() {
            let n = self.times.len() as f32;
            let avg = self.times.iter().sum::<f32>() / n;
            let max = self.times.iter().cloned().fold(0.0, f32::max);
            let hitches = self.times.iter().filter(|&&t| t > avg * 1.5).count();
            let k = self.split_n.max(1) as f32;
            log::info(&format!(
                "frames: {:.1} fps, avg {avg:.1} ms, max {max:.1} ms, {hitches} hitches; game {:.1} ms, present {:.1} ms; read {:.1} MB",
                n / span,
                self.work_ms / k,
                self.present_ms / k,
                crate::loading::take_read_bytes() as f64 / 1e6
            ));
            (self.work_ms, self.present_ms, self.split_n) = (0.0, 0.0, 0);
            self.times.clear();
            self.window_start = Some(now);
        }
    }
}
