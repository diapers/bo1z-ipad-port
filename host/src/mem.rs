//! Raw access to the single shared wasm memory.
//!
//! The module imports one fixed 2 GiB shared memory (min = max = 32767 pages), so the
//! host can hold one base pointer for the whole process. All threads see the same
//! bytes; races are the engine's business, exactly as with a SharedArrayBuffer.

use std::sync::atomic::{AtomicI32, AtomicPtr, AtomicU32, AtomicUsize, Ordering};

static BASE: AtomicPtr<u8> = AtomicPtr::new(std::ptr::null_mut());
static LEN: AtomicUsize = AtomicUsize::new(0);

pub fn init(base: *mut u8, len: usize) {
    BASE.store(base, Ordering::Release);
    LEN.store(len, Ordering::Release);
}

#[inline]
pub fn base() -> *mut u8 {
    BASE.load(Ordering::Relaxed)
}

#[inline]
pub fn len() -> usize {
    LEN.load(Ordering::Relaxed)
}

#[inline]
fn check(addr: u32, n: usize) {
    debug_assert!(addr as usize + n <= len(), "wasm access out of bounds: {addr:#x}+{n}");
}

/// Host pointer for a wasm address.
#[inline]
pub fn ptr(addr: u32) -> *mut u8 {
    unsafe { base().add(addr as usize) }
}

#[inline]
pub fn ptr_or_null(addr: u32) -> *const std::ffi::c_void {
    if addr == 0 {
        std::ptr::null()
    } else {
        ptr(addr) as *const _
    }
}

#[inline]
pub fn u8(addr: u32) -> u8 {
    check(addr, 1);
    unsafe { *ptr(addr) }
}
#[inline]
pub fn set_u8(addr: u32, v: u8) {
    check(addr, 1);
    unsafe { *ptr(addr) = v }
}
#[inline]
pub fn u32(addr: u32) -> u32 {
    check(addr, 4);
    unsafe { (ptr(addr) as *const u32).read_unaligned() }
}
#[inline]
pub fn i32(addr: u32) -> i32 {
    u32(addr) as i32
}
#[inline]
pub fn f32(addr: u32) -> f32 {
    f32::from_bits(u32(addr))
}
#[inline]
pub fn set_u32(addr: u32, v: u32) {
    check(addr, 4);
    unsafe { (ptr(addr) as *mut u32).write_unaligned(v) }
}
#[inline]
pub fn set_i32(addr: u32, v: i32) {
    set_u32(addr, v as u32)
}
#[inline]
pub fn set_f32(addr: u32, v: f32) {
    set_u32(addr, v.to_bits())
}
#[inline]
pub fn u64(addr: u32) -> u64 {
    check(addr, 8);
    unsafe { (ptr(addr) as *const u64).read_unaligned() }
}
#[inline]
pub fn f64(addr: u32) -> f64 {
    f64::from_bits(u64(addr))
}
#[inline]
pub fn set_u64(addr: u32, v: u64) {
    check(addr, 8);
    unsafe { (ptr(addr) as *mut u64).write_unaligned(v) }
}
#[inline]
pub fn set_i64(addr: u32, v: i64) {
    set_u64(addr, v as u64)
}

/// Atomic view of an aligned 32-bit word (Atomics.load/store/add in the JS runtime).
#[inline]
pub fn atomic_i32(addr: u32) -> &'static AtomicI32 {
    check(addr, 4);
    debug_assert!(addr % 4 == 0);
    unsafe { &*(ptr(addr) as *const AtomicI32) }
}
#[inline]
pub fn atomic_u32(addr: u32) -> &'static AtomicU32 {
    check(addr, 4);
    unsafe { &*(ptr(addr) as *const AtomicU32) }
}

pub fn bytes(addr: u32, n: usize) -> &'static [u8] {
    check(addr, n);
    unsafe { std::slice::from_raw_parts(ptr(addr), n) }
}

pub fn bytes_mut(addr: u32, n: usize) -> &'static mut [u8] {
    check(addr, n);
    unsafe { std::slice::from_raw_parts_mut(ptr(addr), n) }
}

pub fn write(addr: u32, data: &[u8]) {
    bytes_mut(addr, data.len()).copy_from_slice(data);
}

/// UTF8ToString: NUL-terminated (or `max` bytes) string at `addr`; 0 -> "".
pub fn cstr(addr: u32) -> String {
    cstr_max(addr, usize::MAX)
}

pub fn cstr_max(addr: u32, max: usize) -> String {
    if addr == 0 {
        return String::new();
    }
    let mut n = 0usize;
    while n < max && u8(addr + n as u32) != 0 {
        n += 1;
    }
    String::from_utf8_lossy(bytes(addr, n)).into_owned()
}

/// stringToUTF8: writes at most `max` bytes including the NUL; returns bytes written
/// excluding the NUL.
pub fn write_cstr(addr: u32, s: &str, max: usize) -> usize {
    if max == 0 || addr == 0 {
        return 0;
    }
    let src = s.as_bytes();
    let mut n = src.len().min(max - 1);
    // Never split a UTF-8 sequence.
    while n > 0 && n < src.len() && (src[n] & 0xc0) == 0x80 {
        n -= 1;
    }
    write(addr, &src[..n]);
    set_u8(addr + n as u32, 0);
    n
}
