#!/usr/bin/env bash
# Build a shareable Dev Pilot Board release package.
#
#   scripts/build-release.sh            # universal (arm64 + x86_64) release build
#   scripts/build-release.sh --debug    # fast debug build, host arch only
#   scripts/build-release.sh --skip-build   # repackage the last build
#
# Output: dist/dev-pilot-board-<version>/ and dist/dev-pilot-board-<version>.zip
# Prints the zip path and its sha256 (needed for the Homebrew cask).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP_DIR="$ROOT/app"
APP_NAME="Dev Pilot Board"
PKG_NAME="dev-pilot-board"

MODE="release"
SKIP_BUILD=0
for arg in "$@"; do
  case "$arg" in
    --debug) MODE="debug" ;;
    --skip-build) SKIP_BUILD=1 ;;
    -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
    *) echo "unknown arg: $arg" >&2; exit 2 ;;
  esac
done

# --- toolchain -----------------------------------------------------------
[ -f "$HOME/.cargo/env" ] && source "$HOME/.cargo/env"
for bin in cargo node npm jq ditto lipo; do
  command -v "$bin" >/dev/null || { echo "missing: $bin" >&2; exit 1; }
done

# --- version consistency -------------------------------------------------
V_TAURI=$(jq -r .version "$APP_DIR/src-tauri/tauri.conf.json")
V_NPM=$(jq -r .version "$APP_DIR/package.json")
V_CARGO=$(sed -n 's/^version = "\(.*\)"/\1/p' "$APP_DIR/src-tauri/Cargo.toml" | head -1)
if [ "$V_TAURI" != "$V_NPM" ] || [ "$V_TAURI" != "$V_CARGO" ]; then
  echo "version mismatch: tauri.conf.json=$V_TAURI package.json=$V_NPM Cargo.toml=$V_CARGO" >&2
  exit 1
fi
VERSION="$V_TAURI"
if git -C "$ROOT" rev-parse "v$VERSION" >/dev/null 2>&1 && [ "$MODE" = "release" ]; then
  echo "note: tag v$VERSION already exists — bump the version unless this is a packaging-only refresh" >&2
fi

# --- build ---------------------------------------------------------------
if [ "$MODE" = "release" ]; then
  TARGET_DIR="$APP_DIR/src-tauri/target/universal-apple-darwin/release/bundle/macos"
else
  TARGET_DIR="$APP_DIR/src-tauri/target/debug/bundle/macos"
fi
APP_BUNDLE="$TARGET_DIR/$APP_NAME.app"

if [ "$SKIP_BUILD" = 0 ]; then
  # The DMG step fails while the app is running; the .app still builds.
  pkill -f "$APP_NAME.app/Contents/MacOS/app" 2>/dev/null || true
  cd "$APP_DIR"
  [ -d node_modules ] || npm install
  if [ "$MODE" = "release" ]; then
    rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null
    npm run tauri build -- --target universal-apple-darwin --bundles app
  else
    npm run tauri build -- --debug --bundles app
  fi
fi

[ -d "$APP_BUNDLE" ] || { echo "no app bundle at $APP_BUNDLE" >&2; exit 1; }

BIN="$APP_BUNDLE/Contents/MacOS/app"
ARCHS=$(lipo -archs "$BIN")
if [ "$MODE" = "release" ] && [[ "$ARCHS" != *arm64* || "$ARCHS" != *x86_64* ]]; then
  echo "expected universal binary, got: $ARCHS" >&2; exit 1
fi
# macOS keys TCC permissions (Bluetooth, Local Network) to the code-signing
# IDENTIFIER. Tauri's ad-hoc signature uses "app-<hash of the binary>", so every
# rebuild looks like a brand-new app, the user's grant is silently lost and
# macOS does not reliably re-prompt — Bluetooth then sits at NotDetermined
# forever. Pin it to the bundle id so a grant survives upgrades.
BUNDLE_ID=$(jq -r '.identifier' "$APP_DIR/src-tauri/tauri.conf.json")
codesign --force --deep --sign - --identifier "$BUNDLE_ID" "$APP_BUNDLE" 2>/dev/null
SIGNED_AS=$(codesign -dv "$APP_BUNDLE" 2>&1 | sed -n 's/^Identifier=//p')
[ "$SIGNED_AS" = "$BUNDLE_ID" ] || { echo "signing identifier is $SIGNED_AS, expected $BUNDLE_ID" >&2; exit 1; }

BUNDLE_VERSION=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$APP_BUNDLE/Contents/Info.plist")
[ "$BUNDLE_VERSION" = "$VERSION" ] || { echo "bundle version $BUNDLE_VERSION != $VERSION" >&2; exit 1; }

# --- package -------------------------------------------------------------
DIST="$ROOT/dist"
SUFFIX=""; [ "$MODE" = "debug" ] && SUFFIX="-debug"
PKG_DIR="$DIST/$PKG_NAME-$VERSION$SUFFIX"
ZIP="$PKG_DIR.zip"
rm -rf "$PKG_DIR" "$ZIP"
mkdir -p "$PKG_DIR"
ditto "$APP_BUNDLE" "$PKG_DIR/$APP_NAME.app"
cp "$ROOT/notify.sh" "$ROOT/scripts/install.sh" "$ROOT/scripts/uninstall.sh" "$ROOT/scripts/INSTALL.md" "$PKG_DIR/"
chmod +x "$PKG_DIR/notify.sh" "$PKG_DIR/install.sh" "$PKG_DIR/uninstall.sh"
xattr -cr "$PKG_DIR"
(cd "$DIST" && ditto -c -k --norsrc --keepParent "$(basename "$PKG_DIR")" "$(basename "$ZIP")")

SHA=$(shasum -a 256 "$ZIP" | cut -d' ' -f1)
echo
echo "version : $VERSION ($MODE, $ARCHS)"
echo "zip     : $ZIP"
echo "size    : $(du -h "$ZIP" | cut -f1)"
echo "sha256  : $SHA"
