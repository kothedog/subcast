#!/usr/bin/env bash
# A local SubCast.app for trying changes (debug build), signed with the Developer ID like a release.
#
# Why sign: an ad-hoc build is known to the Keychain only by its hash, which changes on every build, so each
# new build asks for the login password again to read the saved API keys. Signed with the same identity and
# bundle id as the release, the Keychain recognises it as the same app: "Always Allow" once and it stays.
#
#   scripts/dev-app.sh            build, sign, open
#   scripts/dev-app.sh --no-open  build and sign only
set -euo pipefail
cd "$(dirname "$0")/.."

IDENTITY="${APPLE_SIGNING_IDENTITY:-Developer ID Application: xiangyang qiao (BRCU3DPFH4)}"
APP="src-tauri/target/debug/bundle/macos/SubCast.app"

if ! security find-identity -v -p codesigning | grep -qF "$IDENTITY"; then
  echo "Signing identity not found: $IDENTITY (set APPLE_SIGNING_IDENTITY)" >&2
  exit 1
fi

# The running copy holds the bundle open; quit it first.
osascript -e 'quit app id "ai.qiaomu.subcast"' >/dev/null 2>&1 || true

APPLE_SIGNING_IDENTITY="$IDENTITY" npx tauri build --debug --bundles app --no-sign
cargo build -p subcast-cli
cp src-tauri/target/debug/subcast-cli "$APP/Contents/MacOS/subcast-cli"
# Tauri signs when APPLE_SIGNING_IDENTITY is set; sign again explicitly so the result never depends on that.
codesign --force --deep --options runtime --entitlements src-tauri/Entitlements.plist --sign "$IDENTITY" "$APP"
codesign --verify --deep --strict "$APP"
codesign -d -r- "$APP" 2>&1 | grep designated

[[ "${1:-}" == "--no-open" ]] || open "$APP"
