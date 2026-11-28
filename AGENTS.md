# Agent notes for SubCast

Read before changing or shipping anything here.

## Fork policy (kothedog/subcast)

- Develop and validate macOS only. Keep upstream history and merge upstream updates into this fork, resolving conflicts while preserving fork customizations.
- The only destination for pushes and pull requests is kothedog/subcast. joeseesun/subcast is a read-only upstream source: never push there or open a pull request against it unless the owner explicitly changes this policy.
- Keep fork changes on short branches and merge through pull requests in kothedog/subcast. When asked to sync upstream, merge into the fork; do not replace its branch or tree with upstream content once this fork has customizations.
- Configure local clones with origin as the default push remote and a disabled upstream push URL. A public fork remains publicly visible; this policy controls writes, not visibility.

## Development rules

- **Shipping**: [docs/RELEASING.md](docs/RELEASING.md) decides between an interface hot update (`scripts/ui-release.sh`, front end only, no notarization) and an app release (`scripts/release.sh`, anything in `src-tauri/`). Follow it; don't improvise a release.
- **Architecture**: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Server pieces: [server/deploy/README.md](server/deploy/README.md).
- **Local test build**: `scripts/dev-app.sh` builds a debug SubCast.app signed with the Developer ID, so the Keychain doesn't ask for the login password on every build (ad-hoc builds change identity each time). It serves the interface on http://localhost like a release (from port 14337 up), so YouTube's embedded player can be tried; `tauri dev` uses the Vite server.
- **Checks before a PR**: `npx tsc --noEmit -p .`, `npm run i18n -- --strict`, `npm run build` (the bundler catches syntax errors `tsc` misses, e.g. in `src/locales/*.ts`), `cargo test --lib` in `src-tauri`, `node src/lib/subtitleText.test.ts` (clip subtitle rules). Interface text goes through `t("中文")` with translations in `src/locales/{en,ja,zh-TW}.ts`.
- **Git**: short branch → PR → merge commit; never push to `main`.
- **Tauri commands**: a new command needs the Rust function, `invoke_handler`, `src-tauri/build.rs` COMMANDS and `capabilities/default.json`. That makes it an app release, not a hot update. An async command returns `Res<T>` and wraps its body in `guard::guarded(async move { … }).await`, so a panic becomes an error instead of a promise that never settles (`src-tauri/src/guard.rs`).
- **WebView traps**: no `window.confirm/alert/prompt` (use `ask()` from `src/lib/confirm.ts`); select store slices with `useShallow`, never subscribe to the whole store; never define components inside components.
- **Secrets**: never in the repo, logs or prompts (updater key, Apple credentials, server tokens).
- **Design**: no vertical side-stripe accents (border-left bars); reply to the owner in Chinese.
