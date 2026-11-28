#!/usr/bin/env bash
# Mirrors the latest yt-dlp release for the app (https://subcast.qiaomu.ai/tools/yt-dlp/), so first-time setup
# doesn't depend on reaching GitHub. Every file is checked against the release's SHA2-256SUMS before it replaces
# the old one; VERSION is written last, so clients never see a version whose files are not in place.
# Cron: 17 */6 * * * root /opt/qiaomu-apps/subcast-api/deploy/sync-ytdlp.sh >> /var/log/subcast-ytdlp.log 2>&1
set -euo pipefail
DEST=${DEST:-/www/wwwroot/subcast-tools/yt-dlp}
FILES=(yt-dlp_macos yt-dlp.exe yt-dlp_linux yt-dlp_linux_aarch64)
mkdir -p "$DEST"
tag=$(curl -fsSL -H "Accept: application/vnd.github+json" https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest | python3 -c 'import json,sys; print(json.load(sys.stdin)["tag_name"])')
[ -n "$tag" ] || { echo "no tag"; exit 1; }
if [ "$(cat "$DEST/VERSION" 2>/dev/null)" = "$tag" ] && [ "${FORCE:-0}" != 1 ]; then exit 0; fi
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
base="https://github.com/yt-dlp/yt-dlp/releases/download/$tag"
curl -fsSL -o "$work/SHA2-256SUMS" "$base/SHA2-256SUMS"
for f in "${FILES[@]}"; do curl -fsSL --retry 3 -o "$work/$f" "$base/$f"; done
(cd "$work" && grep -E " (\*)?($(IFS='|'; echo "${FILES[*]}" | sed 's/\./\\./g'))\$" SHA2-256SUMS | sha256sum -c --strict -) 
chmod 644 "$work"/*
for f in "${FILES[@]}" SHA2-256SUMS; do mv -f "$work/$f" "$DEST/$f"; done
echo "$tag" > "$DEST/VERSION.tmp" && mv -f "$DEST/VERSION.tmp" "$DEST/VERSION"
echo "$(date -u +%FT%TZ) mirrored yt-dlp $tag"
