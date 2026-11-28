#!/usr/bin/env bash
# macOS development; prefer project-local tools when installed.
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Darwin ]; then
  echo 'This development entry point supports macOS only.' >&2
  exit 1
fi
TOOLS="$PWD/.dev-tools"
export PATH="$TOOLS/node/bin:$TOOLS/cargo/bin:$TOOLS/cmake/CMake.app/Contents/bin:$PATH"
if [ -d "$TOOLS/rustup" ]; then
  export RUSTUP_HOME="$TOOLS/rustup"
  export CARGO_HOME="$TOOLS/cargo"
fi
export npm_config_cache="$TOOLS/npm-cache"
ACTION="${1:-doctor}"
if [ $# -gt 0 ]; then shift; fi
case "$ACTION" in
  doctor)
    node --version
    npm --version
    rustc --version
    cargo --version
    cmake --version
    xcode-select -p
    xcrun --sdk macosx --show-sdk-version
    xcrun metal --version
    ;;
  install) npm ci "$@" ;;
  check)
    FAILED=0
    ./node_modules/.bin/tsc --noEmit -p . || FAILED=1
    npm run i18n -- --strict || FAILED=1
    npm run build || FAILED=1
    node src/lib/subtitleText.test.ts || FAILED=1
    cargo test --locked --workspace --lib "$@" || FAILED=1
    exit "$FAILED"
    ;;
  dev) npm run tauri dev -- --config src-tauri/tauri.macos-dev.conf.json "$@" ;;
  build) npm run tauri build -- --debug --bundles app --config src-tauri/tauri.macos-dev.conf.json "$@" ;;
  *)
    echo 'Usage: bash scripts/macos-dev.sh [doctor|install|check|dev|build]' >&2
    exit 2
    ;;
esac
