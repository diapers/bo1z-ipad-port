#!/usr/bin/env bash
# fetch-or-build-angle.sh - produce ANGLE (libEGL + libGLESv2, Metal backend) as
# iphoneos-arm64 DYNAMIC frameworks for the BO1Z iOS app. Runs on macOS only.
#
#   usage: ios/fetch-or-build-angle.sh <out_dir>
#
#   output: <out_dir>/libEGL.framework/libEGL
#           <out_dir>/libGLESv2.framework/libGLESv2
#           <out_dir>/ANGLE_INFO.txt          (where it came from, revision, gn args)
#
#   ANGLE_MODE=auto      (default) pinned prebuilt; falls back to a source build if the
#                        download / checksum / layout check fails
#   ANGLE_MODE=prebuilt  pinned prebuilt only
#   ANGLE_MODE=source    build the pinned ANGLE revision from source (depot_tools + gn)
#   ANGLE_FORCE=1        redo the work even if <out_dir> already holds valid frameworks
#   ANGLE_WORK_DIR=...   scratch dir (default: $RUNNER_TEMP/angle-work or /tmp/angle-work)
#
# The CI cache key hashes THIS FILE, so bumping any pin below invalidates the cache.
#
# Why these frameworks work at runtime without any rpath: ANGLE's libEGL loads its
# dispatch library itself from "<dir of main executable>/Frameworks/libGLESv2.framework/
# libGLESv2" on iOS (src/common/system_utils*.cpp, ANGLE_PLATFORM_IOS_FAMILY), and the
# host dlopen()s libEGL by absolute path. Both frameworks must therefore sit side by side
# in BO1Z.app/Frameworks/ - package.sh does that.
set -euo pipefail

# ----------------------------------------------------------------------------- pins
# Prebuilt: ANGLE xcframeworks shipped in ghera/raylib-iOS releases (zlib-licensed repo;
# ANGLE itself is BSD-3-Clause). Built from ANGLE with Chromium's ios_framework_bundle
# (dynamic framework, install name @rpath/libEGL.framework/libEGL), Metal-only (ANGLE's
# iOS default), iphoneos arm64 + simulator slices. We only take the device slice.
PREBUILT_TAG="6.0.6-iOS"
PREBUILT_URL="https://github.com/ghera/raylib-iOS/releases/download/${PREBUILT_TAG}/raylib-ios-xcframeworks-${PREBUILT_TAG}.zip"
PREBUILT_SHA256="8713c8d6e82a4afaf57c358b97a5c0de62943d5ee9aa63883f45d6fc2112abaa"   # GitHub asset digest
PREBUILT_DESC="ANGLE 2.1.28853 (git 170484eea03a) per the raylib-iOS ${PREBUILT_TAG} release notes"

# Source: the same ANGLE revision (a cherry-pick on the chromium M155 release branch),
# so prebuilt and source builds should behave identically.
ANGLE_GIT="https://chromium.googlesource.com/angle/angle"
ANGLE_COMMIT="170484eea03a4255a388072add4232e1fadfbee4"
DEPOT_TOOLS_GIT="https://chromium.googlesource.com/chromium/tools/depot_tools.git"
IOS_MIN="17.0"

# gn args for an iphoneos-arm64 release build with ONLY the Metal backend.
# Unknown/unused args only make gn print a warning, never fail.
GN_ARGS="target_os=\"ios\"
target_cpu=\"arm64\"
target_environment=\"device\"
ios_deployment_target=\"${IOS_MIN}\"
ios_enable_code_signing=false
is_debug=false
is_component_build=false
is_official_build=false
dcheck_always_on=false
symbol_level=0
enable_dsyms=false
treat_warnings_as_errors=false
use_remoteexec=false
angle_enable_metal=true
angle_enable_gl=false
angle_enable_vulkan=false
angle_enable_swiftshader=false
angle_enable_wgpu=false
angle_enable_null=false
angle_build_tests=false
angle_has_histograms=false"

# ----------------------------------------------------------------------------- helpers
OUT="${1:?usage: $0 <out_dir>}"
MODE="${ANGLE_MODE:-auto}"
WORK="${ANGLE_WORK_DIR:-${RUNNER_TEMP:-/tmp}/angle-work}"
LIBS=(libEGL libGLESv2)

log() { printf '[angle] %s\n' "$*" >&2; }
die() { printf '[angle] ERROR: %s\n' "$*" >&2; exit 1; }

[[ "$(uname -s)" == "Darwin" ]] || die "must run on macOS (needs Xcode, lipo, otool)"
mkdir -p "$OUT" "$WORK"
OUT="$(cd "$OUT" && pwd)"

# An exported symbol that must exist in each library.
probe_symbol() { case "$1" in libEGL) echo _eglGetPlatformDisplay ;; libGLESv2) echo _glDrawArrays ;; esac; }

# Platform from LC_BUILD_VERSION: 2 = IOS (device), 7 = IOSSIMULATOR.
macho_platform() { otool -l "$1" | awk '/cmd LC_BUILD_VERSION/{f=1} f && !p && $1=="platform"{print $2; p=1}'; }
macho_minos()    { otool -l "$1" | awk '/cmd LC_BUILD_VERSION/{f=1} f && !p && $1=="minos"{print $2; p=1}'; }

# verify_fw <path/to/X.framework> <X> : dynamic, arm64-only (thins if fat), iOS device, exports.
verify_fw() {
  local fw="$1" name="$2" bin="$1/$2" archs plat sym
  [[ -f "$bin" ]] || { log "$name: missing $bin"; return 1; }
  # (no grep -q on pipes: with pipefail an early grep exit SIGPIPEs the producer)
  file -b "$bin" | grep 'dynamically linked shared library' >/dev/null || { log "$name: not a dylib: $(file -b "$bin")"; return 1; }
  archs="$(lipo -archs "$bin")"
  if [[ "$archs" != "arm64" ]]; then
    [[ " $archs " == *" arm64 "* ]] || { log "$name: no arm64 slice (archs: $archs)"; return 1; }
    lipo "$bin" -thin arm64 -output "$bin.thin" && mv -f "$bin.thin" "$bin"
  fi
  plat="$(macho_platform "$bin")"
  [[ "$plat" == "2" || "$plat" == "IOS" ]] || { log "$name: platform '$plat' is not iOS device (simulator slice?)"; return 1; }
  sym="$(probe_symbol "$name")"
  nm -gU "$bin" | grep " ${sym}\$" >/dev/null || { log "$name: does not export $sym"; return 1; }
  log "$name ok: arm64 iOS minos=$(macho_minos "$bin") id=$(otool -D "$bin" | tail -n1)"
}

verify_out() {
  local l
  for l in "${LIBS[@]}"; do verify_fw "$OUT/$l.framework" "$l" || return 1; done
}

# stage_fw <src X.framework> <X> : copy into $OUT, drop build-only bits, verify.
stage_fw() {
  local src="$1" name="$2" dst="$OUT/$2.framework"
  rm -rf "$dst"
  ditto "$src" "$dst"
  # Headers/Modules are compile-time only (the host dlopen()s ANGLE); old signatures are
  # replaced by package.sh.
  rm -rf "$dst/Headers" "$dst/Modules" "$dst/_CodeSignature"
  verify_fw "$dst" "$name"
}

# Device slice of an xcframework: SupportedPlatform=ios, no SupportedPlatformVariant, arm64.
device_slice() {
  python3 - "$1/Info.plist" <<'PY'
import plistlib, sys
d = plistlib.load(open(sys.argv[1], "rb"))
for lib in d.get("AvailableLibraries", []):
    if (lib.get("SupportedPlatform") == "ios" and not lib.get("SupportedPlatformVariant")
            and "arm64" in lib.get("SupportedArchitectures", [])):
        print(lib["LibraryIdentifier"] + "/" + lib["LibraryPath"])
        sys.exit(0)
sys.exit("no ios-arm64 device slice in " + sys.argv[1])
PY
}

write_info() {
  { echo "source: $1"
    echo "date:   $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "xcode:  $(xcodebuild -version 2>/dev/null | tr '\n' ' ')"
    shift; printf '%s\n' "$@"
    for l in "${LIBS[@]}"; do
      echo "$l: $(shasum -a 256 "$OUT/$l.framework/$l" | cut -d' ' -f1)  minos=$(macho_minos "$OUT/$l.framework/$l")"
    done
  } > "$OUT/ANGLE_INFO.txt"
  cat "$OUT/ANGLE_INFO.txt" >&2
}

# ----------------------------------------------------------------------------- prebuilt
fetch_prebuilt() {
  local dir="$WORK/prebuilt" zip l xcf slice
  zip="$dir/angle.zip"
  rm -rf "$dir"; mkdir -p "$dir/x"
  log "downloading $PREBUILT_URL"
  curl -fL --retry 4 --retry-delay 5 --connect-timeout 30 -o "$zip" "$PREBUILT_URL" || { log "download failed"; return 1; }
  echo "$PREBUILT_SHA256  $zip" | shasum -a 256 -c - >/dev/null \
    || { log "sha256 mismatch (got $(shasum -a 256 "$zip" | cut -d' ' -f1))"; return 1; }
  unzip -q "$zip" -d "$dir/x" || { log "unzip failed"; return 1; }
  for l in "${LIBS[@]}"; do
    xcf="$(find "$dir/x" -type d -name "$l.xcframework" -print -quit)"
    [[ -n "$xcf" ]] || { log "$l.xcframework not found in archive"; return 1; }
    slice="$(device_slice "$xcf")" || return 1
    stage_fw "$xcf/$slice" "$l" || return 1
  done
  write_info "prebuilt $PREBUILT_URL" "sha256(zip): $PREBUILT_SHA256" "angle: $PREBUILT_DESC"
}

# ----------------------------------------------------------------------------- source
build_source() {
  local dt="$WORK/depot_tools" src="$WORK/angle" ob l fw t0=$SECONDS
  command -v python3 >/dev/null || die "python3 required"
  xcrun --sdk iphoneos --show-sdk-path >/dev/null || die "no iphoneos SDK (xcode-select?)"

  [[ -d "$dt/.git" ]] || git clone --depth 1 "$DEPOT_TOOLS_GIT" "$dt"
  export PATH="$dt:$PATH" DEPOT_TOOLS_UPDATE=0 DEPOT_TOOLS_METRICS=0

  if [[ ! -d "$src/.git" ]]; then
    mkdir -p "$src"
    git -C "$src" init -q
    git -C "$src" remote add origin "$ANGLE_GIT"
  fi
  log "fetching ANGLE $ANGLE_COMMIT"
  git -C "$src" fetch -q --depth 1 origin "$ANGLE_COMMIT"
  git -C "$src" checkout -q --force FETCH_HEAD

  # ANGLE's own bootstrap writes an unmanaged .gclient for the "." solution; add iOS deps.
  (cd "$src" && python3 scripts/bootstrap.py)
  grep -q '^target_os' "$src/.gclient" || echo "target_os = ['ios']" >> "$src/.gclient"
  log "gclient sync (no history) ..."
  (cd "$src" && gclient sync --no-history --shallow --jobs 8)
  log "sync took $((SECONDS - t0))s"

  ob="$src/out/ios-arm64"
  (cd "$src" && gn gen "$ob" --args="$(echo "$GN_ARGS" | tr '\n' ' ')")
  (cd "$src" && gn args "$ob" --list --short --overrides-only) >&2 || true
  log "building libEGL libGLESv2 ..."
  (cd "$src" && autoninja -C "$ob" libEGL libGLESv2)
  log "sync+build took $((SECONDS - t0))s"

  for l in "${LIBS[@]}"; do
    fw="$(find "$ob" -maxdepth 2 -type d -name "$l.framework" -print -quit)"
    [[ -n "$fw" ]] || die "$l.framework not produced under $ob"
    stage_fw "$fw" "$l" || die "$l.framework failed verification"
  done
  write_info "source $ANGLE_GIT@$ANGLE_COMMIT" "gn args:" "$GN_ARGS" \
    "build time: $((SECONDS - t0))s"
}

# ----------------------------------------------------------------------------- main
if [[ "${ANGLE_FORCE:-0}" != "1" ]] && verify_out 2>/dev/null; then
  log "valid frameworks already in $OUT - nothing to do (ANGLE_FORCE=1 to redo)"
  exit 0
fi

case "$MODE" in
  prebuilt) fetch_prebuilt || die "prebuilt ANGLE failed" ;;
  source)   build_source ;;
  auto)     fetch_prebuilt || { log "prebuilt failed - building ANGLE from source"; build_source; } ;;
  *)        die "ANGLE_MODE must be auto|prebuilt|source (got '$MODE')" ;;
esac

verify_out || die "final verification failed"
log "done -> $OUT"
