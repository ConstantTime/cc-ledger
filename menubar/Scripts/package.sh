#!/usr/bin/env bash
# Build and package MenuBar.app — a proper macOS app bundle
# with LSUIElement so it lives only in the menu bar.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CONFIG="${1:-debug}"
APP="$ROOT/MenuBar.app"

echo "==> Building ($CONFIG)"
swift build -c "$CONFIG"

BIN_DIR=".build/$CONFIG"
[[ "$CONFIG" == "release" ]] && BIN_DIR=".build/release"

echo "==> Assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"
mkdir -p "$APP/Contents/Resources"

cp "$BIN_DIR/MenuBar" "$APP/Contents/MacOS/MenuBar"
cp "Resources/Info.plist" "$APP/Contents/Info.plist"

# Sign with $APP_IDENTITY if set (stable identity → Keychain "Always Allow"
# sticks across rebuilds). Falls back to ad-hoc, which works but re-prompts
# Keychain on every rebuild because the signature changes.
# Run ./Scripts/setup_dev_signing.sh once to create a stable dev cert.
CODESIGN_ID="${APP_IDENTITY:--}"
if [[ "$CODESIGN_ID" == "-" ]]; then
    echo "==> Signing (ad-hoc — set APP_IDENTITY to skip Keychain prompts)"
else
    echo "==> Signing as: $CODESIGN_ID"
fi
codesign --force --sign "$CODESIGN_ID" "$APP" >/dev/null

echo "==> Done: $APP"
