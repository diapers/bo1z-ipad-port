//! Shader warm-up: build every shader program a map is known to use while its loading video
//! plays, instead of when the player first sees each material.
//!
//! The engine compiles programs on demand (KHR_parallel_shader_compile) and skips draws whose
//! program is not ready, so on the iPad objects pop in and frames hitch for the first minutes
//! of a map. Here every program the engine links is recorded per map (the exact sources: the
//! engine adapts vertex shaders to the vertex format, so the shader pack alone is not enough),
//! and the next time the map loads a worker thread with a shared GL context compiles and links
//! them all. ANGLE then finds the engine's own links in its program cache (blobcache.rs keeps
//! that cache on disk across launches).
//!
//! The engine's own warm-up gets the same list as a program manifest (<slug>-programs.json, the
//! site's format, built by build-program-manifests.mjs; rt.rs passes it to KB_SetProgramManifest).
//!
//! Lists: <game>/shadercache/<slug>.json shipped with the data (recorded on PC), merged with
//! <data>/shadercache/<slug>.json recorded on this device. Format:
//! {"shaders": {"<hash>": "<source>"}, "programs": [["<vs hash>", "<fs hash>"], ...]}.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::{gl_ctx, log};

/// FNV-1a 64: a stable hash for naming shader sources and cache files.
pub fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

#[derive(Default)]
struct List {
    shaders: HashMap<String, String>,
    programs: Vec<(String, String)>,
}

impl List {
    fn load(path: &Path) -> List {
        let Ok(text) = std::fs::read_to_string(path) else { return List::default() };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            log::error(&format!("shader list {} unreadable", path.display()));
            return List::default();
        };
        let shaders = v["shaders"]
            .as_object()
            .map(|m| m.iter().filter_map(|(k, s)| Some((k.clone(), s.as_str()?.to_string()))).collect())
            .unwrap_or_default();
        let programs = v["programs"]
            .as_array()
            .map(|a| a.iter().filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str()?.to_string()))).collect())
            .unwrap_or_default();
        List { shaders, programs }
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        let v = serde_json::json!({
            "shaders": self.shaders,
            "programs": self.programs.iter().map(|(a, b)| [a, b]).collect::<Vec<_>>(),
        });
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(&v)?)?;
        std::fs::rename(&tmp, path)
    }
}

struct Recorder {
    path: PathBuf,
    /// Programs from the seed and this device: not recorded again.
    known: HashSet<(String, String)>,
    /// This device's list, saved to `path`.
    own: List,
    dirty: bool,
}

static REC: Mutex<Option<Recorder>> = Mutex::new(None);

/// Background build progress: (built, total); done when no build runs.
static WARM_TOTAL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static WARM_BUILT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static WARM_RUNNING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn warm_progress() -> Option<(usize, usize)> {
    use std::sync::atomic::Ordering::Relaxed;
    WARM_RUNNING.load(Relaxed).then(|| (WARM_BUILT.load(Relaxed), WARM_TOTAL.load(Relaxed)))
}

/// The map's program manifest for the engine (KB_SetProgramManifest), if shipped.
static MANIFEST: Mutex<Option<Vec<u8>>> = Mutex::new(None);

pub fn manifest() -> Option<Vec<u8>> {
    MANIFEST.lock().clone()
}

fn hash_of(src: &str) -> String {
    format!("{:016x}", fnv(src.as_bytes()))
}

/// The map is chosen: load its lists and start building its programs in the background.
pub fn start(slug: &str, game: &Path, data: &Path) {
    let seed_path = game.join("shadercache").join(format!("{slug}.json"));
    // Same format as the site's <map>-programs.json (build-program-manifests.mjs).
    let manifest = std::fs::read(game.join("shadercache").join(format!("{slug}-programs.json"))).ok();
    if let Some(m) = &manifest {
        log::info(&format!("program manifest: {} bytes", m.len()));
    }
    let manifest_present = manifest.is_some();
    *MANIFEST.lock() = manifest;
    let own_path = data.join("shadercache").join(format!("{slug}.json"));
    let seed = List::load(&seed_path);
    let own = List::load(&own_path);
    // The engine builds the shipped list itself from the manifest; building it here as well
    // would compile everything twice at once. Only this device's extra programs are left.
    let lists: Vec<&List> = if manifest_present { vec![&own] } else { vec![&seed, &own] };
    let mut all = List::default();
    let mut known: HashSet<(String, String)> = seed.programs.iter().cloned().collect();
    if !manifest_present {
        known.clear();
    }
    for list in lists {
        all.shaders.extend(list.shaders.iter().map(|(k, v)| (k.clone(), v.clone())));
        for p in &list.programs {
            if known.insert(p.clone()) {
                all.programs.push(p.clone());
            }
        }
    }
    log::info(&format!(
        "shader warm-up: {} programs to build here ({} shipped, {} recorded here)",
        all.programs.len(),
        seed.programs.len(),
        own.programs.len()
    ));
    let known: HashSet<(String, String)> = seed.programs.iter().chain(&own.programs).cloned().collect();
    *REC.lock() = Some(Recorder { path: own_path, known, own, dirty: false });
    std::thread::Builder::new().name("shader-save".into()).spawn(save_loop).ok();
    if !all.programs.is_empty() {
        WARM_TOTAL.store(all.programs.len(), std::sync::atomic::Ordering::Relaxed);
        WARM_RUNNING.store(true, std::sync::atomic::Ordering::Relaxed);
        std::thread::Builder::new().name("shader-warm".into()).spawn(move || {
            warm(all);
            WARM_RUNNING.store(false, std::sync::atomic::Ordering::Relaxed);
        }).ok();
    }
}

/// The engine linked a program from these sources.
pub fn record(vs: &str, fs: &str) {
    let mut g = REC.lock();
    let Some(r) = g.as_mut() else { return };
    let key = (hash_of(vs), hash_of(fs));
    if !r.known.insert(key.clone()) {
        return;
    }
    r.own.shaders.entry(key.0.clone()).or_insert_with(|| vs.to_string());
    r.own.shaders.entry(key.1.clone()).or_insert_with(|| fs.to_string());
    r.own.programs.push(key);
    r.dirty = true;
}

/// Save this device's list now if it changed.
pub fn save_now() {
    let mut g = REC.lock();
    let Some(r) = g.as_mut() else { return };
    if r.dirty {
        r.dirty = false;
        if let Err(e) = r.own.save(&r.path) {
            log::error(&format!("cannot save {}: {e}", r.path.display()));
        }
    }
}

fn save_loop() {
    loop {
        std::thread::sleep(Duration::from_secs(5));
        save_now();
    }
}

/// Programs alive on the worker context: kept so ANGLE keeps their compiled Metal libraries.
static WARM: OnceLock<Mutex<Vec<u32>>> = OnceLock::new();

fn warm(list: List) {
    if let Err(e) = gl_ctx::make_worker_current() {
        log::error(&format!("shader warm-up unavailable: {e:#}"));
        return;
    }
    let t0 = Instant::now();
    // Without KHR_parallel_shader_compile everything is synchronous: all is always "done".
    let parallel = unsafe {
        let mut n = 0;
        gl::GetIntegerv(gl::NUM_EXTENSIONS, &mut n);
        (0..n.max(0) as u32).any(|i| {
            let p = gl::GetStringi(gl::EXTENSIONS, i);
            !p.is_null() && std::ffi::CStr::from_ptr(p as *const _).to_bytes() == b"GL_KHR_parallel_shader_compile"
        })
    };
    const COMPLETION_STATUS_KHR: u32 = 0x91B1;
    let done = |status: unsafe fn(u32, u32, *mut i32), obj: u32| {
        if !parallel {
            return true;
        }
        let mut v = 0;
        unsafe { status(obj, COMPLETION_STATUS_KHR, &mut v) };
        v != 0
    };
    unsafe fn program_iv(p: u32, pname: u32, v: *mut i32) {
        gl::GetProgramiv(p, pname, v)
    }
    unsafe fn shader_iv(s: u32, pname: u32, v: *mut i32) {
        gl::GetShaderiv(s, pname, v)
    }

    // Compile each source once; ANGLE translates in its worker pool.
    let mut shaders: HashMap<&str, u32> = HashMap::new();
    for (v, f) in &list.programs {
        for (hash, ty) in [(v, gl::VERTEX_SHADER), (f, gl::FRAGMENT_SHADER)] {
            if shaders.contains_key(hash.as_str()) {
                continue;
            }
            let Some(src) = list.shaders.get(hash) else { continue };
            let Ok(c) = std::ffi::CString::new(src.as_str()) else { continue };
            unsafe {
                let s = gl::CreateShader(ty);
                gl::ShaderSource(s, 1, &c.as_ptr(), std::ptr::null());
                gl::CompileShader(s);
                shaders.insert(hash.as_str(), s);
            }
        }
    }
    // Link without waiting: a link that has to wait for its shaders holds ANGLE's lock and
    // would stall the engine's own GL calls, so only link pairs whose shaders are done.
    let mut pending: Vec<(u32, u32)> = list
        .programs
        .iter()
        .filter_map(|(v, f)| Some((*shaders.get(v.as_str())?, *shaders.get(f.as_str())?)))
        .collect();
    let mut linking: Vec<u32> = Vec::new();
    let mut linked = 0usize;
    while !pending.is_empty() || !linking.is_empty() {
        let mut progressed = false;
        let mut i = 0;
        while i < pending.len() && linking.len() < 16 {
            let (vs, fs) = pending[i];
            if done(shader_iv, vs) && done(shader_iv, fs) {
                unsafe {
                    let p = gl::CreateProgram();
                    gl::AttachShader(p, vs);
                    gl::AttachShader(p, fs);
                    gl::LinkProgram(p);
                    linking.push(p);
                }
                pending.swap_remove(i);
                progressed = true;
            } else {
                i += 1;
            }
        }
        linking.retain(|&p| {
            if done(program_iv, p) {
                WARM.get_or_init(Default::default).lock().push(p);
                linked += 1;
                WARM_BUILT.store(linked, std::sync::atomic::Ordering::Relaxed);
                progressed = true;
                false
            } else {
                true
            }
        });
        if !progressed {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    let failed = WARM
        .get()
        .map(|w| {
            w.lock()
                .iter()
                .filter(|&&p| {
                    let mut ok = 0;
                    unsafe { gl::GetProgramiv(p, gl::LINK_STATUS, &mut ok) };
                    ok == 0
                })
                .count()
        })
        .unwrap_or(0);
    unsafe {
        for s in shaders.values() {
            gl::DeleteShader(*s);
        }
        gl::Flush();
    }
    log::info(&format!(
        "shader warm-up: {linked} programs built in {:.1} s ({failed} failed)",
        t0.elapsed().as_secs_f32()
    ));
}
