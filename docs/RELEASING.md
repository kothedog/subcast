# Releasing SubCast

The handbook for anyone (people or agents) shipping a change. Read it before releasing; update it when the process changes.

## Pick the channel

| What changed | Channel | Reaches users | Command |
|---|---|---|---|
| Only `src/` (React, CSS, text, translations), and it uses no new Rust command | **Interface hot update** | Next launch, a few hours after publishing (all platforms) | `scripts/ui-release.sh` |
| Anything in `src-tauri/` (Rust, `tauri.conf.json`, capabilities, Cargo deps), new commands, new permissions | **App release** | In-app updater, then restart | tag + `scripts/release.sh` |
| The server: shared subtitles API, yt-dlp mirror | **Server** | Immediately | see `server/deploy/README.md` |

When unsure, it is an app release. `ui-release.sh` refuses to run when `src-tauri/` differs from the `v<version>` tag.

Never ship through the hot update: purchase or pricing flows, permissions, features that change what the app is for (App Store rules 2.5.2 and 3.3.1 when the iOS app exists), or anything a reviewer hasn't seen.

## Interface hot update

How it works (`src-tauri/src/ota.rs`, modeled on Expo Updates and Capgo):

- The app checks `https://subcast.qiaomu.ai/tools/ui/manifest.json` with the app update check (10 s after launch, then every 6 h), when there is no new app version.
- The manifest is signed with the **same minisign key as app updates** (`~/.tauri/subcast-updater.key`); the archive must match its SHA-256 and size.
- A bundle names the exact app version it was built against (`shell`); other versions ignore it. A new app version starts from its own built-in interface.
- It applies on the **next launch**, never while the app is open.
- **Self-healing**: the page calls `ui_ready` after it renders. If that doesn't happen within 15 s, or the app quits before it, the bundle is marked failed for good and the last good bundle (or the built-in one) is used. Failed builds are never retried; ship the fix as a new build.
- **No downgrades**: only builds above the highest seen are accepted.
- Settings › About shows "界面更新 N" when a hot update is in use.

Steps:

```bash
git switch main && git pull                 # the fix is merged through a PR like any change
scripts/ui-release.sh --dry-run             # builds and signs into release/ui/<version>/, uploads nothing
scripts/ui-release.sh --rollout 20 --notes "修复：…"   # 20% of installs first
# check feedback; then everyone:
scripts/ui-release.sh --notes "修复：…"
```

Each run publishes a new build number (server build + 1). Staged rollout is by install (a stable random id per install), so the same 20% get it each time.

Something wrong:

```bash
scripts/ui-release.sh --rollback            # everyone on this app version back to the built-in interface
```

Then fix, merge, and publish a new build.

Before publishing, test the bundle in the real app: build the app once with `SUBCAST_UI_MANIFEST=<test manifest url>` in the environment, or install the archive by hand into `~/Library/Application Support/ai.qiaomu.subcast/ui/<build>/` and set `staged` in `ui/state.json`.

Server files: `/www/wwwroot/subcast-tools/ui/` on myvps, outside the website root (website deploys use `rsync --delete` and must never touch it).

## Remote config

`https://subcast.qiaomu.ai/tools/config.json` (+ `.sig`), source in `server/deploy/remote-config.json`. Values that go stale outside our control and live on the Rust side, so a hot update can't change them. Today: `youtubeClients` (the YouTube API client names and versions caption fetching uses; when captions stop loading for everyone, try newer versions here first). Also `curatedYoutube` (optional): rss.qiaomu.ai / 精读 source id → YouTube channel id (`UC…`) or `"none"`, to correct or add the channel a curated show's videos come from (`crates/subcast-core/src/curated_video.rs` has the built-in table and finds new shows by name). Fetched with every update check, signed with the updater key, last good copy kept in settings. Publish:

```bash
cp server/deploy/remote-config.json /tmp/config.json && rm -f /tmp/config.json.sig
env -u TAURI_SIGNING_PRIVATE_KEY -u TAURI_SIGNING_PRIVATE_KEY_PASSWORD npx tauri signer sign -f ~/.tauri/subcast-updater.key -p "" /tmp/config.json
rsync -a /tmp/config.json.sig myvps:/www/wwwroot/subcast-tools/config.json.sig
rsync -a /tmp/config.json myvps:/www/wwwroot/subcast-tools/config.json
```

Front-end values (recommended shows, prompts, model lists) change through an interface hot update instead.

## App release

1. Branch → PR → merge (never push to `main`). Bump the version in `package.json`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`. Write `release/notes-<version>.md` (Chinese first, then English; user-facing: New / Fixed / Not verified / Upgrading). `release/` is gitignored; the notes go into the GitHub release body.
2. Before merging, run the CI build on the branch: `gh workflow run build.yml --ref <branch>`. The Windows job checks that `subcast.exe` imports no Visual C++ runtime and that the installer carries `vcrt/`.
3. Tag after merging: `git tag v<version> && git push origin v<version>`. CI builds Windows and Linux into a draft release.
4. Mac (on the Mac, screen unlocked so the `notary` keychain profile can be read; `brew install create-dmg` once): `scripts/release.sh` builds both architectures, notarizes the apps and DMGs, staples, re-makes and signs the updater tarballs, and writes `latest.json`. It also keeps each architecture's dSYMs (`subcast`, `subcast-cli`) in `release/dsym/<version>/<arch>/` and stops if a dSYM's UUID differs from its binary. They stay on this Mac: `release/<version>/` is what gets uploaded, `release/dsym/` is not. The DMG window (app left, Applications right, background with a drag hint) comes from `scripts/dmg/`; after editing `background.svg`, run `scripts/dmg/render.sh` and commit the PNGs.
5. When CI is done: `scripts/release.sh --skip-mac --publish --notes "<one line>"`. Uploads to `subcast.qiaomu.ai/download/<version>/`, points `/download/latest/<stable names>` at it, uploads `latest.json` **last**, and attaches Mac files and stable-named copies to the GitHub release.
6. Publish the draft GitHub release with the notes file: `gh release edit v<version> --draft=false --latest --notes-file release/notes-<version>.md`.
7. Verify from outside: `latest.json` shows the version and four platforms; every link under `/download/latest/` and `github.com/joeseesun/subcast/releases/latest/download/` returns 200; `spctl -a -t open --context context:primary-signature -v` on a downloaded DMG says "Notarized Developer ID"; an install of the previous version updates in-app.
8. Report what was and wasn't verified (Windows/Linux hardware, Windows has no Authenticode signature yet).

Keys and credentials (never in the repo or in prompts): updater key `~/.tauri/subcast-updater.key` (also the GitHub secret `TAURI_SIGNING_PRIVATE_KEY`); Developer ID certificate in the login keychain; notarization profile `notary` in the keychain.

## Reading a crash

- Every Rust panic goes to `panic.log` in the app log folder (macOS: `~/Library/Logs/ai.qiaomu.subcast/`; `src-tauri/src/panic_log.rs`). Each line has the time, version, thread, message and `file:line:col`. Start there.
- macOS crash reports are in `~/Library/Logs/DiagnosticReports/subcast-*.ips`. Match the binary's UUID (`usedImages`) to a dSYM with `dwarfdump --uuid`, then: `atos -o release/dsym/<version>/aarch64/subcast.dSYM/Contents/Resources/DWARF/subcast-* -arch arm64 -l <load address> <frame addresses>`. The load address is the `base` of the `subcast` image in the report.
- Releases up to 0.3.8 have no dSYM.
- A panic inside a Tauri command doesn't crash or hang the app: the call fails with a fixed "内部错误" message (`src-tauri/src/guard.rs`), and `panic.log` has the details.
- Never set `panic = "abort"` in the release profile: wry's `objc2::exception::catch` needs unwinding (see `Cargo.toml`).

## Known traps

- `cmd | grep -q` under `set -o pipefail` can exit 141 (SIGPIPE) after a successful step. Capture output first.
- `tauri signer sign -f` refuses when `TAURI_SIGNING_PRIVATE_KEY` is in the environment: run it under `env -u TAURI_SIGNING_PRIVATE_KEY -u TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
- Publishing a draft release re-triggers CI; CI must never upload Mac assets (it would overwrite the notarized DMG).
- The website deploy (`subcast-website/scripts/deploy.sh`) excludes `/download/`; keep it that way. `/tools/` (yt-dlp mirror, interface updates) lives outside the site root.
- The DMG layout is written by Finder through AppleScript (create-dmg). The first run asks to let Terminal control Finder; if that was denied, allow it in System Settings › Privacy & Security › Automation, or the DMG comes out with the default white window.
- In-app updates download the package from whichever of `subcast.qiaomu.ai/download/<version>/` and the GitHub release `v<version>` answers faster (`src-tauri/src/appupdate.rs`), so both must carry the updater files under the same names (`release.sh --publish` attaches them). The signature check is the same either way.
- The release build serves the interface from `http://localhost:14327` (YouTube refuses `tauri://`), or the next free port up to 14346 when another program holds 14327 on IPv4 or IPv6 (the mobile build in the iOS Simulator did, and the desktop window showed its pages). All 20 origins are allowed in `capabilities/default.json`, and a test keeps that list and `UI_PORTS` in step; a port that isn't listed breaks every command. After installing a test build, look at the window before handing it over: if it looks wrong, run `lsof -nP -iTCP:14327 -sTCP:LISTEN` first.
