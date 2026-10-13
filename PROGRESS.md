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
- Game data: **merged store on E:** (see below). `inject-assets.mjs` reads it and puts maps in an IPA.

## Asset store (2026-10-12) — all 10 maps, deduplicated
```
E:\bo1z\store\<ab>\<sha256>   347 unique blobs, 6.61 GB — every byte stored once
E:\bo1z\maps\<slug>\...       10 maps, own engine path tree, HARD LINKS into the store
index/share-index.json        blob index: hash -> size -> referencing maps
```
582 path references collapse to 347 unique blobs (8.19 GB logical -> 6.61 GB physical,
1.58 GB saved). Each map keeps real engine paths; identical files are hard links, so peak
disk = unique bytes. `nlink` proves sharing (`iw_25.iwd` = 10 maps, `nlink=11`).

Rebuild/refresh: `node build-index.mjs` then `node build-store.mjs --root=E:/bo1z`.
Verify: `node verify-store.mjs E:/bo1z` (347 blobs + 582 links, all hashed).

**Do not key content by path.** 53 paths appear in several maps with *different* bytes
(`main/iw_01.iwd` has 10 distinct versions), so content is addressed by **sha256 only**.

**The old `pack-five/` / `pack-kino/` dirs on C: are gone** (deleted 2026-10-12 to free
C:, which was down to 3.7 GB). Everything now comes from the E: store. `host/target/debug/
game/{five,kino}` junctions were repointed to `E:\bo1z\maps\<slug>`, and `inject-assets.mjs`
resolves via `--store` (default `E:/bo1z/maps`) with a legacy `pack-<slug>/files` fallback.

Caveat: the store lives on E:, so builds depend on E: being present. If E: is unavailable,
re-fetch with `node build-store.mjs --root=<other drive>` (resumable, hash-verified).


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

## 2026-10-12: device crash diagnosed -> AOT
- iPad crash report: EXC_BAD_ACCESS / Instruction Abort executing JIT code (page max prot rw-);
  CS flags 0x22006305 lack CS_DEBUGGED -> JIT was not active. Everything before that worked
  (UIKit, window, ANGLE frameworks, module compile).
- Fix: AOT. `bo1z_host --precompile in.wasm out.cwasm --target aarch64-apple-ios`; build with
  BO1Z_CWASM=out.cwasm -> host/build.rs + src/aot.rs `.incbin` it into an executable section
  (__TEXT,__bo1z_engine on Apple) and load with Module::deserialize_raw + no-op CustomCodeMemory.
  Verified on Windows (.text$bo1z): "module ready in 3.7ms", game boots. COFF max alignment is
  8 KiB (`.p2align 16` crashes LLVM); Apple uses `.p2align 14`.
- CI (ios-build.yml) now fetches the wasm (sha256 pinned), translates with binaryen@132,
  precompiles, embeds. App no longer needs "Open with JIT".
- IPA build 3 (efe6ee6, CI run 38221436862): engine embedded (25.8 MB __TEXT,__bo1z_engine,
  align 2^14). build/BO1Z-five.ipa = 790 MB. Waiting for device test (normal tap, no JIT).
- Device test of build 3: AOT load OK (6.4 ms), ANGLE Metal on M1 OK, all WebGL2 exts OK; engine quit:
  "Could not find zone .../zone/english/code_pre_gfx_mp.ff" -- engine asks zone/common, pack has zone/Common.
- Build 4 (2081507, run 38222917146): case-insensitive fs lookup + r_customMode = surface size + 1x scale on iOS.

## 2026-10-12: build 4 device test -> build 5
Build 4 runs on the iPad (Five playable). Issues from the screen recording + user notes, and fixes:
- Blocky black/white texture corruption: native GLES leaves new textures undefined (WebGL zero-fills);
  on Metal that is garbage BC blocks. Fix: EGL_ROBUST_RESOURCE_INITIALIZATION_ANGLE (gl_ctx.rs).
- Controller was keyboard emulation (keyboard hints, menus stuck). The engine still has BO1's gamepad
  code; only the XInput poller is missing. input.rs now writes pad 0's GamePad struct (147054240,
  124 B: +0 enabled, +2 digitals (XInput bits), +4 last, +8 LT/RT, +24 LX LY RX RY) and the physical
  axes (13382032, i32 x6 /65535: RX RY LX LY RT LT), sends BUTTON_*/DPAD_* keys (A=1 B=2 X=3 Y=4
  LB=5 RB=6 Start=14 Back=15 L3=16 R3=17 LT=18 RT=19 Up..Right=20..23), and sets gpad_enabled /
  gpad_present (dvar ptrs 147054220/147054224, bool at +24) only while a pad is connected.
  `+exec default_controller.cfg` loads BO1's layout. BrowserFrame has a gamepad menu cursor.
  Only Xbox glyphs exist in the PC data; PadKind detects PlayStation for later.
- Choppiness: swap interval 0 + com_maxfps 60 (no 60/30 vsync judder); emscripten setImmediate
  main loops (mode 2) spun a whole core -> 1 ms minimum. Log prints "frames: fps/avg/max/hitches".
- Black boot screen: loading.rs. Progress = bytes read / pack .ff bytes, done at
  "KISAK_STARTUP leave=<zone>". iOS: UIKit views over the game view (updated on the UI thread,
  since the engine barely presents while loading); desktop: scissor clears before present.
- Debug: BO1Z_FAKEPAD="t:Btn;t:ly=1;..." scripted pad, BO1Z_SHOT_AT="seconds" screenshots.
- PC check: in-game 51-58 fps steady (debug JIT build), keyboard hints without a pad.
  iOS-only code (UIKit overlay, GameController productCategory) verified against crate sources only.

## 2026-10-12: all 10 maps, map selector, store-based IPA
- Project moved to D:\bo1z\dddd; data in D:\bo1z\maps (per-map trees, hard links) + D:\bo1z\store
  (content-addressed, each unique file once). The move had dropped ios/, manifests/, tools/,
  src/main.rs: restored from git.
- fs.rs: mounts are `MountSrc::Dir` (dev: --game D:/bo1z/maps) or `MountSrc::Index` (IPA:
  game/maps/<slug>.json = [[path, sha256, size]], blobs in game/store/<ab>/<sha>). Case-insensitive.
- maps.rs: catalog in the site's menu order (kino first). menu.rs: launch-time selector, nothing
  loads until a map is picked. Uses the site's MISSION SELECT art (mapselect-*.webp from
  C:\Users\mirac\Desktop\dddd\mapselect, copied to D:\bo1z\maps\menu; landing.js layout: rows
  450x40 at x352 y259 pitch 42, panel 750x720 at 850,240 on a 1920x1080 stage). iOS: UIImageViews;
  desktop: GL blits. Falls back to plain tiles without art. `--map <slug>` skips the menu.
- emval: Kino's shader bundle needs Object.keys(...).sort() -> JS array methods (sort/reverse in
  place, join, slice, Object.values/entries).
- Desktop: game menus (pause) release the mouse and get an absolute cursor; re-locked on close.
- package-ipa.mjs: CI IPA + engine + store blobs (hard-linked staging in D:\bo1z\tmp-ipa) + per-map
  indexes + menu art -> D:\bo1z\BO1Z-all.ipa.
- Loading screen = each map's loading cinematic (web/video/<zone>_load.webm -> convert-videos.mjs ->
  maps/menu/load-<slug>.mp4, H.264/AAC). iOS: AVPlayer layer under the bar. Desktop: ffmpeg-decoded
  frames in a child window with its own EGL context (gl_ctx::Aux, loadvideo.rs). Fallback still:
  extract-loadscreens.mjs (DXT1 loadscreen_<zone>.iwi -> loadscreen-<slug>.png). Status line says
  LOADING, then PRESS A TO START; the intro gate (KB_Input 7) only opens after load on a fresh press.
- Pause-menu Quit logs Com_LoadFrontEnd (no front end in this build): desktop relaunches into the
  map select; iOS exits (cannot spawn) -> reopen lands on the map select.
- 2026-10-12 device crash on map pick: loading overlay was created before loading::init, so
  progress = 0/0 = NaN -> UIView setFrame NaN -> CALayer exception (SIGABRT). Fixed; overlay removed.
- Loading flow now: cinematic only (no bar/text), removed when it ends -> the engine's own
  loadscreen shows; game auto-starts once loaded and the video is gone (press skips a video
  still running after load). Desktop video drawn with GDI: a second ANGLE GL context on the UI
  thread crashed (segfault) alongside the engine's context.
- Test the iPad data layout on PC: D:\bo1z\idx-game (maps/*.json + junctions store, menu).
- Build 20548b0 (CI run 38240728800): crash fix + cinematic flow -> D:o1z\BO1Z-all.ipa (6.73 GB). Awaiting device test.
- Build 20548b0 on iPad: maps load and start, then abort ~1 s in: engine assert cl_gamepad.cpp:744 |axisDeflection| <= 1 (CL_GamepadAxisValue). iOS GameController sticks are square ((1,1) at diagonals); input.rs now clamps sticks to the unit circle (0.999) and triggers to [0,1]. Reproduced + verified on PC with BO1Z_FAKEPAD diagonals.
- log.rs keeps the previous run as bo1z-previous.log.

## 2026-10-12: iPad black patches, pause-menu pad, performance
- iPad (c0cbe72) still showed black blocks/stripes over surfaces (Moon, Five) and stutter.
  Ruled out on PC: shadow maps (the 1024x2048/512x2048 depth textures are never rendered or
  sampled; units 12/13 are the DXT1 sun-visibility + RGBA lightmaps), float depth, feedback
  loops (none), GL errors (none). The engine does a depth prepass (~650 colour-masked draws a
  frame) and draws colour/decals with LEQUAL from other programs; Metal compiles each program's
  vertex stage separately and may round positions differently -> depth test fails in blocks.
  Fix: gl.rs injects `invariant gl_Position;` into every vertex shader; iOS runs with
  `r_depthPrepass 0` (Apple GPUs remove hidden surfaces in hardware; ~20% fewer draws).
  PC debug: 32 -> ~42-47 fps in the same spot. Awaiting device confirmation.
- EGL context with KHR_no_error (no ANGLE validation per call) unless BO1Z_GLSTATS is set.
- The engine already compiles programs asynchronously (polls COMPLETION_STATUS_KHR).
- Pause menu with a pad: the engine's menus take arrows/Enter/Escape; DPAD_* keys only hit
  gameplay binds (+actionslot). In menus the mapper now sends D-pad/left stick -> arrows (with
  hold-repeat), A -> Enter, B -> Escape, Start -> togglemenu. Buttons held across a menu
  open/close are ignored until released (Start no longer double-fires).
- Debug: BO1Z_NOINPUT=1 (ignore real mouse/keyboard, never grab), FAKEPAD "<t>:shot",
  BO1Z_ARGS="+set ...", BO1Z_GLSTATS=1, BO1Z_SHADERDUMP=<dir>.
- Build 6f81fa3 (CI run 38300609900) -> D:\bo1z\BO1Z-all.ipa (6.73 GB). PC check: Moon + Five render clean, Five 60 fps capped (debug host). Awaiting device test + bo1z.log frames lines.

## 2026-10-12: shader warm-up during loading
- iPad: rendering fixed (invariant + no prepass confirmed by user). Remaining: pop-in and
  stutter early in a map, improving the longer you play.
- Cause: the engine builds programs on demand (KHR_parallel_shader_compile; draws whose
  program is not ready are skipped: WEB_GL_DRAWS pending=N) and only prebuilds the bundle's
  117 "opening_pairs" of 5075 (web/shaders/shaders.pack metadata). r_shaderWarming and
  r_webWorldWarm do nothing in this build. Pack sources are not exactly what is compiled: the
  engine rewrites D3DCOLOR vertex inputs (vs_vN -> vs_vN_bgra.bgra) per vertex format.
- shadercache.rs: records every (vs, fs) source pair the engine links, per map
  (<data>/shadercache/<slug>.json), merged with a shipped list (<game>/shadercache/<slug>.json,
  recorded on PC with a scripted tour: FAKEPAD "+t" steps count from game start, "quit").
  On map pick a worker thread with a shared EGL context (same attributes; surfaceless or
  1x1 pbuffer) compiles and links them during the loading video; links only once both
  shaders report COMPLETION_STATUS so ANGLE's lock is not held waiting.
- blobcache.rs: EGL_ANDROID_blob_cache on disk (iOS Library/Caches/angle, PC data/angle-cache).
- PC Five, same walk: pending draws after spawn 377 -> 6; first 5 s 21 fps / 1.7 s max ->
  55 fps / 42 ms max.
- frames log line now adds "game X ms, present Y ms; read Z MB" (engine work vs present/GPU
  wait, file data read) for diagnosing the iPad.
- The site ships <map>-programs.json (only five exists: 481 programs) and engine.js passes it to
  KB_SetProgramManifest before callMain; the engine then links those programs during loading.
  build-program-manifests.mjs builds them for every map from the host's recordings (mapped back
  to shaders.pack hashes; mask = BGRA-rewritten vertex inputs) + the site's five list ->
  D:\bo1z\maps\shadercache\<slug>-programs.json; rt.rs passes it like engine.js.
- Recorded on PC (scripted tour per map): kino 399, five 317(+481 site = 519), riese 517, nacht 326,
  verruckt 511, shinonuma 369, ascension 547, cotd 364, shangrila 369, moon 444 programs.
- PC fresh-install test (Five, empty caches, shipped lists): pending draws after spawn 0
  (was 377). Remaining early hitch: ~60 MB texture streaming in the first 10 s (read MB column).
