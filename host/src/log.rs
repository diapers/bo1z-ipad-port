//! Logging to stderr and a log file (on iOS the file is the only way to see output:
//! it lands in the app's Documents folder, visible in the Files app).

use std::io::Write;
use std::sync::OnceLock;
use std::time::Instant;

use parking_lot::Mutex;

static START: OnceLock<Instant> = OnceLock::new();
static FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);

pub fn init(path: &std::path::Path) {
    START.get_or_init(Instant::now);
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    // Keep the last run's log: after a crash, reopening the app would otherwise erase the
    // only record of it.
    if path.exists() {
        let _ = std::fs::rename(path, path.with_file_name("bo1z-previous.log"));
    }
    match std::fs::File::create(path) {
        Ok(f) => *FILE.lock() = Some(f),
        Err(e) => eprintln!("cannot open log {}: {e}", path.display()),
    }
}

pub fn uptime_ms() -> f64 {
    START.get_or_init(Instant::now).elapsed().as_secs_f64() * 1000.0
}

fn emit(tag: &str, msg: &str) {
    let line = format!("[{:9.3}] {tag} {msg}\n", uptime_ms() / 1000.0);
    let _ = std::io::stderr().write_all(line.as_bytes());
    if let Some(f) = FILE.lock().as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn info(msg: &str) {
    emit("host ", msg);
}

pub fn error(msg: &str) {
    emit("HOST!", msg);
}

/// Engine stdout/stderr (Module.print / printErr).
pub fn engine(msg: &str, err: bool) {
    crate::loading::engine_line(msg);
    // "Quit" in the pause menu: the engine falls back to its front end, which the web
    // build does not ship (black screen). Go back to the map select instead.
    if msg.contains("Com_LoadFrontEnd") {
        crate::platform::return_to_menu();
    }
    emit(if err { "eng! " } else { "eng  " }, msg.trim_end_matches('\n'));
}

pub fn flush() {
    if let Some(f) = FILE.lock().as_mut() {
        let _ = f.flush();
    }
}
