# iOS packaging (BO1Z for iPad, TrollStore)

Everything here runs on a GitHub Actions `macos-15` runner. Nothing iOS-related builds on
the Windows PC. Workflow: `.github/workflows/ios-build.yml`.

```
ios/fetch-or-build-angle.sh build/angle-ios     # ANGLE libEGL/libGLESv2 iphoneos-arm64 frameworks (cached in CI)
cargo build --release --target aarch64-apple-ios --manifest-path host/Cargo.toml   # IPHONEOS_DEPLOYMENT_TARGET=17.0
ios/package.sh                                  # -> build/ios/Payload/BO1Z.app + build/ios/BO1Z.ipa
node inject-assets.mjs --ipa=BO1Z.ipa           # (on the PC) adds BO1Z.app/game/<map>/...
```

| File | Purpose |
|---|---|
| `fetch-or-build-angle.sh` | Gets ANGLE as dynamic `libEGL.framework` + `libGLESv2.framework` (device arm64 only), verifies them, and writes `ANGLE_INFO.txt` |
| `Info.plist` | `com.local.bo1z`, "BO1Z", iOS 17.0, landscape only, Files-app Documents, ExtendedGamepad |
| `entitlements.plist` | `get-task-allow`, `increased-memory-limit`, `extended-virtual-addressing`. **No `dynamic-codesigning`** (see below) |
| `package.sh` | Builds the bundle, ad-hoc signs it (`codesign`, or `SIGNER=ldid`), zips the IPA |
| `IOS_RISKS.md` | wasmtime-on-iOS risks, JIT vs AOT, and recommended `Config` settings, with sources |

## Decisions

**ANGLE: pinned prebuilt first, source build as fallback (`ANGLE_MODE=auto`).**
- Default source: the `libEGL.xcframework` / `libGLESv2.xcframework` in
  [ghera/raylib-iOS](https://github.com/ghera/raylib-iOS/releases) release `6.0.6-iOS`
  (ANGLE 2.1.28853, git `170484eea03a`, on the M155 release branch). The script pins the
  zip's sha256 from the GitHub asset digest. These are dynamic frameworks produced by ANGLE's
  own Chromium `ios_framework_bundle` (install name `@rpath/libEGL.framework/libEGL`). They
  are Metal-only, because ANGLE's iOS defaults are `angle_enable_metal = is_apple`,
  `angle_enable_gl = ... && !is_ios`, and Vulkan off on iOS. Licenses: ANGLE is BSD-3-Clause
  and the repo is zlib. The download is about 15 MB and takes seconds.
- Why not build from source by default: ANGLE's docs say iOS builds are "currently possible
  only from Chromium checkout". Standalone `fetch angle` + `target_os="ios"` does work in
  practice (ghera/ANGLE-iOS and minigame-labs/migo-runtime do it), but it needs a
  multi-GB `gclient sync` and an estimated **20–45 min** cold build on the 3-core runner.
  It is the fallback, and it can be forced with `ANGLE_MODE=source`. It builds the
  **same commit** (`170484eea03a4255a388072add4232e1fadfbee4`) with the gn args below:
  ```
  target_os="ios" target_cpu="arm64" target_environment="device" ios_deployment_target="17.0"
  ios_enable_code_signing=false is_debug=false is_component_build=false is_official_build=false
  dcheck_always_on=false symbol_level=0 enable_dsyms=false treat_warnings_as_errors=false use_remoteexec=false
  angle_enable_metal=true angle_enable_gl=false angle_enable_vulkan=false angle_enable_swiftshader=false
  angle_enable_wgpu=false angle_enable_null=false angle_build_tests=false angle_has_histograms=false
  ```
  ninja targets: `libEGL libGLESv2` (on iOS both are groups that build `<lib>.framework`).
- **Cache key:** `angle-ios-<arch>-<mode>-hashFiles(ios/fetch-or-build-angle.sh)`. Every pin
  (URL, sha256, commit, gn args) lives in that script, so editing a pin rebuilds. Only the
  final two frameworks (about 10 MB) are cached. The workflow saves the cache straight after
  the ANGLE step, so a cargo failure does not lose a 40-minute build.
- An alternative prebuilt that was considered:
  [minigame-labs/migo-runtime `angle-apple-*`](https://github.com/minigame-labs/migo-runtime/releases/tag/angle-apple-52f59428-p2)
  (Metal-only, adds an `EGL_ANGLE_window_fixed_size` patch for Metal).

**Expected GL extensions on iPad Air M1 / iOS 17** (from ANGLE's Metal caps tables. Log
`GL_EXTENSIONS` at startup to confirm):

| Extension | Status | Gate in ANGLE source |
|---|---|---|
| `GL_EXT_texture_compression_s3tc` / `dxt1` / `ANGLE_texture_compression_dxt3/5` | expected | `display->supportsBCTextureCompression()` = `[MTLDevice supportsBCTextureCompression]` on iOS 16.4+ |
| `GL_EXT_color_buffer_float` | expected | R/RG/RGBA 16F/32F `colorRenderable: true` |
| `GL_EXT_texture_norm16` | expected | R16/RG16/RGBA16Unorm filterable + renderable |
| `GL_OES_texture_float_linear`, `GL_EXT_float_blend` | expected | 32F filterable/blendable if `supportsAppleGPUFamily(7) && supports32BitFloatFiltering()` (M1 is Apple7) |

Keep a CPU BC fallback (as Muguet does with `bcdec`) in case `supportsBCTextureCompression`
reports false.

**Packaging:** a plain binary wrapped by hand into a `.app`, with no Xcode project.
`Info.plist` has **no `UIApplicationSceneManifest`**, because winit 0.30 has no UIScene
support. Scenes only become mandatory for iOS-27-SDK builds running on iOS 27. The
workflow selects Xcode 16.4 when it is installed. `UILaunchScreen` and `UIDeviceFamily`
`[1,2]` are required: without them iPadOS letterboxes the app or runs it in iPhone mode.

**JIT entitlement.** `dynamic-codesigning` was **not** added. TrollStore's README lists it
as one of three entitlements that are "banned" on iOS 15+ with A12 or newer chips, and
says apps signed with them crash on launch. The M1 counts as A12-or-newer. TrollStore
offers JIT a different way: an app signed with `get-task-allow` gets an "Open with JIT"
option, and there is also `apple-magnifier://enable-jit?bundle-id=com.local.bo1z`
(TrollStore 2.0.12+, with "URL Scheme Enabled" in TrollStore settings). That sets
CS_DEBUGGED on the running process, which allows the RW→RX `mprotect` that wasmtime uses.
The AOT alternative, which needs no JIT at all, is in `IOS_RISKS.md`. TrollStore also
supports only iOS 14.0b2–16.6.1, 16.7 RC and **17.0** (17.0.1+ never), so the iPad must
be on exactly 17.0.

## Runtime contract for the Rust host (iOS)

| What | Value |
|---|---|
| Bundle dir | `std::env::current_exe()?.parent()` → `…/BO1Z.app` (read-only) |
| EGL library to dlopen | `<bundle>/Frameworks/libEGL.framework/libEGL` (equivalently the string `@executable_path/Frameworks/libEGL.framework/libEGL`, which dyld expands in `dlopen`) |
| GLES library | `<bundle>/Frameworks/libGLESv2.framework/libGLESv2`. **libEGL loads it itself** from `<exe dir>/Frameworks/libGLESv2.framework/libGLESv2`, so the host does not need to. Get GL entry points through `eglGetProcAddress` (as `gl_ctx.rs` does) or `dlsym` on a handle to this path |
| Must not | link Apple's `OpenGLES.framework`, or resolve `gl*` with `dlsym(RTLD_DEFAULT, …)`. Apple's EAGL exports the same `gl*` names |
| Build-time ANGLE | none. Nothing links ANGLE. `khronos-egl` `dynamic` + `libloading` are enough, and no rpath is needed |
| EGL display | `eglGetDisplay(EGL_DEFAULT_DISPLAY)` → Metal (the only backend in this build) |
| `EGLNativeWindowType` | a `CALayer*`. ANGLE casts the window to `CALayer*` and uses it directly if it is a `CAMetalLayer`; otherwise it adds a `CAMetalLayer` sublayer and tracks bounds/contentsScale itself. Simplest: pass `[uiView layer]` from winit's `UiKitWindowHandle.ui_view`. If you pass your own `CAMetalLayer`, you must keep its frame/contentsScale updated on rotation (main thread). Lower `view.contentScaleFactor` before creating the surface to render below native 2360×1640 |
| Game data | `<bundle>/game/<map>/manifest.json` + pack files (added by `inject-assets.mjs`) |
| Writable | `$HOME/Documents` (visible in the Files app), `$HOME/Library/Caches` (put the wasmtime cache here explicitly), `$HOME/tmp` |
| Threads | winit owns the main thread (`UIApplicationMain` never returns), and the iOS main thread has a **1 MiB** stack. Run wasm on spawned threads with an explicit `stack_size` (≥ `max_wasm_stack` + margin, e.g. 16 MiB). The default secondary-thread stack is 512 KiB |
| Deployment target | build with `IPHONEOS_DEPLOYMENT_TARGET=17.0`. `package.sh` rejects a binary whose `minos` is above 17.0 |
| Memory | log `os_proc_available_memory()` at startup to confirm `increased-memory-limit` took effect |
| JIT (if Cranelift runs on-device) | check `csops(getpid(), CS_OPS_STATUS=0)` for `CS_DEBUGGED (0x10000000)` before compiling. If it is missing, open `apple-magnifier://enable-jit?bundle-id=com.local.bo1z` (UIApplication `openURL`, main thread) or show "launch via TrollStore → Open with JIT" |

## Troubleshooting

- `cpal` → `coreaudio-sys` runs bindgen against the iOS SDK. If it fails, that is in
  `host/` (libclang/SDK), not this packaging.
- `install_name_tool` header-padding failure in `package.sh`: this only happens when the
  host *links* ANGLE through `@rpath`. Either dlopen it, or link with
  `-Wl,-rpath,@executable_path/Frameworks`.
- To check which ANGLE was used: the `BO1Z-ipa` artifact includes `ANGLE_INFO.txt`.
