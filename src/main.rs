//! BO1Z native-port feasibility spike.
//!
//! Answers two questions that decide whether the whole port is viable:
//!
//!   Q1  Can wasmtime *serialize* (AOT-precompile) a module whose imports include a
//!       SHARED memory?  This matters because iOS forbids JIT for third-party apps,
//!       so Muguet's approach is "compile to native at build time, bake into the app".
//!       If shared memories cannot be serialized, that path is closed for BO1Z
//!       (which declares a shared memory of min=max=32767 pages = ~2 GiB).
//!
//!   Q2  Does the real 9.6 MB KisakBlack-web.wasm compile under wasm_threads +
//!       exceptions + bulk memory?
//!
//! Also reports the full import surface, which IS the porting worklist.

use std::collections::BTreeMap;
use wasmtime::*;

fn engine_config() -> Config {
    let mut cfg = Config::new();
    cfg.wasm_threads(true);
    // Needed explicitly: shared-memory creation is off by default even with threads on.
    cfg.shared_memory(true);
    cfg.wasm_simd(true);
    cfg.wasm_relaxed_simd(true);
    cfg.wasm_bulk_memory(true);
    cfg.wasm_reference_types(true);
    cfg.wasm_multi_value(true);
    cfg.wasm_multi_memory(true);
    cfg.wasm_exceptions(true);
    cfg.wasm_tail_call(true);
    cfg.wasm_extended_const(true);
    cfg.wasm_function_references(true);
    cfg.wasm_gc(true);
    cfg.cranelift_opt_level(OptLevel::Speed);
    cfg
}

fn main() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "artifacts/KisakBlack-web.wasm".to_string());
    println!("=== BO1Z native spike ===");
    println!("wasm: {path}");

    let bytes = std::fs::read(&path).expect("read wasm");
    println!("size: {} bytes ({:.2} MB)", bytes.len(), bytes.len() as f64 / 1e6);
    assert_eq!(&bytes[..4], b"\0asm", "not a wasm module");

    let cfg = engine_config();
    let engine = Engine::new(&cfg)?;

    // ---------------------------------------------------------------- Q2
    println!("\n--- Q2: compile with threads/exceptions enabled ---");
    let t0 = std::time::Instant::now();
    let module = match Module::new(&engine, &bytes) {
        Ok(m) => {
            println!("COMPILE: OK in {:?}", t0.elapsed());
            m
        }
        Err(e) => {
            println!("COMPILE: FAILED");
            for line in e.to_string().lines().take(12) {
                println!("  {line}");
            }
            println!("\nRESULT: Q2 = FAIL");
            return Ok(());
        }
    };
    println!("RESULT: Q2 = PASS");

    // -------------------------------------------------- import surface
    let mut by_mod: BTreeMap<String, usize> = BTreeMap::new();
    let mut shared_mem: Option<(String, String, MemoryType)> = None;
    let mut func_count = 0usize;

    for imp in module.imports() {
        *by_mod.entry(imp.module().to_string()).or_default() += 1;
        match imp.ty() {
            ExternType::Memory(mt) => {
                println!(
                    "\nMEMORY IMPORT {}.{}: shared={} min={} max={:?} ({:.1} MB)",
                    imp.module(),
                    imp.name(),
                    mt.is_shared(),
                    mt.minimum(),
                    mt.maximum(),
                    mt.minimum() as f64 * 65536.0 / 1e6
                );
                if mt.is_shared() {
                    shared_mem = Some((imp.module().to_string(), imp.name().to_string(), mt.clone()));
                }
            }
            ExternType::Func(_) => func_count += 1,
            other => println!("NON-FUNC IMPORT {}.{}: {other:?}", imp.module(), imp.name()),
        }
    }
    println!("\nimported funcs: {func_count}");
    for (m, n) in &by_mod {
        println!("  {m}: {n}");
    }
    println!("exports: {}", module.exports().count());

    // ---------------------------------------------------------------- Q1
    println!("\n--- Q1: can a shared-memory module be AOT-serialized? ---");
    let q1_synthetic = test_serialize_shared(&engine);
    let q1_real = test_serialize_real(&engine, &module, &bytes);
    println!("RESULT: Q1 synthetic-shared = {}", if q1_synthetic { "PASS" } else { "FAIL" });
    println!("RESULT: Q1 real-module      = {}", if q1_real { "PASS" } else { "FAIL" });

    // ---------------------------------------------------------------- Q3
    // Q1 serializes for the build host (macOS). The app needs code precompiled for
    // aarch64-apple-ios, which wasmtime checks against the loading engine at runtime.
    println!("\n--- Q3: AOT-precompile the real module for aarch64-apple-ios ---");
    let q3 = test_precompile_ios(&bytes);
    println!("RESULT: Q3 ios-precompile   = {}", if q3 { "PASS" } else { "FAIL" });

    // -------------------------------------------------- instantiate attempt
    println!("\n--- instantiation attempt with stubbed imports ---");
    let mut linker: Linker<()> = Linker::new(&engine);
    for imp in module.imports() {
        if let ExternType::Func(ft) = imp.ty() {
            let m = imp.module().to_string();
            let n = imp.name().to_string();
            let ty = ft.clone();
            let _ = linker.func_new(&m, &n, ty.clone(), move |_c, _a, results| {
                for (i, r) in results.iter_mut().enumerate() {
                    *r = match ty.results().nth(i) {
                        Some(ValType::I32) => Val::I32(0),
                        Some(ValType::I64) => Val::I64(0),
                        Some(ValType::F32) => Val::F32(0),
                        Some(ValType::F64) => Val::F64(0),
                        Some(ValType::V128) => Val::V128(0u128.into()),
                        _ => Val::I32(0),
                    };
                }
                Ok(())
            });
        }
    }
    let mut store = Store::new(&engine, ());
    if let Some((m, n, mt)) = &shared_mem {
        match SharedMemory::new(&engine, mt.clone()) {
            Ok(mem) => {
                if let Err(e) = linker.define(&mut store, m, n, mem) {
                    println!("  define shared memory FAILED: {e}");
                } else {
                    println!("  SharedMemory::new + define: OK");
                }
            }
            Err(e) => println!("  SharedMemory::new FAILED: {e}"),
        }
    }
    match linker.instantiate(&mut store, &module) {
        Ok(_) => println!("INSTANTIATE: OK (all imports satisfied by stubs)"),
        Err(e) => {
            println!("INSTANTIATE: FAILED (expected until real host fns exist)");
            for line in e.to_string().lines().take(8) {
                println!("  {line}");
            }
        }
    }

    println!("\n=== done ===");
    Ok(())
}

/// Tiny hand-built module importing a shared memory + one func, to isolate Q1.
fn test_serialize_shared(engine: &Engine) -> bool {
    // (module
    //   (import "env" "memory" (memory 1 2 shared))
    //   (func (export "noop")))
    let mut w: Vec<u8> = vec![0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

    // type section: one type, () -> ()
    w.extend_from_slice(&[0x01, 0x04, 0x01, 0x60, 0x00, 0x00]);

    // import section: env.memory (memory 1 2 shared), kind=2, flags=0x03
    let mut imp = vec![0x01u8];
    imp.push(0x03); // "env"
    imp.extend_from_slice(b"env");
    imp.push(0x06); // "memory"
    imp.extend_from_slice(b"memory");
    imp.push(0x02); // kind = memory
    imp.push(0x03); // limits flags: has-max | shared
    imp.push(0x01); // min 1
    imp.push(0x02); // max 2
    w.push(0x02);
    w.push(imp.len() as u8);
    w.extend_from_slice(&imp);

    // function section: one func of type 0
    w.extend_from_slice(&[0x03, 0x02, 0x01, 0x00]);

    // export section: "noop" func 0
    let mut ex = vec![0x01u8, 0x04];
    ex.extend_from_slice(b"noop");
    ex.push(0x00);
    ex.push(0x00);
    w.push(0x07);
    w.push(ex.len() as u8);
    w.extend_from_slice(&ex);

    // code section: one empty body
    w.extend_from_slice(&[0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b]);

    match Module::new(engine, &w) {
        Ok(m) => match m.serialize() {
            Ok(ser) => {
                println!("  synthetic shared-mem module serialized: {} bytes", ser.len());
                true
            }
            Err(e) => {
                println!("  synthetic serialize FAILED: {e}");
                false
            }
        },
        Err(e) => {
            println!("  synthetic module construction FAILED: {e}");
            false
        }
    }
}

/// Cross-compile the real module for iOS and write it out (Q3). The .cwasm is what
/// the app would bake in; it can only be *loaded* on the device itself.
fn test_precompile_ios(bytes: &[u8]) -> bool {
    let mut cfg = engine_config();
    if let Err(e) = cfg.target("aarch64-apple-ios") {
        println!("  set target FAILED: {e}");
        return false;
    }
    let engine = match Engine::new(&cfg) {
        Ok(e) => e,
        Err(e) => {
            println!("  iOS engine FAILED: {e}");
            return false;
        }
    };
    let t0 = std::time::Instant::now();
    match engine.precompile_module(bytes) {
        Ok(cwasm) => {
            println!("  precompiled in {:?}: {:.2} MB", t0.elapsed(), cwasm.len() as f64 / 1e6);
            let out = "artifacts/KisakBlack-ios.cwasm";
            match std::fs::write(out, &cwasm) {
                Ok(()) => println!("  wrote {out}"),
                Err(e) => println!("  write {out} FAILED: {e}"),
            }
            true
        }
        Err(e) => {
            println!("  precompile FAILED");
            for line in e.to_string().lines().take(8) {
                println!("  {line}");
            }
            false
        }
    }
}

/// Does the real module serialize? (Q1, definitive form.)
fn test_serialize_real(engine: &Engine, module: &Module, _bytes: &[u8]) -> bool {
    match module.serialize() {
        Ok(ser) => {
            println!("  real module serialized: {:.2} MB", ser.len() as f64 / 1e6);
            // Also verify it can be deserialized on a fresh engine — that is what
            // an iOS app would actually do at launch.
            let engine2 = Engine::new(&engine_config()).unwrap();
            match unsafe { Module::deserialize(&engine2, &ser) } {
                Ok(_) => {
                    println!("  deserialize on fresh engine: OK");
                    true
                }
                Err(e) => {
                    println!("  deserialize FAILED: {e}");
                    false
                }
            }
        }
        Err(e) => {
            println!("  real serialize FAILED: {e}");
            false
        }
    }
}
