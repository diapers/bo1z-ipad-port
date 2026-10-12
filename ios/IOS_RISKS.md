# iOS risks: wasmtime, JIT and memory (iPad Air M1, iOS 17.0, TrollStore)

Every claim below has a source. Items marked *unverified* still need a test on the device.

## 1. Installation and entitlements

| Claim | Verdict | Source |
|---|---|---|
| "TrollStore supports arbitrary entitlements, including `dynamic-codesigning`" | **False for this device.** TrollStore does preserve arbitrary entitlements that a binary was fake-signed with. Three are banned on iOS 15+ with A12 or newer: `com.apple.private.cs.debugger`, `dynamic-codesigning` and `com.apple.private.skip-library-validation`. "Apps signed with them will crash on launch." The M1 is newer than A12. | [TrollStore README](https://github.com/opa334/TrollStore) |
| TrollStore JIT route | Sign with `get-task-allow`. TrollStore 2.0.12+ then shows "Open with JIT" and accepts `apple-magnifier://enable-jit?bundle-id=<id>`, which needs "URL Scheme Enabled" in TrollStore settings. The root helper `ptrace(PT_ATTACHEXC)`/`PT_DETACH`es the process, which leaves `CS_DEBUGGED` set. This has to happen **on every launch**. | [2.0.12 release notes](https://github.com/opa334/TrollStore/releases/tag/2.0.12), [README URL scheme](https://github.com/opa334/TrollStore), [RootHelper/jit.m](https://git.bughz.com/bughz/TrollStore/blame/commit/5b467392e50a3871109b47368cb4c77f95518fe0/RootHelper/jit.m) |
| iOS version | TrollStore supports 14.0b2–16.6.1, 16.7 RC and **17.0**. "17.0.1+ will NEVER be supported." | TrollStore README |
| `increased-memory-limit` / `extended-virtual-addressing` | Both are ordinary `com.apple.developer.*` entitlements, so the CoreTrust bypass grants them. (BO1Z-IPAD-PORT-PLAN.md says "TrollStore alone does not" give the memory entitlement. That conflicts with the README.) EVA exists on iOS 14.0+, and increased-memory-limit on 15.0+ ("only available on some device models; call `os_proc_available_memory`"). | [EVA](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.kernel.extended-virtual-addressing), [memory limit](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.kernel.increased-memory-limit) |
| VA without EVA | About **7.375 GiB** usable on devices with more than 3 GiB RAM. With the EVA "jumbo" map it is the full 64 GB. | [alwaysprocessing.blog: Size Matters](https://alwaysprocessing.blog/2022/02/20/size-matters) |
| Debugger JIT on this chip | The M1 is A14-generation, so it **predates SPTM/TXM** (A15/M2+). On TXM devices, "the debugger flag alone is not enough" and a per-region debugger handshake is required. On the M1, CS_DEBUGGED plus W^X flipping is the classic working method. | [SPTM & TXM](https://tin-z.github.io/ios-exploit-starterpack/en/sptm-txm/), [Play--CodeGen #34](https://github.com/jpd002/Play--CodeGen/pull/34), [Dolphin iOS PR #8492](https://github.com/dolphin-emu/dolphin/pull/8492) ("memory cannot be writable and executable at the same time … W^X exclusivity") |

## 2. wasmtime on `aarch64-apple-ios`

- **Support tier:** `aarch64-apple-ios` is **Tier 3** in wasmtime, which means no CI
  guarantee and no full-time maintainer. Threads/shared memory are Tier 2.
  ([stability tiers](https://docs.wasmtime.dev/stability-tiers.html))
- **It compiles.** `macos-spike.yml` already cross-compiles wasmtime 48 for
  `aarch64-apple-ios`. Muguet ships wasmtime 49 on iOS
  ([Cargo.toml](https://github.com/c22dev/muguet/blob/main/Cargo.toml)). Old issue
  [#5754](https://github.com/bytecodealliance/wasmtime/issues/5754) ("iOS doesn't allow JIT
  engines") was closed without an iOS fix.
- **Cranelift on iOS:** the `cranelift` feature builds for the target because
  `has_host_compiler_backend` covers `aarch64`
  ([build.rs](https://github.com/bytecodealliance/wasmtime/blob/main/crates/wasmtime/build.rs)).
  Running it on the device needs executable memory, so it needs CS_DEBUGGED (section 1).
- **Executable memory:** wasmtime does **not** use `MAP_JIT` or
  `pthread_jit_write_protect_np`. It writes code into RW pages and then `mprotect`s them
  to R+X ([mmap.rs](https://github.com/bytecodealliance/wasmtime/blob/main/crates/wasmtime/src/runtime/vm/sys/unix/mmap.rs)).
  That is exactly the W^X pattern that iOS accepts under CS_DEBUGGED, and it needs **no**
  JIT write-protect toggling in the host. (`MAP_JIT` RWX on iOS needs `dynamic-codesigning`,
  which is banned.) Icache flushing on `aarch64` + `target_vendor="apple"` is handled by
  `wasmtime-jit-icache-coherence`
  ([libc.rs](https://github.com/bytecodealliance/wasmtime/blob/main/crates/jit-icache-coherence/src/libc.rs)).
  *Unverified:* that `mprotect(RX)` succeeds on iOS 17.0 + M1 once CS_DEBUGGED is set.
  **Startup self-test:** map a 16 KiB page RW, write `ret` (`0xd65f03c0`), `mprotect` it
  to RX, call it, and log the result before creating the Engine.
- **Traps:** on Apple targets wasmtime handles faults with **Mach exception ports**,
  registered per thread with `thread_set_exception_ports` (so crash reporters still get
  task-level exceptions). It does not use POSIX signals there
  ([machports.rs](https://github.com/bytecodealliance/wasmtime/blob/main/crates/wasmtime/src/runtime/vm/sys/unix/machports.rs)).
  This applies to iOS as well (`target_vendor = "apple"`). If lldb is attached, it stops
  on guard-page `EXC_BAD_ACCESS`; continue past it. TrollStore detaches after setting
  CS_DEBUGGED, so this does not affect normal runs.
- **Other runtimes:** swift-wasmtime **forces Pulley** (the interpreter, no exec memory) on
  iOS ([swift-wasmtime](https://github.com/OpenCow42/swift-wasmtime)). This is the slow
  fallback if neither JIT nor AOT works.

## 3. Two execution strategies. Pick one per build.

**A. JIT on device** (current `host/Cargo.toml`: `cranelift`, `cache`)
- Needs `get-task-allow` (included) and TrollStore enable-jit on every launch. Gate it in
  code with `csops(getpid(), 0 /*CS_OPS_STATUS*/, &flags, 4)` and `flags & 0x10000000`
  (`CS_DEBUGGED`). If the flag is missing, open the URL scheme instead of crashing inside
  Cranelift.
- Compiling a 9.6 MB module on the device takes time and has a memory peak. Point
  wasmtime's cache at `$HOME/Library/Caches`. Loading from the cache still needs JIT,
  because the code goes through mmap + `mprotect(RX)`.

**B. AOT embedded in the signed binary** (Muguet's method; **no JIT, no URL dance**)
- Muguet precompiles with `config.target("aarch64-apple-ios")` and
  `cranelift_flag_enable("has_lse")`. It embeds the `.cwasm` with
  `global_asm!(".section __GAME,__code,regular,pure_instructions" / ".p2align 14" / ".incbin …")`,
  links with `-Wl,-segprot,__GAME,rx,rx`, sets `config.with_custom_code_memory(SignedCode)`
  where `publish_executable` / `unpublish_executable` are no-ops, and calls
  `Module::deserialize_raw` on that segment
  ([main.rs](https://github.com/c22dev/muguet/blob/main/src/main.rs), [build.rs](https://github.com/c22dev/muguet/blob/main/build.rs)).
  The code pages are covered by the app's (fake) signature, so iOS executes them without
  CS_DEBUGGED.
- Requirements:
  - The precompile Engine `Config` must match the runtime Config exactly: memory
    reservation and guard sizes, `signals_based_traps`, wasm features, Cranelift flags,
    and the same wasmtime version. Deserialization rejects any mismatch.
  - CI has to run the precompile step on macOS, which needs the game wasm in CI. It is
    downloadable from vel.gg, as `macos-spike.yml` does.
  - It depends on the open spike question of whether a module with **shared** memory
    serializes (`BO1Z-IPAD-PORT-PLAN.md` §8).
- Recommendation: **ship B if the spike passes, keep A as the dev/fallback path.** B
  removes the per-launch TrollStore round-trip and the unverified `mprotect` risk.

## 4. Recommended `wasmtime::Config` on iOS (2 GiB shared memory, min = max = 32767 pages)

```rust
cfg.wasm_threads(true).shared_memory(true);
cfg.memory_may_move(false);              // shared memories never move anyway
// With extended-virtual-addressing (the expected case):
cfg.memory_reservation(4 << 30);         // 4 GiB: Cranelift can drop 32-bit bounds checks
cfg.memory_guard_size(32 << 20);         // default 32 MiB guard
cfg.guard_before_linear_memory(false);   // save VA; it is defense in depth only
cfg.signals_based_traps(true);           // Mach-port traps (default)
cfg.max_wasm_stack(4 << 20);             // and run wasm threads with ≥ 16 MiB native stacks
```
- One linear memory then uses about 4.03 GiB of VA. That is fine with EVA (64 GB). Without
  EVA (about 7.4 GiB in total) it competes with ANGLE, the asset mmaps and thread stacks.
- **No-EVA fallback**: `memory_reservation(2 << 30)` (fits 32767 pages),
  `memory_guard_size(64 << 10)`. Cranelift then emits explicit bounds checks, which costs a
  few percent to low double digits. Muguet uses exactly `memory_guard_size(64 << 10)` with an
  iOS-specific reservation. Choose between the two at startup by trying
  `mmap(PROT_NONE, 4 GiB + 64 MiB)`. Under strategy B the choice is fixed at precompile time,
  so ship one variant and rely on EVA.
- `signals_based_traps(false)` is the escape hatch if Mach exception ports misbehave on
  iOS. In that mode every access is explicitly bounds-checked and guard pages are not
  relied on.
- Memory footprint: the 2 GiB counts toward jetsam only once it is touched. Log
  `os_proc_available_memory()`, and expect jetsam kills, not `mmap` failures, to be the
  failure mode under memory pressure.
- Thread stacks: the iOS main thread has 1 MiB and secondary threads default to 512 KiB
  ([Apple Threading Guide](https://developer.apple.com/library/archive/documentation/Cocoa/Conceptual/Multithreading/CreatingThreads/CreatingThreads.html)).
  Never run wasm on the winit main thread.

## 5. Other iOS-specific requirements for the host

- **UIScene:** winit 0.30 creates its window without a scene. Do not add
  `UIApplicationSceneManifest`. Building with the iOS 27 SDK *and* running on iOS 27 would
  require scenes and SIGTRAP at launch, which does not apply to this iOS 17 target
  ([bevy_ios_toolkit #42](https://github.com/marmikshah/bevy_ios_toolkit/issues/42)).
- **ANGLE window:** `EGLNativeWindowType` must be a `CALayer*`. If it is not a
  `CAMetalLayer`, ANGLE adds a sublayer and resizes it from bounds × contentsScale
  ([SurfaceMtl.mm](https://chromium.googlesource.com/angle/angle/+/refs/heads/main/src/libANGLE/renderer/metal/SurfaceMtl.mm)).
  libEGL loads `<exe dir>/Frameworks/libGLESv2.framework/libGLESv2` by itself on iOS
  ([system_utils.cpp](https://chromium.googlesource.com/angle/angle/+/refs/heads/main/src/common/system_utils.cpp),
  [system_utils_posix.cpp](https://chromium.googlesource.com/angle/angle/+/refs/heads/main/src/common/system_utils_posix.cpp)).
- **BC textures:** exposed only when `[MTLDevice supportsBCTextureCompression]` is true
  (iOS 16.4+) ([DisplayMtl.mm](https://chromium.googlesource.com/angle/angle/+/refs/heads/main/src/libANGLE/renderer/metal/DisplayMtl.mm),
  [mtl_format_map.json](https://chromium.googlesource.com/angle/angle/+/refs/heads/main/src/libANGLE/renderer/metal/mtl_format_map.json)).
  Keep a CPU decode fallback.
- **ANGLE source builds on iOS** are documented as "only from Chromium checkout"
  ([DevSetup.md](https://chromium.googlesource.com/angle/angle/+/refs/heads/main/doc/DevSetup.md)).
  Standalone builds work in practice ([ghera/ANGLE-iOS](https://github.com/ghera/ANGLE-iOS)),
  but that is why the prebuilt is the default.
- **Prebuilt supply chain:** the ANGLE binaries come from a third-party release (sha256-pinned).
  If the release disappears, `auto` mode falls back to the source build automatically.
