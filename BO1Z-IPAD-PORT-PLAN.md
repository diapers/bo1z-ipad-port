# BO1Z → iPad: port plan

Derived from (a) the reference IPA you supplied and (b) direct analysis of the live
vel.gg/bo1z artifacts. Everything below is measured, not assumed.

## 1. What the reference IPA actually is

The IPA you supplied is a **native Mach-O iOS port** built with
[c22dev/muguet](https://github.com/c22dev/muguet), an open-source host-skeleton project
that runs a web game's WebAssembly build as a normal native app. Its payload is
`Payload/<app>.app`.

Measured contents:

| Item | Size | Meaning |
|---|---|---|
| main executable | 120.2 MB | **ARM64 Mach-O** (`cffaedfe` / `cputype 0x0100000c`) — native, not a web wrapper |
| `shaders-iphoneos.metallib` | 71.5 MB | Metal shader library, **AOT-compiled at build time** |
| `BCTextureDecoderReference.inc` | 2 KB | `bcdec_bc1..bc7` — decodes BC1-7 on CPU (Apple GPUs can't sample BC/DXT) |
| `manifest.json` | 384 KB | Maps the game's asset VFS; `mount: /game/`, ~2921 files |
| `bootset_native.json` / `_low` | 185 KB each | Which assets to boot with (full vs low-memory) |
| `icon_*.png` × 22 | — | Touch control set: fire, aim, reload, jump, sprint, cover, gyro... |
| `Info.plist` | — | `DTXcode 1640`, `iphoneos18.5`, `MinimumOSVersion 17.0`, `UIRequiredDeviceCapabilities arm64`, landscape-only, GameController support |

**The method (from the project README, verbatim):**

> "this whole port relies on the WebAssembly compilation of the game. in a browser, that
> `game.wasm` runs inside the JS engine, behind WebGPU, Web Audio, web workers... Muguet
> throws that layer away and runs the same `game.wasm` as a regular native app, with
> wasmtime for the code and wgpu on Metal for the graphics"
>
> "On iOS, we can't properly do that (i dont rlly want you to deal with the hassle of
> enabling JIT), so the game code is compiled when you build the app and baked into it"

So: **no engine port, no engine source needed.** The Wasm module is the portable artifact.
You replace the *web platform layer* with native Metal (wgpu), native audio, native
filesystem, and a touch UI.

Build requirements (from README): Apple Silicon Mac, macOS 14+, Rust, **Xcode 26+**,
`aarch64-apple-ios` target, `xcodegen`, Apple dev account (free tier works).
iOS notes: *"iOS needs the increased memory limit entitlement"*, *"a device on iOS 15 or
later, ideally 8 GB of RAM"*.

## 2. Does vel.gg/bo1z expose the same artifact? YES

```
https://vel.gg/bo1z/artifacts/KisakBlack-web.wasm   200  application/wasm   9.62 MB
https://vel.gg/bo1z/artifacts/KisakBlack-web.mjs    200  (Emscripten loader, 275 KB)
```

Saved to `bo1z-wasm/KisakBlack-web.wasm`. Verified `\0asm` version 1.

### Module facts (parsed from the binary)

| Property | Value |
|---|---|
| Size | 9.62 MB |
| Defined functions | 11,523 |
| **Imports** | **170** (166 `env` + 5 `wasi_snapshot_preview1`) |
| Exports | 73 |
| Memory | **shared**, min 32767 pages = **2047.9 MB**, max identical (fixed 2 GiB) |
| Exception tags | 2 |
| SIMD (v128) | no |
| Threads | **yes** (shared memory, wasm atomics/futex) |

### The 170 imports — this IS the worklist

| Category | Count | Native replacement |
|---|---|---|
| WebGL2 subset | ~66 | `wgpu` on Metal, or a GL-on-Metal shim |
| Emscripten WebGL glue (`emscripten_webgl_*`) | 6 | trivial context bookkeeping |
| **WebGPU bridge** (`RegisterWebGPUContext`, `__asyncjs__InitializeWebGPU`, `SetWebGPUProgramJS`, `WebGPUPendingPipelinesJS`, readback fns) | ~9 | `wgpu` — closest 1:1 mapping available |
| Filesystem (`_wasmfs_opfs_*`, preload) | 31 | plain native file I/O over the asset zip |
| embind / emval (`_embind_*`, `_emval_*`) | 21 | reimplement C++↔host bindings in Rust |
| JSPI async (`__asyncjs__*`, `emscripten_asm_const_async_on_main_thread`) | ~5 | native blocking/async calls — **JSPI ceases to exist as a concept** |
| misc (time, logging, exit, console) | rest | std |

Key insight: **only 66 real WebGL2 entry points** are imported — a narrow, well-known
subset, not the full API. And the module carries a *WebGPU* path, which is what you want on
Metal.

## 3. Why this demolishes the earlier blockers

| Earlier blocker | Status under this approach |
|---|---|
| JSPI missing on iOS 17 (`WebAssembly.Suspending`) | **GONE.** JSPI only exists to bridge Wasm↔JS. With wasmtime there is no JS. Their own build even keeps `__asyncjs__*` names, which you replace with native calls. |
| COOP/COEP, SharedArrayBuffer, cross-origin isolation | **GONE.** No browser, no headers. Shared memory becomes a real native shared mapping. |
| WKWebView `WKURLSchemeHandler` isolation bug | **N/A** — no WebView at all. |
| `EXT_color_buffer_float`, `s3tc`, GL extension gates | **GONE.** Those gates live in `capabilities.js` (JS), which you don't run. Texture format gaps are handled in Rust, exactly as Muguet does with `bcdec_bc1..7`. |
| `requestPointerLock` / no touch code | **REMAINS — and is now the single hardest part.** You must build the touch layer from scratch (Muguet has 22 control icons + gyro; nothing analogous exists for BO1Z). |
| 2 GiB memory demand | **REMAINS.** Module wants a fixed 2047.9 MB shared memory. Muguet's README says the increased-memory-limit entitlement is required — on your jailbroken device that is grantable. iPad Air M1 has 8 GB; it fits, but it's the main runtime risk. |

## 4. Your device

iPad Air M1 (5th gen), iOS 17, TrollStore.

- iPadOS 27 does support this model ("iPad Air 4th generation and later"). With this port
  route you **no longer need to update** — iOS 17 is fine, because wasmtime replaces WebKit.
- TrollStore installs unsigned IPAs, which is what you want since you can't easily sign.
- Jailbreak gives the memory entitlement; TrollStore alone does not.

## 5. The build machine

The port must be **compiled on macOS** (Apple Silicon, macOS 14+, Xcode 26+), because:

- iOS forbids JIT for third-party apps. Muguet's workaround is `wasmtime` **AOT
  precompilation at build time**, baked into the binary — that step needs macOS.
- `shaders-iphoneos.metallib` is produced by Apple's Metal toolchain.
- Rust cross-compilation to `aarch64-apple-ios` needs the Apple SDK.

**Correction to an earlier assumption:** no physical Mac is required. A GitHub-hosted
`macos-15` runner gives you Xcode, the iOS SDK and the Metal toolchain, is free for public
repos, and produces the unsigned `.ipa` that TrollStore installs. That is what
`.github/workflows/macos-spike.yml` uses. Cross-compiling on Linux (theos/ldid) was
explored and abandoned — the Apple SDK is the blocker there, not the linker.

## 6. Asset mirror — DONE and hash-verified

The pack is publicly downloadable and I mirrored all of it. Verified independently by
re-hashing every file from disk against the manifest:

```
verified : 51 / 51        bad : 0        missing : 0
717.5 MB decoded, 684.3 MB on disk (from 232.5 MB of wire traffic)

boot        40 files  534.1 MB
background  10 files  166.4 MB
lazy         1 file    17.0 MB

zone/Common/zombie_pentagon.ff   108.0 MB   ok
zone/Common/common_zombie.ff      51.3 MB   ok
web/shaders/shaders-wgsl.pack     39.8 MB   ok
web/shaders/shaders.pack          17.0 MB   ok
web/sound/sounds.pack             66.1 MB   ok (fallback variant)
```

### The transport protocol (learned the hard way)

`manifest.transport = { encoding: "br", chunk: 8388608 }` (8 MiB identity chunks).

| Request | Result |
|---|---|
| `GET pack/<path>?part=<k>` | `Content-Encoding: br`; **fetch decodes it**; body = exactly one 8 MiB identity chunk. **Do not brotli-decode again.** |
| `GET pack/<path>.br/<k>` | same part, but **raw Brotli, undecoded** — this variant caused double-decompression bugs |
| `GET pack/<path>` | whole file; supports `Range: bytes=N-` → `206` + `Content-Range` |
| `cdn.vel.gg/packs/five/...` | **404 for all pack paths** — the CDN origin only authorises the `vel.gg` origin |

### The opus/fallback trap (cost two failed runs)

11 entries carry both `opus` and `fallback`. The worker's own code:

```js
function useFallback(e){ const {size,sha256,br}=e.fallback; Object.assign(e,{size,sha256,br}); delete e.opus; }
```

For these, **`<path>?part=k` serves the FALLBACK**, while the true primary bytes live at
`.kop?part=k` (Opus, needs an AudioDecoder to rebuild) or `.dxs?part=k` (DXT-stream-split,
inverted by `dxsInverse`). Chunking must follow the variant actually received, or parts are
read at wrong offsets. The site ships the fallback whenever WebCodecs `AudioDecoder` is
unavailable, so it is a supported variant — and 181 MB smaller for `sounds.pack`.

### Streamed cinematics — also fetched and verified

```
5.34 MB  web/video/zombie_pentagon_load.av1.webm   sha256 verified
5.36 MB  web/video/zombie_pentagon_load.webm       sha256 verified
```

**The mirror is complete: 53 files, 694.5 MB, every byte hash-verified.**

### Layout

Everything below lives on **E:** (C: is too small to build on — 5.1 GB free).

```
E:\bo1z\store\<ab>\<sha256>       347 unique blobs, 6.61 GB — every byte stored once
E:\bo1z\maps\<slug>\...           10 maps, each its own engine path tree (hard links)
E:\bo1z\maps\<slug>\manifest.json
index/share-index.json            blob index: hash -> size -> which maps reference it

C:\Users\mirac\Desktop\dddd\
  bo1z-wasm/KisakBlack-web.wasm   9.6 MB  engine wasm (shared by ALL maps)
  manifests/<slug>.json           all 10 manifests
  bo1z-wasm-host-surface.md       the 170-import porting worklist
```

Scripts: `build-index.mjs` (dedup index), `build-store.mjs` (fetch + materialise),
`verify-store.mjs` / `verify-map.mjs` (hash checkers), `fetch-map.mjs` (single map),
`map-space.mjs` (space accounting), `probe-*.mjs` (protocol/variant provers),
`wasm-inventory.mjs` (import mapper).

## 7. Maps: all 10 mirrored in a merged store — DONE

582 path references across 10 maps collapse to **347 unique blobs**:

```
8.19 GB of paths  ->  6.61 GB unique bytes   (1.58 GB saved, 19.3%)
56 blobs are referenced by more than one map
```

How it works: the engine opens *paths*, but the bytes are what cost disk. So each map
keeps its own real path tree under `maps/<slug>/`, and identical files are **hard links**
into `store/<ab>/<sha256>`. Nothing is copied, so peak disk = unique bytes. `nlink` proves
it: `iw_25.iwd` (10 maps, 21.5 MB) reports `nlink=11` — store copy plus ten map links.

| Map | Paths | Logical | Map | Paths | Logical |
|---|---|---|---|---|---|
| five | 53 | 728.2 MB | ascension | 64 | 821.0 MB |
| kino | 53 | 741.0 MB | cotd | 61 | 852.9 MB |
| riese | 59 | 791.0 MB | shangrila | 60 | 867.3 MB |
| nacht | 58 | 722.0 MB | moon | 57 | 1.14 GB |
| verruckt | 59 | 778.9 MB | | | |
| shinonuma | 58 | 745.0 MB | **total logical** | **582** | **8.19 GB** |
| | | | **physical** | **347** | **6.61 GB** |

Top savings: `common_zombie.ff` (79.7 MB × 7 = 478 MB), `iw_25.iwd` (21.5 MB × 10 =
193 MB), `patch_mp.ff` (10.9 MB × 10 = 98.5 MB), `iw_00.iwd` (10.9 MB × 10 = 98.3 MB).

### The trap this exposed: same path, different bytes

**53 paths appear in multiple maps with DIFFERENT content.** Every `main/iw_*.iwd` has up
to 10 distinct versions — `main/iw_01.iwd` alone has ten, from 16.5 MB to 22.1 MB. So
content must be keyed by **sha256, never by path**. Any layout that assumes a path means
the same bytes will silently serve the wrong map's data. The store is content-addressed
precisely because of this.

### Three bugs worth remembering (all mine, all fixed)

1. **Chunking follows `f.br`, not file size.** A file with an `br` array is served as
   identity chunks via `?part=k`; a file *without* `br` is one raw response. Kino's
   `common_zombie.ff` is 51.3 MB with no `br`, so a size-based heuristic invented 7
   phantom parts and 404'd. Every other large file happened to have `br`, hiding it.
2. **`?part=k` vs `.br/<k>`.** The parts are stored with `Content-Encoding: br`, so `fetch`
   decodes them — the body is already one plain 8 MiB identity chunk. Brotli-decompressing
   it again is wrong. `.br/<k>` is the raw-Brotli variant.
3. **The opus/fallback dual encoding.** 11 entries per map ship two encodings of the same
   file; `<path>?part=k` serves the *fallback*, while the primary lives at `.kop?part=k`
   (Opus) or `.dxs?part=k`. Chunking must match the variant received, or parts are read at
   wrong offsets. The fallback is also ~181 MB smaller for `sounds.pack`.

## 8. Open technical risk: AOT + shared memory

Not yet proven, and it gates the build strategy. Wasmtime's own docs say shared-memory
*creation* "is disabled by default... there are enough deficiencies in Wasmtime's
implementation and API integration", and wasm threads are **tier 2** ("will not receive
security updates... security CVEs will not be issued"). The BO1Z module declares
`shared memory min=max=32767` pages (~2048 MB), so this is not optional.

Muguet's iOS strategy is AOT precompilation (iOS forbids JIT). **Whether a shared-memory
module can be serialized is the question** — `.github/workflows/macos-spike.yml` +
`src/main.rs` test exactly that, including deserialize-on-fresh-engine, which is what an
app would do at launch. If serialization refuses shared memories, the fallback is enabling
JIT on the jailbroken device (`dynamic-codesigning` entitlement), which removes the need
for AOT entirely.

## 9. Proposed build order

1. **Run the macOS CI spike.** `.github/workflows/macos-spike.yml` already installs the
   Rust toolchain, adds `aarch64-apple-ios`, fetches the wasm, and tests whether a
   shared-memory module can be AOT-serialized. This answers section 8 before any port code
   is written.
2. **Fork [muguet](https://github.com/c22dev/muguet)** as the host skeleton — it already
   does wasmtime + wgpu + Metal + iOS packaging + touch UI + `make-ios.sh`.
3. **Load `KisakBlack-web.wasm` in wasmtime** with `wasm_threads`/shared memory enabled;
   stub all 170 imports, then fill them in by category (filesystem first — easiest and
   needed to boot, then WebGPU/`wgpu`, then embind, then audio).
4. **Serve the asset pack natively.** Per map: Five 728 MB / Kino 741 MB on disk (51 files
   + 2 cinematics each), and the engine wants a fixed ~2 GiB shared memory. Skip OPFS
   entirely: lay the files out on disk and back `_wasmfs_*` with real file I/O.
5. **Touch UI from zero.** Virtual sticks, aim-drag, fire/reload/interact/knife/grenade.
   The reference IPA's 22-icon control set is a usable starting visual language.
6. **Sign + install.** TrollStore on your iPad; grant the increased-memory entitlement.

### Honest sizing
Step 1 is decided — CI supplies the macOS toolchain, so nothing needs buying. Steps 2-5
are a real systems project: think weeks of focused work, not an afternoon. The reference
IPA proves the *approach* works for a comparable engine; it does not reduce the work of
reimplementing 170 host functions and inventing a control scheme.
