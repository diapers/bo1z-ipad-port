#!/usr/bin/env bash
# package.sh - assemble Payload/BO1Z.app from the cargo-built iOS executable + ANGLE
# frameworks, ad-hoc sign it with ios/entitlements.plist, and zip it to BO1Z.ipa
# (installable with TrollStore, which fake-signs on install and keeps our entitlements).
# Runs on macOS only.
#
#   env (all optional):
#     BIN           executable  (default host/target/aarch64-apple-ios/release/bo1z_host)
#     ANGLE_DIR     dir holding libEGL.framework + libGLESv2.framework (default build/angle-ios)
#     OUT_DIR       output dir  (default build/ios) -> $OUT_DIR/Payload/BO1Z.app, $OUT_DIR/BO1Z.ipa
#     SIGNER        codesign (default) | ldid | none
#     ENTITLEMENTS  default ios/entitlements.plist
#     BUILD_NUMBER  -> CFBundleVersion (CI passes github.run_number)
#
# Resulting bundle:
#   BO1Z.app/bo1z_host                          main executable (CFBundleExecutable)
#   BO1Z.app/Info.plist, PkgInfo
#   BO1Z.app/Frameworks/libEGL.framework/libEGL         <- host dlopen()s this
#   BO1Z.app/Frameworks/libGLESv2.framework/libGLESv2   <- libEGL dlopen()s this itself
#   (BO1Z.app/game/<map>/... is added later by inject-assets.mjs, NOT here)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"
BIN="${BIN:-$ROOT/host/target/aarch64-apple-ios/release/bo1z_host}"
ANGLE_DIR="${ANGLE_DIR:-$ROOT/build/angle-ios}"
OUT_DIR="${OUT_DIR:-$ROOT/build/ios}"
SIGNER="${SIGNER:-codesign}"
ENTITLEMENTS="${ENTITLEMENTS:-$HERE/entitlements.plist}"
INFO_SRC="$HERE/Info.plist"
APP_NAME="BO1Z"
LIBS=(libEGL libGLESv2)

log()  { printf '[package] %s\n' "$*" >&2; }
warn() { printf '[package] WARNING: %s\n' "$*" >&2; }
die()  { printf '[package] ERROR: %s\n' "$*" >&2; exit 1; }

[[ "$(uname -s)" == "Darwin" ]] || die "must run on macOS"
PB=/usr/libexec/PlistBuddy
plutil -lint "$INFO_SRC" >/dev/null || die "Info.plist does not lint"
plutil -lint "$ENTITLEMENTS" >/dev/null || die "entitlements.plist does not lint"
EXE_NAME="$($PB -c 'Print :CFBundleExecutable' "$INFO_SRC")"
MIN_OS="$($PB -c 'Print :MinimumOSVersion' "$INFO_SRC")"

macho_platform() { otool -l "$1" | awk '/cmd LC_BUILD_VERSION/{f=1} f && !p && $1=="platform"{print $2; p=1}'; }
macho_minos()    { otool -l "$1" | awk '/cmd LC_BUILD_VERSION/{f=1} f && !p && $1=="minos"{print $2; p=1}'; }
ver_gt() { python3 -c 'import sys;v=lambda s:[int(x) for x in s.split(".")];sys.exit(0 if v(sys.argv[1])>v(sys.argv[2]) else 1)' "$1" "$2"; }

# ---------------------------------------------------------------- check the executable
[[ -f "$BIN" ]] || die "executable not found: $BIN (cargo build --release --target aarch64-apple-ios --manifest-path host/Cargo.toml)"
file -b "$BIN" | grep 'executable' >/dev/null || die "$BIN is not a Mach-O executable: $(file -b "$BIN")"
[[ "$(lipo -archs "$BIN")" == "arm64" ]] || die "$BIN archs are '$(lipo -archs "$BIN")', expected arm64"
plat="$(macho_platform "$BIN")"
[[ "$plat" == "2" || "$plat" == "IOS" ]] || die "$BIN platform '$plat' is not iOS device (wrong --target?)"
minos="$(macho_minos "$BIN")"
log "executable: $BIN  minos=$minos  size=$(du -h "$BIN" | cut -f1)"
if [[ -n "$minos" ]] && ver_gt "$minos" "$MIN_OS"; then
  die "executable minos $minos > MinimumOSVersion $MIN_OS - dyld would refuse it on iOS $MIN_OS; build with IPHONEOS_DEPLOYMENT_TARGET=$MIN_OS"
fi
deps="$(otool -L "$BIN")"
if grep 'OpenGLES.framework' <<<"$deps" >/dev/null; then
  warn "executable links Apple's OpenGLES.framework - its gl* symbols can shadow ANGLE's; resolve GL only via eglGetProcAddress / the libGLESv2 handle"
fi

# ---------------------------------------------------------------- stage the bundle
APP="$OUT_DIR/Payload/$APP_NAME.app"
rm -rf "$OUT_DIR/Payload" "$OUT_DIR/$APP_NAME.ipa"
mkdir -p "$APP/Frameworks"

cp "$BIN" "$APP/$EXE_NAME"
chmod 755 "$APP/$EXE_NAME"

cp "$INFO_SRC" "$APP/Info.plist"
SDK_VER="$(xcrun --sdk iphoneos --show-sdk-version)"
SDK_BUILD="$(xcrun --sdk iphoneos --show-sdk-build-version 2>/dev/null || true)"
XC_VER="$(xcodebuild -version | awk 'NR==1{print $2}')"          # e.g. 16.4
XC_BUILD="$(xcodebuild -version | awk '/Build version/{print $3}')"
DT_XCODE="$(awk -F. '{printf "%d%d%d", $1, ($2==""?0:$2), ($3==""?0:$3)}' <<<"$XC_VER")"   # 16.4 -> 1640
plutil -replace DTPlatformName    -string iphoneos           "$APP/Info.plist"
plutil -replace DTPlatformVersion -string "$SDK_VER"         "$APP/Info.plist"
plutil -replace DTSDKName         -string "iphoneos$SDK_VER" "$APP/Info.plist"
[[ -n "$SDK_BUILD" ]] && plutil -replace DTSDKBuild -string "$SDK_BUILD" "$APP/Info.plist"
plutil -replace DTXcode           -string "$DT_XCODE"        "$APP/Info.plist"
[[ -n "$XC_BUILD" ]]  && plutil -replace DTXcodeBuild -string "$XC_BUILD" "$APP/Info.plist"
[[ -n "${BUILD_NUMBER:-}" ]] && plutil -replace CFBundleVersion -string "$BUILD_NUMBER" "$APP/Info.plist"
plutil -convert binary1 "$APP/Info.plist"
printf 'APPL????' > "$APP/PkgInfo"

# ---------------------------------------------------------------- ANGLE frameworks
for l in "${LIBS[@]}"; do
  src="$ANGLE_DIR/$l.framework"
  [[ -f "$src/$l" ]] || die "missing $src/$l - run ios/fetch-or-build-angle.sh $ANGLE_DIR first"
  ditto "$src" "$APP/Frameworks/$l.framework"
  fb="$APP/Frameworks/$l.framework/$l"
  rm -rf "$APP/Frameworks/$l.framework/Headers" "$APP/Frameworks/$l.framework/Modules" \
         "$APP/Frameworks/$l.framework/_CodeSignature"
  if [[ "$(lipo -archs "$fb")" != "arm64" ]]; then
    lipo "$fb" -thin arm64 -output "$fb.thin" && mv -f "$fb.thin" "$fb"
  fi
  fplat="$(macho_platform "$fb")"
  [[ "$fplat" == "2" || "$fplat" == "IOS" ]] || die "$l is not an iOS device binary (platform $fplat)"
  fmin="$(macho_minos "$fb")"
  if [[ -n "$fmin" ]] && ver_gt "$fmin" "$MIN_OS"; then die "$l minos $fmin > $MIN_OS"; fi
  log "framework: $l  id=$(otool -D "$fb" | tail -n1)  minos=$fmin"
done

# Only needed if the host LINKS ANGLE (@rpath/...) instead of dlopen()ing it by path.
if grep -E '@rpath/(libEGL|libGLESv2)\.framework' <<<"$deps" >/dev/null; then
  if ! otool -l "$APP/$EXE_NAME" | grep '@executable_path/Frameworks' >/dev/null; then
    log "executable links ANGLE via @rpath - adding LC_RPATH @executable_path/Frameworks"
    install_name_tool -add_rpath @executable_path/Frameworks "$APP/$EXE_NAME" \
      || die "install_name_tool failed (header padding?) - link with -Wl,-rpath,@executable_path/Frameworks instead"
  fi
fi

# ---------------------------------------------------------------- sign
if $PB -c 'Print :dynamic-codesigning' "$ENTITLEMENTS" >/dev/null 2>&1; then
  warn "entitlements contain dynamic-codesigning: TrollStore README says such apps CRASH ON LAUNCH on A12+/iOS 15+"
fi

case "$SIGNER" in
  codesign)
    for l in "${LIBS[@]}"; do
      codesign --force --sign - --timestamp=none "$APP/Frameworks/$l.framework"
    done
    if ! codesign --force --sign - --timestamp=none --entitlements "$ENTITLEMENTS" \
           --generate-entitlement-der "$APP" 2>/dev/null; then
      codesign --force --sign - --timestamp=none --entitlements "$ENTITLEMENTS" "$APP"
    fi
    codesign --verify --deep --strict "$APP" || warn "codesign --verify complained (TrollStore re-signs anyway)"
    ;;
  ldid)
    command -v ldid >/dev/null || die "SIGNER=ldid but ldid is not installed (brew install ldid)"
    for l in "${LIBS[@]}"; do ldid -S "$APP/Frameworks/$l.framework/$l"; done
    ldid -S"$ENTITLEMENTS" "$APP/$EXE_NAME"
    ;;
  none)
    warn "SIGNER=none: no entitlements embedded - TrollStore will install without increased-memory-limit / EVA / get-task-allow"
    ;;
  *) die "SIGNER must be codesign|ldid|none" ;;
esac
if [[ "$SIGNER" != "none" ]]; then
  log "embedded entitlements:"
  codesign -d --entitlements - "$APP/$EXE_NAME" >&2 || true
fi

# ---------------------------------------------------------------- zip
( cd "$OUT_DIR" && zip -qry -X "$APP_NAME.ipa" Payload )
log "bundle contents:"
( cd "$OUT_DIR/Payload" && find "$APP_NAME.app" -maxdepth 3 -not -path '*/_CodeSignature/*' | sort ) >&2
log "wrote $OUT_DIR/$APP_NAME.ipa ($(du -h "$OUT_DIR/$APP_NAME.ipa" | cut -f1))"
