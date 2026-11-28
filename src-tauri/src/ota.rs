//! Hot updates for the interface (the HTML, JS and CSS in `dist/`), so a front-end fix reaches users without a
//! signed, notarized app release. Modeled on Expo Updates and Capgo:
//!
//! - **Signed**: `manifest.json` is verified with the app updater's minisign public key (from tauri.conf.json);
//!   the archive must match the manifest's SHA-256 and size.
//! - **Exact shell match**: a bundle names the one app version it was built against (`shell`). Changes that need
//!   new Rust commands never go out this way; they ship as an app release.
//! - **Never bricks**: a bundle goes staged → booting → committed. The page calls `ui_ready` once it has rendered;
//!   if that doesn't happen within READY_SECS, or the app quits first, the bundle is marked failed for good and
//!   the last good bundle (or the one built into the app) is used.
//! - **No downgrades**: only builds above the highest one seen are accepted, even with a valid signature.
//! - **Staged rollout and kill switch**: `rollout` (percent of installs) and `rollback: true` (everyone back to
//!   the built-in interface).
//!
//! New bundles apply on the next launch, never while the app is in use. Release tool: scripts/ui-release.sh.
use crate::net;
use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use parking_lot::RwLock;
use std::sync::Arc;

const MANIFEST_URL: &str = "https://subcast.qiaomu.ai/tools/ui/manifest.json";
/// SUBCAST_UI_MANIFEST points a test build at another manifest.
fn manifest_url() -> String { std::env::var("SUBCAST_UI_MANIFEST").unwrap_or_else(|_| MANIFEST_URL.to_string()) }
#[cfg_attr(debug_assertions, allow(dead_code))]
pub const READY_SECS: u64 = 15;
const MAX_ARCHIVE: u64 = 40 * 1024 * 1024;
pub const SHELL: &str = env!("CARGO_PKG_VERSION");

#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct Manifest {
    pub build: u64,
    pub shell: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default = "full")]
    pub rollout: u8,
    #[serde(default)]
    pub rollback: bool,
    #[serde(default)]
    pub notes: String,
}
fn full() -> u8 { 100 }

#[derive(Deserialize, Serialize, Default, Debug, Clone, PartialEq)]
struct State {
    /// The app version these builds belong to; a new app version starts from its built-in interface.
    shell: String,
    committed: Option<u64>,
    booting: Option<u64>,
    staged: Option<u64>,
    failed: Vec<u64>,
    max_seen: u64,
}

/// The folder the interface is served from right now; None means the one built into the app.
pub type Active = Arc<RwLock<Option<PathBuf>>>;

fn root(data: &Path) -> PathBuf { data.join("ui") }
fn dir(data: &Path, build: u64) -> PathBuf { root(data).join(build.to_string()) }

fn load(data: &Path) -> State {
    let st: State = std::fs::read_to_string(root(data).join("state.json")).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    if st.shell == SHELL { st } else { State { shell: SHELL.into(), ..Default::default() } }
}

fn save(data: &Path, st: &State) -> Result<()> {
    std::fs::create_dir_all(root(data))?;
    let tmp = root(data).join("state.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(st)?)?;
    std::fs::rename(tmp, root(data).join("state.json"))?;
    Ok(())
}

fn usable(data: &Path, build: u64) -> bool { dir(data, build).join("index.html").is_file() }

/// Decides what this launch serves. A bundle still "booting" means the last launch never reported ready: it is
/// failed for good. A staged bundle is tried now.
pub fn boot(data: &Path) -> Option<PathBuf> {
    let mut st = load(data);
    if let Some(b) = st.booting.take() { if !st.failed.contains(&b) { st.failed.push(b); } }
    if let Some(s) = st.staged.take() { if !st.failed.contains(&s) && usable(data, s) { st.booting = Some(s); } }
    if st.committed.is_some_and(|c| st.failed.contains(&c) || !usable(data, c)) { st.committed = None; }
    let _ = save(data, &st);
    st.booting.or(st.committed).map(|b| dir(data, b))
}

/// The page rendered: the bundle being tried becomes the last known good one; older ones are removed.
pub fn ready(data: &Path) -> Option<u64> {
    let mut st = load(data);
    if let Some(b) = st.booting.take() { st.committed = Some(b); let _ = save(data, &st); }
    prune(data, &st);
    st.committed
}

/// Called READY_SECS after launch: a bundle that still hasn't reported ready is failed. Returns true when the
/// page has to be reloaded from the fallback.
pub fn give_up(data: &Path, active: &Active) -> bool {
    let mut st = load(data);
    let Some(b) = st.booting.take() else { return false };
    if !st.failed.contains(&b) { st.failed.push(b); }
    let _ = save(data, &st);
    *active.write() = st.committed.filter(|c| usable(data, *c)).map(|c| dir(data, c));
    true
}

fn prune(data: &Path, st: &State) {
    let keep = [st.committed, st.booting, st.staged];
    for entry in std::fs::read_dir(root(data)).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(n) = name.trim_end_matches(".tmp").parse::<u64>() else { continue };
        if !keep.contains(&Some(n)) || name.ends_with(".tmp") { let _ = std::fs::remove_dir_all(entry.path()); }
    }
}

/// This install's place in a staged rollout (0–99), stable across launches.
fn bucket(data: &Path) -> u8 {
    let f = root(data).join("install-id");
    let id = std::fs::read_to_string(&f).ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| {
        let id = uuid::Uuid::new_v4().to_string();
        let _ = std::fs::create_dir_all(root(data));
        let _ = std::fs::write(&f, &id);
        id
    });
    (Sha256::digest(id.trim().as_bytes())[0] as u16 * 100 / 256) as u8
}

/// minisign, as the app updater does it: the key and signature are base64 of minisign's text format.
pub fn verify(pubkey_b64: &str, bytes: &[u8], sig_b64: &str) -> Result<()> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD;
    let pk = String::from_utf8(b64.decode(pubkey_b64.trim())?)?;
    let sig = String::from_utf8(b64.decode(sig_b64.trim())?)?;
    let pk = minisign_verify::PublicKey::decode(&pk).map_err(|e| anyhow!("公钥无效：{e}"))?;
    let sig = minisign_verify::Signature::decode(&sig).map_err(|e| anyhow!("签名格式无效：{e}"))?;
    pk.verify(bytes, &sig, false).map_err(|_| anyhow!("界面更新的签名不对"))
}

/// What a check found.
#[derive(Debug, PartialEq)]
pub enum Outcome { None, Staged(u64), RolledBack }

/// Which manifest this install should act on, before any download.
fn decide(m: &Manifest, st: &State, bucket: u8) -> Outcome {
    if m.shell != SHELL { return Outcome::None; }
    if m.rollback { return if st.committed.is_some() || st.booting.is_some() || st.staged.is_some() { Outcome::RolledBack } else { Outcome::None }; }
    let current = st.staged.or(st.booting).or(st.committed).unwrap_or(0);
    if m.build <= st.max_seen.max(current) || st.failed.contains(&m.build) || bucket >= m.rollout { return Outcome::None; }
    Outcome::Staged(m.build)
}

/// Checks the server and stages a newer interface for the next launch.
pub async fn check(data: &Path, pubkey: &str) -> Result<Outcome> {
    let url = manifest_url();
    let body = net::client().get(&url).send().await?.error_for_status()?.bytes().await?;
    let sig = net::get_text(&format!("{url}.sig")).await?;
    verify(pubkey, &body, &sig)?;
    let m: Manifest = serde_json::from_slice(&body).context("界面更新清单格式不对")?;
    let mut st = load(data);
    match decide(&m, &st, bucket(data)) {
        Outcome::None => Ok(Outcome::None),
        Outcome::RolledBack => {
            st.committed = None; st.booting = None; st.staged = None; st.max_seen = st.max_seen.max(m.build);
            save(data, &st)?;
            prune(data, &st);
            Ok(Outcome::RolledBack)
        }
        Outcome::Staged(build) => {
            if m.size == 0 || m.size > MAX_ARCHIVE { return Err(anyhow!("界面更新包大小不对")); }
            let bytes = net::client().get(&m.url).send().await?.error_for_status()?.bytes().await?;
            if bytes.len() as u64 != m.size || hex::encode(Sha256::digest(&bytes)) != m.sha256.to_lowercase() {
                return Err(anyhow!("界面更新包校验失败"));
            }
            let tmp = root(data).join(format!("{build}.tmp"));
            let _ = std::fs::remove_dir_all(&tmp);
            std::fs::create_dir_all(&tmp)?;
            tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..])).unpack(&tmp)?;
            if !tmp.join("index.html").is_file() { let _ = std::fs::remove_dir_all(&tmp); return Err(anyhow!("界面更新包缺少 index.html")); }
            let dest = dir(data, build);
            let _ = std::fs::remove_dir_all(&dest);
            std::fs::rename(&tmp, &dest)?;
            let mut st = load(data);
            st.staged = Some(build);
            st.max_seen = st.max_seen.max(build);
            save(data, &st)?;
            prune(data, &st);
            Ok(Outcome::Staged(build))
        }
    }
}

/// A file of the active bundle, or None to fall back to the interface built into the app.
pub fn read(active: &Active, path: &str) -> Option<Option<(Vec<u8>, &'static str)>> {
    let base = active.read().clone()?;
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.split('/').any(|p| p == ".." || p.contains('\\')) { return Some(None); }
    Some(std::fs::read(base.join(rel)).ok().map(|b| (b, mime(rel))))
}

fn mime(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "html" => "text/html", "js" | "mjs" => "text/javascript", "css" => "text/css", "json" | "webmanifest" => "application/json",
        "svg" => "image/svg+xml", "png" => "image/png", "jpg" | "jpeg" => "image/jpeg", "webp" => "image/webp", "ico" => "image/x-icon",
        "woff2" => "font/woff2", "woff" => "font/woff", "ttf" => "font/ttf", "wasm" => "application/wasm", "wav" => "audio/wav", "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn m(build: u64) -> Manifest { Manifest { build, shell: SHELL.into(), url: String::new(), sha256: String::new(), size: 1, rollout: 100, rollback: false, notes: String::new() } }
    fn data() -> PathBuf { let d = std::env::temp_dir().join(format!("subcast-ota-{}", uuid::Uuid::new_v4())); std::fs::create_dir_all(&d).unwrap(); d }
    fn put(d: &Path, b: u64) { std::fs::create_dir_all(dir(d, b)).unwrap(); std::fs::write(dir(d, b).join("index.html"), "<p>").unwrap(); }

    #[test]
    fn only_newer_builds_for_this_shell() {
        let st = State { shell: SHELL.into(), committed: Some(3), max_seen: 3, ..Default::default() };
        assert_eq!(decide(&m(4), &st, 50), Outcome::Staged(4));
        assert_eq!(decide(&m(3), &st, 50), Outcome::None);
        assert_eq!(decide(&m(2), &st, 50), Outcome::None, "no downgrade even when signed");
        assert_eq!(decide(&Manifest { shell: "0.0.1".into(), ..m(9) }, &st, 50), Outcome::None);
        assert_eq!(decide(&m(4), &State { failed: vec![4], ..st.clone() }, 50), Outcome::None, "a failed build is never retried");
        assert_eq!(decide(&Manifest { rollout: 10, ..m(4) }, &st, 50), Outcome::None);
        assert_eq!(decide(&Manifest { rollout: 10, ..m(4) }, &st, 5), Outcome::Staged(4));
        assert_eq!(decide(&Manifest { rollback: true, ..m(5) }, &st, 50), Outcome::RolledBack);
    }

    #[test]
    fn a_bundle_that_never_says_ready_is_dropped() {
        let d = data();
        put(&d, 1); put(&d, 2);
        save(&d, &State { shell: SHELL.into(), committed: Some(1), staged: Some(2), max_seen: 2, ..Default::default() }).unwrap();
        assert_eq!(boot(&d), Some(dir(&d, 2)), "the staged bundle is tried");
        // The app quit (or hung) before ui_ready: next launch drops it and serves the last good one.
        assert_eq!(boot(&d), Some(dir(&d, 1)));
        assert!(load(&d).failed.contains(&2));
        // In-session timeout: the same, and the page reloads from the fallback.
        put(&d, 3);
        let mut st = load(&d); st.staged = Some(3); save(&d, &st).unwrap();
        let active: Active = Arc::new(RwLock::new(boot(&d)));
        assert_eq!(*active.read(), Some(dir(&d, 3)));
        assert!(give_up(&d, &active));
        assert_eq!(*active.read(), Some(dir(&d, 1)));
        // A bundle that reports ready becomes the good one; older folders go.
        put(&d, 4);
        let mut st = load(&d); st.staged = Some(4); save(&d, &st).unwrap();
        boot(&d);
        assert_eq!(ready(&d), Some(4));
        assert!(!dir(&d, 1).exists() && dir(&d, 4).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_new_app_version_starts_from_its_own_interface() {
        let d = data();
        put(&d, 7);
        save(&d, &State { shell: "0.0.1".into(), committed: Some(7), max_seen: 7, ..Default::default() }).unwrap();
        assert_eq!(boot(&d), None);
    }

    /// OTA_FILE + OTA_SIG: a file and the .sig `tauri signer sign` wrote for it, checked with the app's real key.
    #[test]
    #[ignore]
    fn verifies_a_real_signature() {
        let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let pk = conf["plugins"]["updater"]["pubkey"].as_str().unwrap();
        let file = std::fs::read(std::env::var("OTA_FILE").unwrap()).unwrap();
        let sig = std::fs::read_to_string(std::env::var("OTA_SIG").unwrap()).unwrap();
        verify(pk, &file, &sig).unwrap();
        let mut tampered = file.clone(); tampered.push(b' ');
        assert!(verify(pk, &tampered, &sig).is_err());
    }

    #[test]
    fn paths_stay_inside_the_bundle() {
        let d = data(); put(&d, 1);
        let active: Active = Arc::new(RwLock::new(Some(dir(&d, 1))));
        assert!(read(&active, "/").unwrap().is_some());
        assert!(read(&active, "/../state.json").unwrap().is_none());
        assert!(read(&Arc::new(RwLock::new(None)), "/").is_none(), "no bundle: built-in interface");
        let _ = std::fs::remove_dir_all(&d);
    }
}
