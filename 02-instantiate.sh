// 02-instantiate.sh — load the BO1Z wasm in wasmtime with everything enabled,
// compile it, and attempt instantiation with stubbed host imports.
// The failure/success pattern here is the real porting worklist.
set -uo pipefail
export PATH="$HOME/.cargo/bin:$PATH"

WASM="${1:-/mnt/c/Users/mirac/Desktop/dddd/bo1z-wasm/KisakBlack-web.wasm}"
PROJ="$HOME/bo1z-host"

echo "### wasm: $WASM"
ls -la "$WASM" || { echo "wasm not found"; exit 1; }

mkdir -p "$PROJ/src"
cd "$PROJ"

cat > Cargo.toml <<'EOF'
[package]
name = "bo1z-host"
version = "0.1.0"
edition = "2021"

[dependencies]
wasmtime = { version = "48.0.5", features = ["cranelift", "runtime", "threads", "gc", "gc-drc", "gc-null", "component-model"] }

[profile.release]
opt-level = 2
EOF

cat > src/main.rs <<'EOF'
use std::collections::BTreeMap;
use wasmtime::*;

fn main() {
    let path = std::env::args().nth(1).expect("usage: bo1z-host <file.wasm>");
    let bytes = std::fs::read(&path).expect("read wasm");
    println!("file: {path}  ({} bytes)", bytes.len());

    let mut cfg = Config::new();
    // Turn on everything the module might need. wasmtime rejects unknown
    // requirements at compile time, so this doubles as a feature probe.
    cfg.wasm_threads(true);
    cfg.wasm_simd(true);
    cfg.wasm_relaxed_simd(true);
    cfg.wasm_bulk_memory(true);
    cfg.wasm_reference_types(true);
    cfg.wasm_multi_value(true);
    cfg.wasm_multi_memory(true);
    cfg.wasm_memory64(false);
    cfg.wasm_exceptions(true);
    cfg.wasm_tail_call(true);
    cfg.wasm_extended_const(true);
    cfg.wasm_function_references(true);
    cfg.wasm_gc(true);
    cfg.wasm_custom_page_sizes(true);
    cfg.wasm_wide_arithmetic(true);
    cfg.cranelift_opt_level(OptLevel::Speed);
    // Wasm threads + shared memory need the pooling/async story off.
    cfg.async_support(false);

    let engine = Engine::new(&cfg).expect("engine");

    // ---- 1. compile ----
    let t0 = std::time::Instant::now();
    match Module::new(&engine, &bytes) {
        Ok(m) => {
            println!("COMPILE: OK in {:?}", t0.elapsed());
            println!("  exports: {}", m.exports().count());
            let mems: Vec<_> = m.exports().filter(|e| matches!(e.ty(), ExternType::Memory(_))).collect();
            println!("  exported memories: {}", mems.len());
            for e in m.exports() {
                if let ExternType::Memory(mt) = e.ty() {
                    println!("    {} shared={} min={} max={:?}",
                        e.name(), mt.is_shared(), mt.minimum(), mt.maximum());
                }
            }
        }
        Err(e) => {
            println!("COMPILE: FAILED");
            println!("  {e}");
            return;
        }
    }

    // ---- 2. import inventory + instantiate with stubs ----
    let module = Module::new(&engine, &bytes).unwrap();
    let mut linker: Linker<()> = Linker::new(&engine);

    // Build stub funcs for every imported function, all returning 0.
    let mut by_mod: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut n_func = 0usize;
    let mut mem_import: Option<(String, String, MemoryType)> = None;

    for imp in module.imports() {
        let m = imp.module().to_string();
        let n = imp.name().to_string();
        by_mod.entry(m.clone()).or_default().push(n.clone());
        match imp.ty() {
            ExternType::Func(ft) => {
                n_func += 1;
                let ty = ft.clone();
                // Stub: returns a zero of the first result type, ignores all args.
                linker.func_new(&m, &n, ty.clone(), move |_caller, _args, results| {
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
                }).ok();
            }
            ExternType::Memory(mt) => {
                mem_import = Some((m.clone(), n.clone(), mt.clone()));
            }
            ExternType::Global(gt) => {
                let _ = linker.define(&m, &n, Global::new(&mut Store::new(&engine, ()).unwrap(), Val::I32(0)));
                let _ = gt;
            }
            ExternType::Table(_tt) => { /* left unimplemented on purpose */ }
            _ => {}
        }
    }

    println!("\nIMPORTS: {n_func} funcs across {} modules", by_mod.len());
    for (m, v) in &by_mod {
        println!("  {m}: {}", v.len());
    }

    if let Some((m, n, mt)) = &mem_import {
        println!("\nMEMORY IMPORT {m}.{n}: shared={} min={} max={:?}",
            mt.is_shared(), mt.minimum(), mt.maximum());
        let mem = Memory::new(&mut Store::new(&engine, ()).unwrap(), mt.clone());
        match mem {
            Ok(mem) => {
                let mut store = Store::new(&engine, ());
                if linker.define(&mut store, m, n, mem).is_ok() {
                    println!("  memory defined in linker: OK");
                } else {
                    println!("  memory define FAILED");
                }
            }
            Err(e) => println!("  Memory::new FAILED: {e}"),
        }
    }

    let mut store = Store::new(&engine, ());
    match linker.instantiate(&mut store, &module) {
        Ok(_inst) => println!("\nINSTANTIATE: OK (all imports satisfied)"),
        Err(e) => {
            println!("\nINSTANTIATE: FAILED (expected until stubs are correct)");
            let msg = e.to_string();
            for line in msg.lines().take(14) { println!("  {line}"); }
        }
    }
}
EOF

echo "### building (this may take a few minutes)"
cargo build --release 2>&1 | tail -25

echo
echo "### running"
./target/release/bo1z-host "$WASM"
