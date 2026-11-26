#!/usr/bin/env bash
# Package Medis as a macOS .app bundle and .dmg.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DIST="$ROOT/dist"
APP="$DIST/Medis.app"
PLIST_TEMPLATE="$ROOT/packaging/macos/Info.plist.in"
ICON="$ROOT/crates/medis-app/assets/Medis.icns"
BUNDLE_ID="${BUNDLE_ID:-com.medis.app}"
# TARGET: a Rust target triple to build for (e.g. x86_64-apple-darwin for Intel Macs).
# Unset = the host's architecture.
TARGET="${TARGET:-}"
case "$TARGET" in
  x86_64-apple-darwin) ARCH="x86_64" ;;
  aarch64-apple-darwin) ARCH="arm64" ;;
  "") ARCH="$(uname -m)" ;;
  *) echo "Unsupported TARGET: $TARGET" >&2; exit 2 ;;
esac

SKIP_BUILD=0
NO_DMG=0
for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=1 ;;
    --no-dmg) NO_DMG=1 ;;
    -h|--help)
      echo "Usage: $0 [--skip-build] [--no-dmg]"
      echo "Env: TARGET, SIGN_IDENTITY, NOTARY_PROFILE, BUNDLE_ID, CARGO_TARGET_DIR"
      exit 0 ;;
    *) echo "Unknown argument: $arg" >&2; exit 2 ;;
  esac
done

cd "$ROOT"

# Version from cargo metadata
VERSION="$(cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys
m = json.load(sys.stdin)
print(next(p["version"] for p in m["packages"] if p["name"] == "medis-app"))
')"
echo "==> Medis version $VERSION"

# Resolve target dir
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
case "$TARGET_DIR" in /*) ;; *) TARGET_DIR="$ROOT/$TARGET_DIR" ;; esac
if [[ -n "$TARGET" ]]; then
  BIN="$TARGET_DIR/$TARGET/release/medis"
  TARGET_ARGS=(--target "$TARGET")
else
  BIN="$TARGET_DIR/release/medis"
  TARGET_ARGS=()
fi

if [[ $SKIP_BUILD -eq 0 ]]; then
  echo "==> cargo build --release -p medis-app ${TARGET_ARGS[*]:-}"
  cargo build --release -p medis-app ${TARGET_ARGS[@]+"${TARGET_ARGS[@]}"}
else
  echo "==> Skipping build"
fi
[[ -f "$BIN" ]] || { echo "Binary not found: $BIN" >&2; exit 1; }

# Assemble bundle
echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/medis"
chmod 755 "$APP/Contents/MacOS/medis"
cp "$ICON" "$APP/Contents/Resources/Medis.icns"
sed -e "s|@VERSION@|$VERSION|g" -e "s|@BUNDLE_ID@|$BUNDLE_ID|g" \
  "$PLIST_TEMPLATE" > "$APP/Contents/Info.plist"
plutil -lint "$APP/Contents/Info.plist"

# Sign
if [[ -n "${SIGN_IDENTITY:-}" ]]; then
  echo "==> Signing with identity: $SIGN_IDENTITY (hardened runtime)"
  codesign --force --deep --options runtime --timestamp -s "$SIGN_IDENTITY" "$APP"
else
  echo "==> Ad-hoc signing"
  codesign --force --deep -s - "$APP"
fi
codesign --verify --deep --strict "$APP"

# DMG (needed for notarization too, but we notarize the app via a zip)
notarize() {
  echo "==> Notarizing (profile: $NOTARY_PROFILE)"
  local zip="$DIST/Medis-notarize.zip"
  rm -f "$zip"
  ditto -c -k --keepParent "$APP" "$zip"
  xcrun notarytool submit "$zip" --keychain-profile "$NOTARY_PROFILE" --wait
  rm -f "$zip"
  xcrun stapler staple "$APP"
}
if [[ -n "${NOTARY_PROFILE:-}" ]]; then
  if [[ -z "${SIGN_IDENTITY:-}" ]]; then
    echo "NOTARY_PROFILE requires SIGN_IDENTITY (a Developer ID identity)" >&2
    exit 1
  fi
  notarize
else
  echo "==> Skipping notarization (NOTARY_PROFILE not set)"
fi

DMG=""
if [[ $NO_DMG -eq 0 ]]; then
  DMG="$DIST/Medis-$VERSION-macos-$ARCH.dmg"
  echo "==> Creating $DMG"
  STAGE="$(mktemp -d "${TMPDIR:-/tmp}/medis-dmg.XXXXXX")"
  trap 'rm -rf "$STAGE"' EXIT
  cp -R "$APP" "$STAGE/Medis.app"
  ln -s /Applications "$STAGE/Applications"
  rm -f "$DMG"
  hdiutil create -volname "Medis" -srcfolder "$STAGE" -ov -format UDZO "$DMG"
  if [[ -n "${SIGN_IDENTITY:-}" ]]; then
    codesign --force --timestamp -s "$SIGN_IDENTITY" "$DMG"
  fi
fi

echo
echo "==> Done"
echo "App: $APP"
[[ -n "$DMG" ]] && echo "DMG: $DMG"
exit 0
