//! ANGLE's program cache on disk (EGL_ANDROID_blob_cache): translated and linked programs
//! survive app restarts, so a map's shaders are only built from scratch once per install.
//! Blobs live in memory and in one file per key (u32 key length, key, value), written by a
//! background thread.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use crate::log;
use crate::shadercache::fnv;

static DIR: OnceLock<PathBuf> = OnceLock::new();
static MEM: Mutex<Option<HashMap<Vec<u8>, Arc<[u8]>>>> = Mutex::new(None);
static WRITER: OnceLock<Mutex<Sender<(Vec<u8>, Arc<[u8]>)>>> = OnceLock::new();

/// Where the cache lives; call before the EGL display is initialized.
pub fn configure(dir: PathBuf) {
    let _ = DIR.set(dir);
}

fn file(key: &[u8]) -> Option<PathBuf> {
    Some(DIR.get()?.join(format!("{:016x}-{}", fnv(key), key.len())))
}

extern "C" fn set_blob(key: *const c_void, key_size: isize, value: *const c_void, value_size: isize) {
    if key.is_null() || value.is_null() || key_size <= 0 || value_size < 0 {
        return;
    }
    let key = unsafe { std::slice::from_raw_parts(key as *const u8, key_size as usize) }.to_vec();
    let value: Arc<[u8]> = unsafe { std::slice::from_raw_parts(value as *const u8, value_size as usize) }.into();
    MEM.lock().get_or_insert_with(HashMap::new).insert(key.clone(), value.clone());
    if let Some(w) = WRITER.get() {
        let _ = w.lock().send((key, value));
    }
}

fn load(key: &[u8]) -> Option<Arc<[u8]>> {
    let data = std::fs::read(file(key)?).ok()?;
    let n = u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as usize;
    if data.get(4..4 + n)? != key {
        return None;
    }
    Some(data[4 + n..].into())
}

extern "C" fn get_blob(key: *const c_void, key_size: isize, value: *mut c_void, value_size: isize) -> isize {
    if key.is_null() || key_size <= 0 {
        return 0;
    }
    let key = unsafe { std::slice::from_raw_parts(key as *const u8, key_size as usize) };
    let hit = MEM.lock().get_or_insert_with(HashMap::new).get(key).cloned();
    let Some(blob) = hit.or_else(|| {
        let b = load(key)?;
        MEM.lock().get_or_insert_with(HashMap::new).insert(key.to_vec(), b.clone());
        Some(b)
    }) else {
        return 0;
    };
    // ANGLE asks for the size first (value_size 0), then for the data.
    if !value.is_null() && value_size >= blob.len() as isize {
        unsafe { std::ptr::copy_nonoverlapping(blob.as_ptr(), value as *mut u8, blob.len()) };
    }
    blob.len() as isize
}

type SetBlobCacheFuncs = unsafe extern "system" fn(
    *mut c_void,
    extern "C" fn(*const c_void, isize, *const c_void, isize),
    extern "C" fn(*const c_void, isize, *mut c_void, isize) -> isize,
);

/// Hand the callbacks to ANGLE (`set_funcs` is eglSetBlobCacheFuncsANDROID).
pub fn install(set_funcs: *const c_void, display: *mut c_void) {
    let Some(dir) = DIR.get().cloned() else { return };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::error(&format!("program cache {}: {e}", dir.display()));
        return;
    }
    let (tx, rx) = channel::<(Vec<u8>, Arc<[u8]>)>();
    let _ = WRITER.set(Mutex::new(tx));
    std::thread::Builder::new()
        .name("blob-cache".into())
        .spawn(move || {
            for (key, value) in rx {
                let Some(path) = file(&key) else { continue };
                let mut data = Vec::with_capacity(4 + key.len() + value.len());
                data.extend_from_slice(&(key.len() as u32).to_le_bytes());
                data.extend_from_slice(&key);
                data.extend_from_slice(&value);
                let tmp = path.with_extension("tmp");
                if std::fs::write(&tmp, &data).and_then(|_| std::fs::rename(&tmp, &path)).is_err() {
                    let _ = std::fs::remove_file(&tmp);
                }
            }
        })
        .ok();
    let entries = std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0);
    unsafe {
        let f: SetBlobCacheFuncs = std::mem::transmute(set_funcs);
        f(display, set_blob, get_blob);
    }
    log::info(&format!("program cache: {} ({entries} entries)", dir.display()));
}
