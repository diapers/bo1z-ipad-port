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

```
bo1z-wasm/KisakBlack-web.wasm     9.6 MB   engine wasm (shared by ALL maps)
pack-five/files/                   694 MB   Five   — 53 files, all verified
pack-kino/files/                   707 MB   Kino der Toten — 53 files, all verified
manifests/<slug>.json                      per-map manifests (all 10 saved)
pack-manifest.json                         Five's manifest
bo1z-wasm-host-surface.md                  the 170-import porting worklist
```

Scripts: `fetch-map.mjs` (map-selectable, resumable, hash-verifying downloader),
`fetch-streams.mjs`, `verify-map.mjs` / `verify-pack.mjs` (hash checkers),
`probe-maps.mjs` (prices every map), `probe-pack-protocol.mjs` (byte-protocol prover),
`probe-opus-variants.mjs`, `probe-shared-files.mjs`, `wasm-inventory.mjs`.

## 7. Maps: all 10 exist, we mirror 2

`maps.js` MAP_LIST gives the slug for each map. `five` is the exception — it is the FIRST
map, so its manifest and pack live at the site root, not under `/five/`:

| slug | zone | disk | wire |
|---|---|---|---|
| five | zombie_pentagon | 717.5 MB | 410.8 MB |
| kino | zombie_theater | 736.3 MB | 429.6 MB |
| riese | zombie_cod5_factory | 786.4 MB | 479.0 MB |
| nacht | zombie_cod5_prototype | 717.4 MB | 407.0 MB |
| verruckt | zombie_cod5_asylum | 774.2 MB | 438.8 MB |
| shinonuma | zombie_cod5_sumpf | 740.0 MB | 403.5 MB |
| ascension | zombie_cosmodrome | 814.8 MB | 440.2 MB |
| cotd | zombie_coast | 837.7 MB | 460.9 MB |
| shangrila | zombie_temple | 860.7 MB | 487.1 MB |
| moon | zombie_moon | **1.14 GB** | **662.7 MB** |
| | **TOTAL** | **8.19 GB** | **4.69 GB** |

**Mirrored so far: Five + Kino** (the proof-of-port pair). Fetch any other with:

```
node fetch-map.mjs --map=<slug>
```

All 10 manifests are in `manifests/`, so `--offline` works without re-probing.

### Two bugs worth remembering (both mine, both fixed in `fetch-map.mjs`)

1. **Chunking follows `f.br`, not file size.** A file with an `br` array is served as
   identity chunks via `?part=k`; a file *without* `br` is one raw response. Kino's
   `zone/Common/common_zombie.ff` is 51.3 MB with no `br`, so a size-based heuristic
   invented 7 phantom parts and 404'd. Every other large file happened to have `br`,
   which hid this until the second map.
2. **Sibling-mirror reuse.** 10+ paths are byte-identical across maps
   (`common_zombie.ff`, `patch.ff`, `iw_25.iwd`, `localization.txt`, ...), so
   `fetch-map.mjs` now reuses a matching copy from another `pack-*/files` tree — but only
   after the sha256 matches the current map's manifest. Never assumed from the path.

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
