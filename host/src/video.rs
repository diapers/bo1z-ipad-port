//! Module.kbVideo replacement. Cinematics (loading-screen videos) are not decoded yet:
//! every video reports "ended" immediately, which the engine treats like a finished
//! playback, so loading proceeds without them.
//!
//! Shared words (Int32Array(shared, 10)): [0] frame seq lock, [1] current frame,
//! [2] time ms, [3] buffered-ahead ms (-1 = fully buffered), [4] state
//! (0 init, 1 playing, 2 ended, 3 error, 4 closed), [5] width, [6] height, [7] frame ptr.

use parking_lot::Mutex;

use crate::{log, mem};

static SLOTS: Mutex<Vec<(i32, u32)>> = Mutex::new(Vec::new());

fn word(shared: u32, i: u32) -> u32 {
    shared + 4 * i
}

fn mark_ended(shared: u32) {
    mem::set_i32(word(shared, 2), 0);
    mem::set_i32(word(shared, 3), -1);
    mem::set_i32(word(shared, 1), 1 << 30);
    mem::atomic_i32(word(shared, 4)).store(2, std::sync::atomic::Ordering::Release);
}

pub fn open(slot: i32, name: &str, shared: u32) {
    log::info(&format!("video open slot={slot} name={name} (skipped: reported as ended)"));
    let mut s = SLOTS.lock();
    s.retain(|x| x.0 != slot);
    s.push((slot, shared));
    if shared != 0 {
        mark_ended(shared);
    }
}

pub fn rewind(slot: i32) {
    if let Some(&(_, shared)) = SLOTS.lock().iter().find(|x| x.0 == slot) {
        if shared != 0 {
            mark_ended(shared);
        }
    }
}

pub fn close(slot: i32) {
    let mut s = SLOTS.lock();
    if let Some(pos) = s.iter().position(|x| x.0 == slot) {
        let (_, shared) = s.remove(pos);
        if shared != 0 {
            mem::atomic_i32(word(shared, 4)).store(4, std::sync::atomic::Ordering::Release);
        }
    }
}
