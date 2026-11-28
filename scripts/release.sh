#!/usr/bin/env bash
# Release SubCast.
#   1. Build, sign and notarize the macOS app for Apple Silicon and Intel on this Mac.
#   2. Pull the Windows and Linux builds from the GitHub Actions draft release for the same tag.
#   3. Write latest.json (the in-app updater manifest, plus installer links for the website).
#   4. With --publish: upload everything to subcast.qiaomu.ai/download/ and attach the Mac files to the GitHub release.
#
# Usage: scripts/release.sh [--skip-mac] [--publish] [--notes "text"]
# Needs: ~/.tauri/subcast-updater.key, the Developer ID certificate in the keychain,
#        a notarytool keychain profile named "notary", gh logged in, ssh access to myvps,
#        create-dmg (brew install create-dmg) for the Mac disk images.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

SKIP_MAC=0; PUBLISH=0; NOTES=""
while [ $# -gt 0 ]; do
  case "$1" in
    --skip-mac) SKIP_MAC=1 ;;
    --publish) PUBLISH=1 ;;
    --notes) NOTES="$2"; shift ;;
    *) echo "unknown option: $1" >&2; exit 2 ;;
  esac
  shift
done

if [ "$SKIP_MAC" = 0 ] && ! command -v create-dmg >/dev/null; then
  echo "create-dmg is missing (it lays out the DMG window): brew install create-dmg" >&2
  exit 1
fi

VERSION=$(node -p 'require("./src-tauri/tauri.conf.json").version')
TAG="v$VERSION"
OUT="release/$VERSION"
KEY="$HOME/.tauri/subcast-updater.key"
IDENTITY="${APPLE_SIGNING_IDENTITY:-Developer ID Application: xiangyang qiao (BRCU3DPFH4)}"
NOTARY_PROFILE="${NOTARY_PROFILE:-notary}"
SSH_HOST="${SSH_HOST:-myvps}"
SITE_ROOT="${SITE_ROOT:-/www/wwwroot/subcast.qiaomu.ai}" # aaPanel vhost root, same as the website's deploy.sh
BASE_URL="https://subcast.qiaomu.ai/download/$VERSION"
mkdir -p "$OUT"

export TAURI_SIGNING_PRIVATE_KEY="$(cat "$KEY")"
export TAURI_SIGNING_PRIVATE_KEY_PASSWORD=""

notarize() { # file
  # Capture first, then match: piping into `grep -q` lets grep exit early, tee dies of SIGPIPE (141)
  # and pipefail + set -e abort the release even though Apple accepted the submission.
  local out
  out=$(xcrun notarytool submit "$1" --keychain-profile "$NOTARY_PROFILE" --wait 2>&1) || true
  printf '%s\n' "$out" >&2
  grep -q "status: Accepted" <<<"$out"
}

if [ "$SKIP_MAC" = 0 ]; then
  # DMG window background (source: scripts/dmg/background.svg). One TIFF with 1x and 2x, so Retina stays sharp.
  DMG_BG="$(mktemp -d)/background.tiff"
  tiffutil -cathidpicheck scripts/dmg/background.png scripts/dmg/background@2x.png -out "$DMG_BG" >/dev/null

  for TARGET in aarch64-apple-darwin x86_64-apple-darwin; do
    ARCH=$([ "$TARGET" = aarch64-apple-darwin ] && echo aarch64 || echo x64)
    rustup target add "$TARGET" >/dev/null
    echo "==> building $TARGET"
    # Line tables in a separate dSYM: the shipped binary stays stripped, and crash reports can be symbolicated
    # later (`atos -o <dSYM>/Contents/Resources/DWARF/subcast-* -arch arm64 -l <load address> <addresses>`).
    # Env vars, not Cargo.toml: profiles can't be per-target, and CI's Windows/Linux builds don't need it.
    export CARGO_PROFILE_RELEASE_DEBUG=line-tables-only CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO=packed
    APPLE_SIGNING_IDENTITY="$IDENTITY" npm run tauri build -- --target "$TARGET" --bundles app
    APP="src-tauri/target/$TARGET/release/bundle/macos/SubCast.app"
    cargo build --release --target "$TARGET" -p subcast-cli
    unset CARGO_PROFILE_RELEASE_DEBUG CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO
    # Keep the dSYMs (local only, release/ is gitignored). The UUIDs must match the shipped binaries.
    DSYM_DIR="release/dsym/$VERSION/$ARCH"
    rm -rf "$DSYM_DIR" && mkdir -p "$DSYM_DIR"
    for BIN in subcast subcast-cli; do
      cp -RL "src-tauri/target/$TARGET/release/$BIN.dSYM" "$DSYM_DIR/" # -L: cargo links it into deps/
      [ "$(dwarfdump --uuid "$DSYM_DIR/$BIN.dSYM" | awk '{print $2}')" = "$(dwarfdump --uuid "src-tauri/target/$TARGET/release/$BIN" | awk '{print $2}')" ] \
        || { echo "dSYM UUID mismatch for $BIN ($ARCH)" >&2; exit 1; }
    done
    cp "src-tauri/target/$TARGET/release/subcast-cli" "$APP/Contents/MacOS/subcast-cli"
    codesign --force --options runtime --timestamp --entitlements src-tauri/Entitlements.plist --sign "$IDENTITY" "$APP/Contents/MacOS/subcast-cli"
    codesign --force --options runtime --timestamp --entitlements src-tauri/Entitlements.plist --sign "$IDENTITY" "$APP"
    codesign --verify --deep --strict "$APP"

    echo "==> notarizing the app ($ARCH)"
    ZIP="$(mktemp -d)/SubCast.zip"
    ditto -c -k --keepParent "$APP" "$ZIP"
    notarize "$ZIP"
    xcrun stapler staple "$APP"

    # The updater package has to be made after stapling, so Tauri's own tarball is replaced.
    TGZ="$OUT/SubCast_${VERSION}_${ARCH}.app.tar.gz"
    COPYFILE_DISABLE=1 tar -czf "$TGZ" -C "$(dirname "$APP")" SubCast.app
    rm -f "$TGZ.sig"
    # The build exports TAURI_SIGNING_PRIVATE_KEY; the signer refuses it together with -f.
    env -u TAURI_SIGNING_PRIVATE_KEY -u TAURI_SIGNING_PRIVATE_KEY_PASSWORD npx tauri signer sign -f "$KEY" -p "" "$TGZ" >/dev/null

    echo "==> disk image ($ARCH)"
    DMG="$OUT/SubCast_${VERSION}_${ARCH}.dmg"
    STAGE="$(mktemp -d)"
    cp -R "$APP" "$STAGE/"
    rm -f "$DMG"
    # The usual macOS layout: the app on the left, an Applications link on the right, an arrow and a
    # drag hint in the background. The positions match scripts/dmg/background.svg; change them together.
    # create-dmg drives Finder over AppleScript, so the first run asks to let Terminal control Finder.
    create-dmg \
      --volname "SubCast" \
      --background "$DMG_BG" \
      --window-pos 200 120 \
      --window-size 660 400 \
      --icon-size 128 \
      --text-size 13 \
      --icon "SubCast.app" 170 180 \
      --hide-extension "SubCast.app" \
      --app-drop-link 490 180 \
      --hdiutil-quiet \
      "$DMG" "$STAGE" >/dev/null
    codesign --sign "$IDENTITY" --timestamp "$DMG"
    notarize "$DMG"
    xcrun stapler staple "$DMG"
    spctl -a -t open --context context:primary-signature -v "$DMG"
  done
fi

echo "==> Windows and Linux builds from the $TAG GitHub release"
if gh release view "$TAG" >/dev/null 2>&1; then
  gh release download "$TAG" -D "$OUT" --clobber \
    -p '*-setup.exe' -p '*-setup.exe.sig' -p '*.msi' -p '*.AppImage' -p '*.AppImage.sig' -p '*.deb' -p '*.rpm' || true
else
  echo "   no release $TAG yet; push the tag and let Actions finish to include Windows and Linux" >&2
fi

echo "==> latest.json"
VERSION="$VERSION" BASE_URL="$BASE_URL" NOTES="$NOTES" OUT="$OUT" node <<'JS'
const fs = require("fs"), path = require("path");
const { VERSION, BASE_URL, NOTES, OUT } = process.env;
const files = fs.readdirSync(OUT);
const find = (re) => files.find((f) => re.test(f));
const url = (f) => `${BASE_URL}/${encodeURIComponent(f)}`;
const platforms = {};
const add = (key, re) => {
  const f = find(re);
  if (f && files.includes(f + ".sig")) platforms[key] = { signature: fs.readFileSync(path.join(OUT, f + ".sig"), "utf8").trim(), url: url(f) };
};
add("darwin-aarch64", /_aarch64\.app\.tar\.gz$/);
add("darwin-x86_64", /_x64\.app\.tar\.gz$/);
add("windows-x86_64", /-setup\.exe$/);
add("linux-x86_64", /\.AppImage$/);
const dl = {};
const link = (key, re) => { const f = find(re); if (f) dl[key] = { url: url(f), size: fs.statSync(path.join(OUT, f)).size }; };
link("mac-arm", /_aarch64\.dmg$/);
link("mac-intel", /_x64\.dmg$/);
link("windows", /-setup\.exe$/);
link("windows-msi", /\.msi$/);
link("linux-appimage", /\.AppImage$/);
link("linux-deb", /\.deb$/);
link("linux-rpm", /\.rpm$/);
const manifest = { version: VERSION, notes: NOTES || `SubCast ${VERSION}`, pub_date: new Date().toISOString(), platforms, downloads: dl };
fs.writeFileSync(path.join(OUT, "latest.json"), JSON.stringify(manifest, null, 2) + "\n");
console.log("platforms:", Object.keys(platforms).join(", ") || "(none)");
console.log("downloads:", Object.keys(dl).join(", ") || "(none)");
JS

# Version-free names, so a link always gets the newest build:
#   https://subcast.qiaomu.ai/download/latest/<name>   (symlinks on the server)
#   https://github.com/joeseesun/subcast/releases/latest/download/<name>   (copies on the release)
stable_pairs() { # prints "versioned-file stable-name" lines for what exists in $OUT
  for pair in "_aarch64.dmg:SubCast-mac-apple-silicon.dmg" "_x64.dmg:SubCast-mac-intel.dmg" "-setup.exe:SubCast-windows-setup.exe" \
              ".msi:SubCast-windows.msi" ".AppImage:SubCast-linux.AppImage" ".deb:SubCast-linux.deb" ".rpm:SubCast-linux.rpm"; do
    f=$(cd "$OUT" && ls -1 *"${pair%%:*}" 2>/dev/null | grep -v '\.sig$' | head -1 || true)
    [ -n "$f" ] && echo "$f ${pair#*:}"
  done
}

if [ "$PUBLISH" = 1 ]; then
  echo "==> uploading to $SSH_HOST:$SITE_ROOT/download/"
  ssh "$SSH_HOST" "mkdir -p '$SITE_ROOT/download/$VERSION' '$SITE_ROOT/download/latest'"
  rsync -av --progress --exclude latest.json "$OUT/" "$SSH_HOST:$SITE_ROOT/download/$VERSION/"
  LINKS=$(stable_pairs | while read -r f name; do printf "ln -sfn '../%s/%s' '%s/download/latest/%s'; " "$VERSION" "$f" "$SITE_ROOT" "$name"; done)
  ssh "$SSH_HOST" "$LINKS"
  # latest.json goes last so clients never see a manifest whose files are not there yet.
  rsync -av "$OUT/latest.json" "$SSH_HOST:$SITE_ROOT/download/latest.json"
  if gh release view "$TAG" >/dev/null 2>&1; then
    STABLE="$(mktemp -d)"
    stable_pairs | while read -r f name; do cp "$OUT/$f" "$STABLE/$name"; done
    gh release upload "$TAG" --clobber "$OUT"/*.dmg "$OUT"/*.app.tar.gz "$OUT"/*.app.tar.gz.sig "$OUT/latest.json" "$STABLE"/*
  fi
  echo "published $VERSION"
else
  echo "built into $OUT (run again with --skip-mac --publish to upload)"
fi
