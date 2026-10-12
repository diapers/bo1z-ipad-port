//! WasmFS OPFS backend imports, served from the native filesystem.
//!
//! OPFS handles become virtual paths. The OPFS root is a writable directory; mounts
//! graft read-only trees (the bundled game packs) onto virtual paths, e.g.
//! "pack/files" -> <app bundle>/game/five, which is what `fs_b /opfs/pack/files` reads.
//! Async imports finish by calling emscripten_proxy_finish(ctx), as the JS does.

use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use wasmtime::Result;
use parking_lot::Mutex;
use wasmtime::*;

use crate::rt::{rt, Ctx};
use crate::{log, mem};

type Cx<'a> = Caller<'a, Ctx>;

const ENOENT_JS: i32 = -20; // NotFoundError
const TYPE_MISMATCH_FILE: i32 = -31;
const TYPE_MISMATCH_DIR: i32 = -54;
const EIO_JS: i32 = -29;

struct Access {
    file: File,
}

struct Blob {
    file: File,
    size: u64,
}

/// HandleAllocator: ids start at 1, freed ids are reused.
struct Handles<T> {
    slots: Vec<Option<T>>,
    free: Vec<usize>,
}

impl<T> Handles<T> {
    const fn new() -> Self {
        Self { slots: Vec::new(), free: Vec::new() }
    }
    fn alloc(&mut self, v: T) -> i32 {
        if self.slots.is_empty() {
            self.slots.push(None); // id 0 is never used
        }
        if let Some(i) = self.free.pop() {
            self.slots[i] = Some(v);
            i as i32
        } else {
            self.slots.push(Some(v));
            (self.slots.len() - 1) as i32
        }
    }
    fn get(&self, id: i32) -> Option<&T> {
        self.slots.get(id as usize).and_then(|s| s.as_ref())
    }
    fn get_mut(&mut self, id: i32) -> Option<&mut T> {
        self.slots.get_mut(id as usize).and_then(|s| s.as_mut())
    }
    fn release(&mut self, id: i32) {
        if let Some(s) = self.slots.get_mut(id as usize) {
            if s.take().is_some() {
                self.free.push(id as usize);
            }
        }
    }
}

struct State {
    dirs: Handles<Vec<String>>,
    files: Handles<Vec<String>>,
    access: Handles<Access>,
    blobs: Handles<Blob>,
}

static STATE: Mutex<State> = Mutex::new(State {
    dirs: Handles::new(),
    files: Handles::new(),
    access: Handles::new(),
    blobs: Handles::new(),
});

fn vjoin(v: &[String]) -> String {
    v.join("/")
}

/// Real path for a virtual path, honouring mounts (longest prefix wins).
fn real(v: &[String]) -> PathBuf {
    let p = rt();
    let joined = vjoin(v);
    let mut best: Option<(&str, &PathBuf)> = None;
    for (mv, mr) in &p.paths.mounts {
        let under = joined == *mv || joined.starts_with(&format!("{mv}/"));
        if under && best.map_or(true, |(b, _)| mv.len() > b.len()) {
            best = Some((mv.as_str(), mr));
        }
    }
    match best {
        Some((mv, mr)) => {
            let rest = joined[mv.len()..].trim_start_matches('/');
            if rest.is_empty() { mr.clone() } else { mr.join(rest) }
        }
        None => {
            if joined.is_empty() {
                p.paths.opfs_root.clone()
            } else {
                p.paths.opfs_root.join(&joined)
            }
        }
    }
}

/// Directories that exist only because a mount lives beneath them ("pack" for "pack/files").
fn synthetic_children(v: &[String]) -> Vec<String> {
    let prefix = vjoin(v);
    let mut out = Vec::new();
    for (mv, _) in &rt().paths.mounts {
        let rest = if prefix.is_empty() {
            Some(mv.as_str())
        } else {
            mv.strip_prefix(&format!("{prefix}/"))
        };
        if let Some(rest) = rest {
            let first = rest.split('/').next().unwrap_or("").to_string();
            if !first.is_empty() && !out.contains(&first) {
                out.push(first);
            }
        }
    }
    out
}

fn is_synthetic_dir(v: &[String]) -> bool {
    let joined = vjoin(v);
    rt().paths.mounts.iter().any(|(mv, _)| *mv == joined || mv.starts_with(&format!("{joined}/")))
}

enum Kind {
    File,
    Dir,
    Missing,
}

fn kind(v: &[String]) -> Kind {
    if is_synthetic_dir(v) {
        return Kind::Dir;
    }
    match std::fs::metadata(real(v)) {
        Ok(m) if m.is_dir() => Kind::Dir,
        Ok(_) => Kind::File,
        Err(_) => Kind::Missing,
    }
}

fn child(st: &State, parent: i32, name: &str) -> Option<Vec<String>> {
    let mut v = st.dirs.get(parent)?.clone();
    v.push(name.to_string());
    Some(v)
}

fn finish(cx: &mut Cx, ctx: i32) -> Result<()> {
    let ex = cx.data().ex().clone();
    ex.proxy_finish.call(&mut *cx, ctx)
}

#[cfg(unix)]
fn read_at(f: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    f.read_at(buf, pos)
}
#[cfg(windows)]
fn read_at(f: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    f.seek_read(buf, pos)
}
#[cfg(unix)]
fn write_at(f: &File, buf: &[u8], pos: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    f.write_at(buf, pos)
}
#[cfg(windows)]
fn write_at(f: &File, buf: &[u8], pos: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    f.seek_write(buf, pos)
}

/// Fill `buf` completely unless EOF (AccessHandle.read reads as much as available).
fn read_full(f: &File, buf: &mut [u8], pos: u64) -> std::io::Result<usize> {
    let mut done = 0;
    while done < buf.len() {
        match read_at(f, &mut buf[done..], pos + done as u64)? {
            0 => break,
            n => done += n,
        }
    }
    Ok(done)
}

pub fn register(l: &mut Linker<Ctx>) -> Result<()> {
    // --- preloaded files: none ------------------------------------------------------------
    l.func_wrap("env", "_wasmfs_get_num_preloaded_files", |_: Cx| -> i32 { 0 })?;
    l.func_wrap("env", "_wasmfs_get_num_preloaded_dirs", |_: Cx| -> i32 { 0 })?;
    l.func_wrap("env", "_wasmfs_get_preloaded_file_mode", |_: Cx, _i: i32| -> i32 { 0 })?;
    l.func_wrap("env", "_wasmfs_get_preloaded_file_size", |_: Cx, _i: i32| -> i32 { 0 })?;
    l.func_wrap("env", "_wasmfs_get_preloaded_child_path", |_: Cx, _i: i32, _b: i32| {})?;
    l.func_wrap("env", "_wasmfs_get_preloaded_parent_path", |_: Cx, _i: i32, _b: i32| {})?;
    l.func_wrap("env", "_wasmfs_get_preloaded_path_name", |_: Cx, _i: i32, _b: i32| {})?;
    l.func_wrap("env", "_wasmfs_copy_preloaded_file_data", |_: Cx, _i: i32, _b: i32| {})?;
    l.func_wrap("env", "_wasmfs_stdin_get_char", |_: Cx| -> i32 { -1 })?;

    // --- directories -----------------------------------------------------------------------
    l.func_wrap("env", "_wasmfs_opfs_init_root_directory", |mut cx: Cx, ctx: i32| -> Result<()> {
        {
            let mut st = STATE.lock();
            if st.dirs.slots.len() <= 1 {
                let _ = std::fs::create_dir_all(&rt().paths.opfs_root);
                let id = st.dirs.alloc(Vec::new());
                debug_assert_eq!(id, 1);
            }
        }
        finish(&mut cx, ctx)
    })?;
    l.func_wrap(
        "env",
        "_wasmfs_opfs_get_child",
        |mut cx: Cx, ctx: i32, parent: i32, name: i32, type_ptr: i32, id_ptr: i32| -> Result<()> {
            let name = mem::cstr(name as u32);
            let (ty, id) = {
                let mut st = STATE.lock();
                match child(&st, parent, &name) {
                    None => (1, EIO_JS),
                    Some(v) => match kind(&v) {
                        Kind::File => (1, st.files.alloc(v)),
                        Kind::Dir => (2, st.dirs.alloc(v)),
                        Kind::Missing => (1, ENOENT_JS),
                    },
                }
            };
            mem::set_i32(type_ptr as u32, ty);
            mem::set_i32(id_ptr as u32, id);
            finish(&mut cx, ctx)
        },
    )?;
    l.func_wrap(
        "env",
        "_wasmfs_opfs_get_entries",
        |mut cx: Cx, ctx: i32, dir: i32, entries: i32, err_ptr: i32| -> Result<()> {
            let v = STATE.lock().dirs.get(dir).cloned();
            let mut list: Vec<(String, i32)> = Vec::new();
            match v {
                None => mem::set_i32(err_ptr as u32, EIO_JS),
                Some(v) => {
                    for s in synthetic_children(&v) {
                        list.push((s, 2));
                    }
                    if let Ok(rd) = std::fs::read_dir(real(&v)) {
                        for e in rd.flatten() {
                            let n = e.file_name().to_string_lossy().into_owned();
                            if list.iter().any(|(x, _)| *x == n) {
                                continue;
                            }
                            let t = if e.file_type().map_or(false, |t| t.is_dir()) { 2 } else { 1 };
                            list.push((n, t));
                        }
                    }
                }
            }
            let ex = cx.data().ex().clone();
            for (name, ty) in list {
                let sp = ex.stack_current.call(&mut cx, ())?;
                let size = name.len() as i32 + 1;
                let p = ex.stack_alloc.call(&mut cx, size)?;
                mem::write_cstr(p as u32, &name, size as usize);
                ex.record_entry.call(&mut cx, (entries, p, ty))?;
                ex.stack_restore.call(&mut cx, sp)?;
            }
            finish(&mut cx, ctx)
        },
    )?;
    l.func_wrap(
        "env",
        "_wasmfs_opfs_insert_directory",
        |mut cx: Cx, ctx: i32, parent: i32, name: i32, id_ptr: i32| -> Result<()> {
            let name = mem::cstr(name as u32);
            let id = {
                let mut st = STATE.lock();
                match child(&st, parent, &name) {
                    None => EIO_JS,
                    Some(v) => match kind(&v) {
                        Kind::Dir => st.dirs.alloc(v),
                        Kind::File => TYPE_MISMATCH_DIR,
                        Kind::Missing => match std::fs::create_dir_all(real(&v)) {
                            Ok(()) => st.dirs.alloc(v),
                            Err(e) => {
                                log::error(&format!("mkdir {}: {e}", vjoin(&v)));
                                EIO_JS
                            }
                        },
                    },
                }
            };
            mem::set_i32(id_ptr as u32, id);
            finish(&mut cx, ctx)
        },
    )?;
    l.func_wrap(
        "env",
        "_wasmfs_opfs_insert_file",
        |mut cx: Cx, ctx: i32, parent: i32, name: i32, id_ptr: i32| -> Result<()> {
            let name = mem::cstr(name as u32);
            let id = {
                let mut st = STATE.lock();
                match child(&st, parent, &name) {
                    None => EIO_JS,
                    Some(v) => match kind(&v) {
                        Kind::File => st.files.alloc(v),
                        Kind::Dir => TYPE_MISMATCH_FILE,
                        Kind::Missing => {
                            let r = real(&v);
                            if let Some(p) = r.parent() {
                                let _ = std::fs::create_dir_all(p);
                            }
                            match OpenOptions::new().create(true).write(true).open(&r) {
                                Ok(_) => st.files.alloc(v),
                                Err(e) => {
                                    log::error(&format!("create {}: {e}", vjoin(&v)));
                                    EIO_JS
                                }
                            }
                        }
                    },
                }
            };
            mem::set_i32(id_ptr as u32, id);
            finish(&mut cx, ctx)
        },
    )?;
    l.func_wrap(
        "env",
        "_wasmfs_opfs_remove_child",
        |mut cx: Cx, ctx: i32, dir: i32, name: i32, err_ptr: i32| -> Result<()> {
            let name = mem::cstr(name as u32);
            let v = child(&STATE.lock(), dir, &name);
            let ok = match v {
                Some(v) => {
                    let r = real(&v);
                    if r.is_dir() { std::fs::remove_dir_all(&r).is_ok() } else { std::fs::remove_file(&r).is_ok() }
                }
                None => false,
            };
            if !ok {
                mem::set_i32(err_ptr as u32, EIO_JS);
            }
            finish(&mut cx, ctx)
        },
    )?;
    l.func_wrap(
        "env",
        "_wasmfs_opfs_move_file",
        |mut cx: Cx, ctx: i32, file: i32, new_parent: i32, name: i32, err_ptr: i32| -> Result<()> {
            let name = mem::cstr(name as u32);
            let ok = {
                let mut st = STATE.lock();
                let from = st.files.get(file).cloned();
                let to = child(&st, new_parent, &name);
                match (from, to) {
                    (Some(from), Some(to)) => {
                        let ok = std::fs::rename(real(&from), real(&to)).is_ok();
                        if ok {
                            if let Some(slot) = st.files.get_mut(file) {
                                *slot = to;
                            }
                        }
                        ok
                    }
                    _ => false,
                }
            };
            if !ok {
                mem::set_i32(err_ptr as u32, EIO_JS);
            }
            finish(&mut cx, ctx)
        },
    )?;
    l.func_wrap("env", "_wasmfs_opfs_free_directory", |_: Cx, id: i32| STATE.lock().dirs.release(id))?;
    l.func_wrap("env", "_wasmfs_opfs_free_file", |_: Cx, id: i32| STATE.lock().files.release(id))?;

    // --- files: size ------------------------------------------------------------------------
    l.func_wrap("env", "_wasmfs_opfs_get_size_file", |mut cx: Cx, ctx: i32, file: i32, size_ptr: i32| -> Result<()> {
        let v = STATE.lock().files.get(file).cloned();
        let size = v.and_then(|v| std::fs::metadata(real(&v)).ok()).map_or(EIO_JS as i64, |m| m.len() as i64);
        mem::set_i64(size_ptr as u32, size);
        finish(&mut cx, ctx)
    })?;
    l.func_wrap(
        "env",
        "_wasmfs_opfs_set_size_file",
        |mut cx: Cx, ctx: i32, file: i32, size: i64, err_ptr: i32| -> Result<()> {
            let v = STATE.lock().files.get(file).cloned();
            let ok = v
                .and_then(|v| OpenOptions::new().write(true).open(real(&v)).ok())
                .map_or(false, |f| f.set_len(size as u64).is_ok());
            if !ok {
                mem::set_i32(err_ptr as u32, EIO_JS);
            }
            finish(&mut cx, ctx)
        },
    )?;

    // --- access handles ---------------------------------------------------------------------
    l.func_wrap("env", "_wasmfs_opfs_open_access", |mut cx: Cx, ctx: i32, file: i32, id_ptr: i32| -> Result<()> {
        let id = {
            let mut st = STATE.lock();
            match st.files.get(file).cloned() {
                None => EIO_JS,
                Some(v) => {
                    let r = real(&v);
                    // Packs are read-only (app bundle); user files are read-write.
                    let f = OpenOptions::new().read(true).write(true).open(&r).or_else(|_| File::open(&r));
                    match f {
                        Ok(file) => st.access.alloc(Access { file }),
                        Err(e) => {
                            log::error(&format!("open {}: {e}", vjoin(&v)));
                            EIO_JS
                        }
                    }
                }
            }
        };
        mem::set_i32(id_ptr as u32, id);
        finish(&mut cx, ctx)
    })?;
    l.func_wrap("env", "_wasmfs_opfs_read_access", |_: Cx, id: i32, buf: i32, len: i32, pos: i64| -> i32 {
        let st = STATE.lock();
        let Some(a) = st.access.get(id) else { return EIO_JS };
        match read_full(&a.file, mem::bytes_mut(buf as u32, len as usize), pos as u64) {
            Ok(n) => n as i32,
            Err(_) => EIO_JS,
        }
    })?;
    l.func_wrap("env", "_wasmfs_opfs_write_access", |_: Cx, id: i32, buf: i32, len: i32, pos: i64| -> i32 {
        let st = STATE.lock();
        let Some(a) = st.access.get(id) else { return EIO_JS };
        let data = mem::bytes(buf as u32, len as usize);
        let mut done = 0usize;
        while done < data.len() {
            match write_at(&a.file, &data[done..], pos as u64 + done as u64) {
                Ok(0) | Err(_) => return if done > 0 { done as i32 } else { EIO_JS },
                Ok(n) => done += n,
            }
        }
        done as i32
    })?;
    l.func_wrap("env", "_wasmfs_opfs_get_size_access", |mut cx: Cx, ctx: i32, id: i32, size_ptr: i32| -> Result<()> {
        let size = STATE.lock().access.get(id).and_then(|a| a.file.metadata().ok()).map_or(EIO_JS as i64, |m| m.len() as i64);
        mem::set_i64(size_ptr as u32, size);
        finish(&mut cx, ctx)
    })?;
    l.func_wrap(
        "env",
        "_wasmfs_opfs_set_size_access",
        |mut cx: Cx, ctx: i32, id: i32, size: i64, err_ptr: i32| -> Result<()> {
            let ok = STATE.lock().access.get(id).map_or(false, |a| a.file.set_len(size as u64).is_ok());
            if !ok {
                mem::set_i32(err_ptr as u32, EIO_JS);
            }
            finish(&mut cx, ctx)
        },
    )?;
    l.func_wrap("env", "_wasmfs_opfs_flush_access", |mut cx: Cx, ctx: i32, _id: i32, _err: i32| -> Result<()> {
        finish(&mut cx, ctx)
    })?;
    l.func_wrap("env", "_wasmfs_opfs_close_access", |mut cx: Cx, ctx: i32, id: i32, _err: i32| -> Result<()> {
        STATE.lock().access.release(id);
        finish(&mut cx, ctx)
    })?;

    // --- blobs (read-only snapshots) --------------------------------------------------------
    l.func_wrap("env", "_wasmfs_opfs_open_blob", |mut cx: Cx, ctx: i32, file: i32, id_ptr: i32| -> Result<()> {
        let id = {
            let mut st = STATE.lock();
            match st.files.get(file).cloned() {
                None => EIO_JS,
                Some(v) => match File::open(real(&v)) {
                    Ok(f) => {
                        let size = f.metadata().map(|m| m.len()).unwrap_or(0);
                        st.blobs.alloc(Blob { file: f, size })
                    }
                    Err(_) => -2,
                },
            }
        };
        mem::set_i32(id_ptr as u32, id);
        finish(&mut cx, ctx)
    })?;
    l.func_wrap("env", "_wasmfs_opfs_get_size_blob", |_: Cx, id: i32| -> i64 {
        STATE.lock().blobs.get(id).map_or(0, |b| b.size as i64)
    })?;
    l.func_wrap(
        "env",
        "_wasmfs_opfs_read_blob",
        |mut cx: Cx, ctx: i32, id: i32, buf: i32, len: i32, pos: i64, nread_ptr: i32| -> Result<i32> {
            let n = {
                let st = STATE.lock();
                match st.blobs.get(id) {
                    None => EIO_JS,
                    Some(b) => {
                        let pos = pos as u64;
                        let len = (len as u64).min(b.size.saturating_sub(pos)) as usize;
                        read_full(&b.file, mem::bytes_mut(buf as u32, len), pos).map_or(EIO_JS, |n| n as i32)
                    }
                }
            };
            mem::set_i32(nread_ptr as u32, n);
            finish(&mut cx, ctx)?;
            Ok(0)
        },
    )?;
    l.func_wrap("env", "_wasmfs_opfs_close_blob", |_: Cx, id: i32| STATE.lock().blobs.release(id))?;
    Ok(())
}
