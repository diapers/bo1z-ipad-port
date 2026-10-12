//! BO1Z native host: runs the vel.gg/bo1z WebAssembly engine outside the browser.
//!
//! Desktop usage:
//!   bo1z_host [--map five|kino|...] [--game <dir with <slug>/ packs>] [--wasm <file>] [--egl <libEGL path>]
//! iOS: everything is found inside the app bundle; logs go to Documents/bo1z.log.

mod aot;
mod audio;
mod emval;
mod fs;
mod gl;
mod gl_ctx;
mod imports;
mod input;
mod log;
mod mem;
mod platform;
mod rt;
mod video;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};

/// (slug, zone) from maps.js; "five" is the site's first map and lives at the pack root.
const MAPS: &[(&str, &str)] = &[
    ("five", "zombie_pentagon"),
    ("kino", "zombie_theater"),
    ("riese", "zombie_cod5_factory"),
    ("nacht", "zombie_cod5_prototype"),
    ("verruckt", "zombie_cod5_asylum"),
    ("shinonuma", "zombie_cod5_sumpf"),
    ("ascension", "zombie_cosmodrome"),
    ("cotd", "zombie_coast"),
    ("shangrila", "zombie_temple"),
    ("moon", "zombie_moon"),
];

struct Options {
    map: String,
    game: PathBuf,
    wasm: PathBuf,
    egl: PathBuf,
    data: PathBuf,
    width: i32,
    height: i32,
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| ".".into())
}

fn first_existing(cands: &[PathBuf]) -> Option<PathBuf> {
    cands.iter().find(|p| p.exists()).cloned()
}

fn options() -> Result<Options> {
    let dir = exe_dir();
    let mut args = std::env::args().skip(1);
    let mut map = None;
    let mut game = None;
    let mut wasm = None;
    let mut egl = None;
    while let Some(a) = args.next() {
        let mut val = || args.next().ok_or_else(|| anyhow!("{a} needs a value"));
        match a.as_str() {
            "--map" => map = Some(val()?),
            "--game" => game = Some(PathBuf::from(val()?)),
            "--wasm" => wasm = Some(PathBuf::from(val()?)),
            "--egl" => egl = Some(PathBuf::from(val()?)),
            other => return Err(anyhow!("unknown argument {other}")),
        }
    }

    let data = data_dir();

    let game = game
        .or_else(|| first_existing(&[dir.join("game"), dir.join("../../../game")]))
        .ok_or_else(|| anyhow!("no game/ folder next to the executable (use --game)"))?;
    // Map: --map, else Documents/map.txt, else the first pack present.
    let map = map
        .or_else(|| std::fs::read_to_string(data.join("map.txt")).ok().map(|s| s.trim().to_string()))
        .filter(|m| game.join(m).is_dir())
        .or_else(|| MAPS.iter().map(|m| m.0.to_string()).find(|m| game.join(m).is_dir()))
        .ok_or_else(|| anyhow!("no map packs in {}", game.display()))?;
    let wasm = wasm
        .or_else(|| {
            first_existing(&[
                dir.join("KisakBlack-native.wasm"),
                dir.join("KisakBlack-web.cwasm"),
                dir.join("KisakBlack-web.wasm"),
                dir.join("../../../bo1z-wasm/KisakBlack-web.wasm"),
            ])
        })
        .ok_or_else(|| anyhow!("KisakBlack-web.wasm not found (use --wasm)"))?;
    #[cfg(target_os = "ios")]
    let egl_default = dir.join("Frameworks/libEGL.framework/libEGL");
    #[cfg(windows)]
    let egl_default = dir.join("libEGL.dll");
    #[cfg(not(any(windows, target_os = "ios")))]
    let egl_default = PathBuf::from("libEGL.so");
    Ok(Options { map, game, wasm, egl: egl.unwrap_or(egl_default), data, width: 1600, height: 900 })
}

/// engine.js fiveArguments + the settings start() splices in, for the WebGL2 route with
/// all rendering on the main pthread (the Firefox route: r_smp_backend 0).
fn engine_args(o: &Options, zone: &str, opfs_root: &str) -> Vec<String> {
    let mut a: Vec<String> = [
        "+set", "fs_b", opfs_root,
        "+set", "fs_h", "/opfs/user",
        "+set", "kisak_zombies", "1",
        "+set", "zombiemode", "1",
        "+set", "kisak_testclient", "0",
        "+set", "kisak_scripterrors", "100",
        "+set", "r_fullscreen", "0",
        "+set", "r_aaSamples", "1",
        "+set", "r_clear", "0",
        "+set", "r_vsync", "0",
        "+set", "com_maxfps", "0",
        "+set", "snd_softwaremixer", "1",
        "+set", "snd_softwarewav", "0",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.extend(["+set".into(), "snd_softwaretable".into(), format!("{opfs_root}/web/sound")]);
    a.extend(["+set", "r_smp_backend", "0", "+set", "sensitivity", "5", "+set", "snd_menu_master", "0.8"].map(String::from));
    a.extend(["+set".into(), "r_customMode".into(), format!("{}x{}", o.width, o.height)]);
    a.extend(input::extra_binds());
    a.extend(["+devmap".into(), zone.into()]);
    a
}

/// Writable data: Documents on iOS (visible in the Files app), ./data next to the exe elsewhere.
fn data_dir() -> PathBuf {
    #[cfg(target_os = "ios")]
    {
        std::env::var("HOME").map(|h| PathBuf::from(h).join("Documents")).unwrap_or_else(|_| exe_dir().join("Documents"))
    }
    #[cfg(not(target_os = "ios"))]
    {
        exe_dir().join("data")
    }
}

/// `bo1z_host --precompile <in.wasm> <out.cwasm> [--target <triple>]`: AOT-compile the
/// engine with the exact runtime configuration (used by CI for the iOS build).
fn precompile(args: &[String]) -> Result<()> {
    let input = args.first().ok_or_else(|| anyhow!("--precompile <in.wasm> <out.cwasm> [--target T]"))?;
    let output = args.get(1).ok_or_else(|| anyhow!("--precompile <in.wasm> <out.cwasm> [--target T]"))?;
    let mut cfg = rt::engine_config();
    if let Some(i) = args.iter().position(|a| a == "--target") {
        let t = args.get(i + 1).ok_or_else(|| anyhow!("--target needs a triple"))?;
        cfg.target(t)?;
        eprintln!("target {t}");
    }
    let engine = wasmtime::Engine::new(&cfg)?;
    let t0 = std::time::Instant::now();
    let bytes = std::fs::read(input)?;
    let out = engine.precompile_module(&bytes)?;
    std::fs::write(output, &out)?;
    eprintln!("precompiled {input} -> {output}: {:.1} MB in {:.1?}", out.len() as f64 / 1e6, t0.elapsed());
    Ok(())
}

fn main() {
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("--precompile") {
        if let Err(e) = precompile(&argv[2..]) {
            eprintln!("precompile failed: {e:?}");
            std::process::exit(1);
        }
        return;
    }
    // Log before anything else can fail: on iOS the log file is the only output.
    log::init(&data_dir().join("bo1z.log"));
    log::info(&format!("bo1z_host {} starting; exe dir {}", env!("CARGO_PKG_VERSION"), exe_dir().display()));
    std::panic::set_hook(Box::new(|info| {
        log::error(&format!("PANIC: {info}\n{}", std::backtrace::Backtrace::force_capture()));
        log::flush();
    }));
    if let Err(e) = run() {
        log::error(&format!("{e:?}"));
        log::flush();
        eprintln!("error: {e:?}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let o = options()?;
    let (slug, zone) = MAPS.iter().find(|m| m.0 == o.map).copied().ok_or_else(|| anyhow!("unknown map {}", o.map))?;
    log::info(&format!("map {slug} ({zone}); packs {}; wasm {}", o.game.display(), o.wasm.display()));

    // maps.js: Five lives in OPFS "pack", every other map in "pack-<slug>".
    let opfs_dir = if slug == "five" { "pack".to_string() } else { format!("pack-{slug}") };
    let opfs_root = format!("/opfs/{opfs_dir}/files");
    let paths = rt::Paths {
        opfs_root: o.data.join("opfs"),
        mounts: vec![(format!("{opfs_dir}/files"), o.game.join(slug))],
    };
    // The module is compiled on the browser-main thread after the window is up, so the UI
    // thread finishes launching quickly (iOS kills apps that take too long to launch).
    let boot = rt::Boot { wasm: o.wasm.clone(), args: engine_args(&o, zone, &opfs_root), paths };
    let mut app = platform::App::new(o.egl.clone(), boot);
    platform::run(&mut app)
}
