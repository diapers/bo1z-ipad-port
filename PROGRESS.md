# BO1Z → iPad IPA: progress log

Goal: a playable unsigned IPA (TrollStore, iPad Air M1, iOS 17) with Xbox controller support,
game packs bundled inside. Keep this file current; it is the hand-off document.

## Architecture (decided)
- Run the site's `KisakBlack-web.wasm` natively with **wasmtime** (Rust host in `host/`).
- Renderer: the engine's **WebGL2 path** (not WebGPU). Its gl* imports + `ExecuteGLPackets`
  map ~1:1 onto **OpenGL ES 3 via ANGLE** (D3D11 on Windows for dev, Metal on iPad).
  GLSL shaders come from `web/shaders/shaders.pack` (already mirrored).
- Threads: each Emscripten pthread = OS thread + its own wasmtime instance, one shared 2 GiB memory.
- Files: OPFS backend → real dirs. `/opfs/pack/files` (Five) or `/opfs/pack-<slug>/files` mounted
  read-only on `<bundle>/game/<slug>`; `/opfs/user` writable in Documents.
- Audio: engine mixer ring (KB_AudioRing) read directly by cpal callback; wake via futex notify.
- Input: controller → engine key/mouse events (KB_Input). Engine's built-in PC binds used;
  `+bind F7 +usereload`, `+bind F8 +speed_throw` added at startup for X / LT.
  Menus (KB_UIState()&1): stick drives absolute cursor, A clicks. First A/Start sends KB_Input(7,1,0)
  (intro release).
- Videos (loading cinematics): stubbed as "ended" immediately.
- Launch args: copied from engine.js `fiveArguments` + `r_smp_backend 0` (Firefox route: all GL on
  main pthread, swap after each main-loop iteration).

## Reference material
- `web-src/pretty/*.js` — de-minified site code (KisakBlack-web.mjs is the spec for every import).
- `web-src/imports.tsv`, `web-src/exports.tsv` — wasm signatures (`tools/wasm-sigs.mjs`).
- `pack-five/`, `pack-kino/` — verified game data. `inject-assets.mjs` puts them in an IPA.

## Status
| Piece | File | State |
|---|---|---|
| shared memory helpers | host/src/mem.rs | written |
| pthreads/mailbox/main loop/boot | host/src/rt.rs | written |
| misc/time/wasi/EM_ASM/proxy imports | host/src/imports.rs | written |
| OPFS file system | host/src/fs.rs | written |
| embind/emval (logging object model) | host/src/emval.rs | written |
| GL imports + packet interpreter | host/src/gl.rs | written |
| EGL/ANGLE context | host/src/gl_ctx.rs | written |
| audio | host/src/audio.rs | written |
| controller mapping (gilrs / GameController) | host/src/input.rs | written |
| window/event loop | host/src/platform.rs | written |
| entry/args | host/src/main.rs | written |
| first compile on Windows | — | DONE (wasmtime 49; uses wasmtime::Error not anyhow in host fns) |
| run on Windows (ANGLE DLLs from an Electron app, e.g. LM Studio) | — | todo |
| iOS packaging + ANGLE-for-iOS CI | ios/, .github/workflows/ios-build.yml | written by agent, untested (see ios/README.md, ios/IOS_RISKS.md) |
| browser-main role on its own 32 MiB-stack thread (UI thread only forwards input) | rt.rs/platform.rs | DONE |
| git repo initialised, first commit cf42c34 | — | DONE (no remote yet) |
| GitHub repo diapers/bo1z-ipad-port; ios-build CI green | — | DONE 2026-10-12 |
| IPA build 1 (fb10f7b) | build/BO1Z-five.ipa | crashed on open on iPad (no log collected yet) |
| IPA build 2 (3fc3037): log first, compile off UI thread, JIT check | build/BO1Z-five.ipa | waiting for user test + Documents/bo1z.log |

## Toolchain on this PC
Rust (rustup, stable MSVC) + VS 2022 C++ workload installed 2026-10-11.
Build: `cd host && cargo build --release`. Run needs `libEGL.dll` + `libGLESv2.dll` next to the exe.

## Open risks
- wasmtime on iOS needs JIT (dynamic-codesigning entitlement via TrollStore) or AOT precompile.
- 2 GiB shared memory on iOS: needs increased-memory-limit + extended-virtual-addressing.
- ANGLE for iOS must be built/fetched in CI.
- Unknown emval calls (logged at runtime as `emval: call ...`).

## Key findings log
- The wasm uses LEGACY wasm exceptions; wasmtime can't run them. Fix: Binaryen
  `npx --prefix tools/node wasm-opt bo1z-wasm/KisakBlack-web.wasm -g --enable-threads --enable-bulk-memory
  --enable-bulk-memory-opt --enable-exception-handling --enable-mutable-globals --enable-nontrapping-float-to-int
  --enable-sign-ext --enable-reference-types --enable-multivalue --enable-simd --enable-tail-call --enable-extended-const
  --translate-to-exnref -o bo1z-wasm/KisakBlack-native.wasm`   (do NOT use -all: emits compact-imports wasmtime can't parse)
  -> run the host with `--wasm KisakBlack-native.wasm`. CI must do the same step.
- Engine uses emval for JSON.parse of the shader bundle (emval.rs has a JSON object model now).
- TrollStore BANS dynamic-codesigning on A12+/M1 (crash on launch). iOS options: (a) AOT-precompile the
  module for aarch64-apple-ios and load it without JIT (Muguet approach), or (b) TrollStore "Open with JIT"
  per launch (get-task-allow). TrollStore needs iOS 17.0 exactly.
- On iOS pass winit's `ui_view.layer` to ANGLE (it adds its own CAMetalLayer and tracks size).
- Dev run: `cd host/target/debug && ./bo1z_host.exe --map five --wasm KisakBlack-native.wasm`
  (game/five and game/kino are junctions to pack-*/files; libEGL/libGLESv2/d3dcompiler_47 copied from LM Studio).

## Next steps (in order)
1. User creates an empty PUBLIC GitHub repo; `git remote add origin <url> && git push -u origin main`.
2. Actions -> ios-build runs (first run fetches ANGLE). Fix any aarch64-apple-ios compile errors
   (iOS-only code paths: input.rs GameController, platform.rs native_window, cpal CoreAudio).
3. Download artifact BO1Z.ipa, then on this PC:
   `node inject-assets.mjs --ipa=BO1Z.ipa --maps=five` (adds game/five + KisakBlack-native.wasm).
4. TrollStore install; launch with "Open with JIT" (long-press). Log: Files app -> BO1Z -> bo1z.log.
5. Later: AOT precompile (no per-launch JIT), loading-screen video decode, Kino/other maps picker.
Note: iOS only supports Xbox One S / Series pads over Bluetooth (not Xbox 360 pads).
