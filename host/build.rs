// BO1Z_CWASM=<path to a .cwasm precompiled for this target> embeds the engine's machine
// code into the executable (see src/aot.rs). Without it the engine is JIT-compiled at
// startup, which on iOS needs TrollStore's "Open with JIT".
fn main() {
    println!("cargo::rustc-check-cfg=cfg(bo1z_aot)");
    println!("cargo:rerun-if-env-changed=BO1Z_CWASM");
    if let Ok(p) = std::env::var("BO1Z_CWASM") {
        if !p.is_empty() {
            let p = std::fs::canonicalize(&p).unwrap_or_else(|e| panic!("BO1Z_CWASM={p}: {e}"));
            println!("cargo:rerun-if-changed={}", p.display());
            println!("cargo:rustc-cfg=bo1z_aot");
            // The path is spliced into an assembler string: plain forward slashes only.
            let s = p.display().to_string();
            let s = s.strip_prefix(r"\\?\").unwrap_or(&s).replace('\\', "/");
            println!("cargo:rustc-env=BO1Z_CWASM_PATH={s}");
        }
    }
}
