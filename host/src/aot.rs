//! Ahead-of-time engine code, so iOS can run the game without JIT.
//!
//! CI precompiles KisakBlack-native.wasm for aarch64-apple-ios (`bo1z_host --precompile`)
//! and builds the app with BO1Z_CWASM pointing at the result. The assembler `.incbin`s the
//! bytes into an executable section of the binary (__TEXT on Apple: code-signed, mapped
//! read+execute), and wasmtime loads them in place (`Module::deserialize_raw`) with a
//! code-memory hook that has nothing to do: no JIT permission is ever needed.

use std::ptr::NonNull;
use std::sync::Arc;

use wasmtime::{CustomCodeMemory, Engine, Module, Result};

#[cfg(all(bo1z_aot, target_vendor = "apple"))]
core::arch::global_asm!(
    ".section __TEXT,__bo1z_engine,regular,pure_instructions",
    ".p2align 14",
    ".globl _bo1z_engine_start",
    "_bo1z_engine_start:",
    concat!(".incbin \"", env!("BO1Z_CWASM_PATH"), "\""),
    ".globl _bo1z_engine_end",
    "_bo1z_engine_end:",
    ".text",
);

#[cfg(all(bo1z_aot, windows))]
core::arch::global_asm!(
    ".section .text$bo1z,\"xr\"",
    ".p2align 12",
    ".globl bo1z_engine_start",
    "bo1z_engine_start:",
    concat!(".incbin \"", env!("BO1Z_CWASM_PATH"), "\""),
    ".globl bo1z_engine_end",
    "bo1z_engine_end:",
    ".text",
);

#[cfg(bo1z_aot)]
extern "C" {
    static bo1z_engine_start: u8;
    static bo1z_engine_end: u8;
}

/// The embedded precompiled engine, if this binary was built with one.
pub fn embedded() -> Option<&'static [u8]> {
    #[cfg(bo1z_aot)]
    unsafe {
        let start = std::ptr::addr_of!(bo1z_engine_start);
        let end = std::ptr::addr_of!(bo1z_engine_end);
        Some(std::slice::from_raw_parts(start, end as usize - start as usize))
    }
    #[cfg(not(bo1z_aot))]
    {
        None
    }
}

/// The embedded code already lives in executable, signed pages.
pub struct InPlaceCode;

impl CustomCodeMemory for InPlaceCode {
    fn required_alignment(&self) -> usize {
        1
    }
    fn publish_executable(&self, _ptr: *const u8, _len: usize) -> Result<()> {
        Ok(())
    }
    fn unpublish_executable(&self, _ptr: *const u8, _len: usize) -> Result<()> {
        Ok(())
    }
}

pub fn code_memory() -> Arc<dyn CustomCodeMemory> {
    Arc::new(InPlaceCode)
}

/// Load the embedded module in place.
pub fn load(engine: &Engine, code: &'static [u8]) -> Result<Module> {
    let ptr = NonNull::from(code);
    // SAFETY: `code` is 'static, immutable, and was produced by `Engine::precompile_module`
    // with this exact wasmtime version and engine configuration (same Cargo.lock, same
    // rt::engine_config), which wasmtime verifies from the artifact's metadata.
    unsafe { Module::deserialize_raw(engine, ptr) }
}
