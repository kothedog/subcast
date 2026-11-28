//! Cross-device synchronization over iCloud Drive and WebDAV (`subcast-sync.json` v2).
//! Two-tier layout:
//!   - `subcast-sync.json`: subscriptions (LWW + 90-day tombstones), episode progress/watched states,
//!     app-level `favorites`/`notes`, and `contentIndex` (per-episode content hashes).
//!   - `episodes/<episode_id>.json`: incremental per-episode AI & reading blobs (summary, deep-read,
//!     article, chat, chat-archive, highlights, notes-zh), fetched/uploaded only when hashes differ.
use crate::{db, llm, net, ops, settings, CoreCtx, Show};
use anyhow::{anyhow, Result};
use once_cell::sync::Lazy;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;

pub const SYNC_FILE_NAME: &str = "subcast-sync.json";
pub const SYNC_SCHEMA_VERSION: u32 = 2;
pub const WEBDAV_KEY_ACCOUNT: &str = "sync.webdav";
const TOMBSTONE_TTL_MS: i64 = 90 * 24 * 3600 * 1000;
const MAX_SYNC_EPISODES: usize = 5000;

static SYNC_LOCK: Lazy<AsyncMutex<()>> = Lazy::new(|| AsyncMutex::new(()));

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    #[serde(default)]
    pub icloud_enabled: bool,
    #[serde(default)]
    pub icloud_dir: String,
    #[serde(default)]
    pub webdav_enabled: bool,
    #[serde(default)]
    pub webdav_url: String,
    #[serde(default = "default_webdav_path")]
    pub webdav_path: String,
    #[serde(default)]
    pub webdav_username: String,
    #[serde(default = "default_true")]
    pub auto_sync: bool,
    #[serde(default = "default_true")]
    pub sync_progress: bool,
    #[serde(default = "default_true")]
    pub sync_content: bool,
}

fn default_webdav_path() -> String {
    "/SubCast".to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncSubscription {
    pub feed_url: String,
    #[serde(default = "default_kind")]
    pub kind: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<i64>,
}

fn default_kind() -> String {
    "podcast".to_string()
}

impl SyncSubscription {
    fn effective_ts(&self) -> i64 {
        self.updated_at.max(self.deleted_at.unwrap_or(0))
    }

    fn is_deleted(&self) -> bool {
        self.deleted_at.is_some_and(|d| d >= self.updated_at)
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncEpisodeState {
    pub progress: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watched: Option<String>,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncBlobEntry {
    pub data: String,
    pub updated_at: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ContentIndexEntry {
    pub updated_at: i64,
    pub hash: String,
    #[serde(default)]
    pub kinds: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EpisodeSyncBundle {
    pub episode_id: String,
    #[serde(default)]
    pub blobs: BTreeMap<String, SyncBlobEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SyncSnapshot {
    pub version: u32,
    pub updated_at: i64,
    pub device_id: String,
    #[serde(default)]
    pub subscriptions: Vec<SyncSubscription>,
    #[serde(default)]
    pub episodes: BTreeMap<String, SyncEpisodeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favorites: Option<SyncBlobEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<SyncBlobEntry>,
    #[serde(default)]
    pub content_index: BTreeMap<String, ContentIndexEntry>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ICloudStatus {
    pub available: bool,
    pub path: String,
    pub default_path: String,
    pub has_file: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct BackendSyncSummary {
    pub backend: String,
    pub synced_at: i64,
    pub added_shows: usize,
    pub removed_shows: usize,
    pub updated_episodes: usize,
    #[serde(default)]
    pub updated_content: usize,
    pub uploaded: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
struct StoredSyncStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    icloud: Option<BackendSyncSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    webdav: Option<BackendSyncSummary>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub icloud: ICloudStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_icloud: Option<BackendSyncSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_webdav: Option<BackendSyncSummary>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub results: Vec<BackendSyncSummary>,
    pub shows_changed: bool,
    pub settings_changed: bool,
    #[serde(default)]
    pub blobs_changed: bool,
}

pub fn canonical_feed_key(url: &str) -> String {
    let trimmed = url.trim();
    if trimmed.contains('?') || trimmed.contains('#') {
        trimmed.to_string()
    } else {
        trimmed.trim_end_matches('/').to_string()
    }
}

fn is_safe_ep_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 80
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn non_empty_opt(s: Option<&str>) -> Option<String> {
    s.map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
}

fn merge_sub_pair(a: &SyncSubscription, b: &SyncSubscription) -> SyncSubscription {
    let (winner, loser) = if a.effective_ts() > b.effective_ts() {
        (a, b)
    } else if b.effective_ts() > a.effective_ts() {
        (b, a)
    } else if !a.is_deleted() && b.is_deleted() {
        (a, b)
    } else if a.is_deleted() && !b.is_deleted() {
        (b, a)
    } else {
        (a, b)
    };
    let mut out = winner.clone();
    if !out.is_deleted() {
        if out.title.trim().is_empty() && !loser.title.trim().is_empty() {
            out.title = loser.title.clone();
        }
        if out.kind.trim().is_empty() && !loser.kind.trim().is_empty() {
            out.kind = loser.kind.clone();
        }
        if out.author.is_none() {
            out.author = loser.author.clone();
        }
        if out.cover.is_none() {
            out.cover = loser.cover.clone();
        }
        if out.link.is_none() {
            out.link = loser.link.clone();
        }
        if out.alias.is_none() {
            out.alias = loser.alias.clone();
        }
    }
    out
}

fn merge_ep_pair(a: &SyncEpisodeState, b: &SyncEpisodeState) -> SyncEpisodeState {
    if a.updated_at > b.updated_at {
        return a.clone();
    }
    if b.updated_at > a.updated_at {
        return b.clone();
    }
    let progress = a.progress.max(b.progress);
    let watched = match (a.watched.as_deref(), b.watched.as_deref()) {
        (Some("done"), _) | (_, Some("done")) => Some("done".to_string()),
        (Some(x), _) => Some(x.to_string()),
        (_, Some(y)) => Some(y.to_string()),
        _ => None,
    };
    SyncEpisodeState {
        progress,
        watched,
        updated_at: a.updated_at,
    }
}

fn merge_chat_archive(a: &SyncBlobEntry, b: &SyncBlobEntry) -> Option<SyncBlobEntry> {
    let arr_a = serde_json::from_str::<Vec<serde_json::Value>>(&a.data).ok()?;
    let arr_b = serde_json::from_str::<Vec<serde_json::Value>>(&b.data).ok()?;
    let mut by_id: HashMap<String, (serde_json::Value, i64)> = HashMap::new();
    for (arr, ts) in [(arr_a, a.updated_at), (arr_b, b.updated_at)] {
        for item in arr {
            let key = item
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| item.to_string());
            by_id
                .entry(key)
                .and_modify(|cur| {
                    if ts > cur.1 {
                        *cur = (item.clone(), ts);
                    }
                })
                .or_insert((item, ts));
        }
    }
    let mut items: Vec<serde_json::Value> = by_id.into_values().map(|(v, _)| v).collect();
    items.sort_by(|x, y| {
        let ax = x.get("at").and_then(|v| v.as_i64()).unwrap_or(0);
        let ay = y.get("at").and_then(|v| v.as_i64()).unwrap_or(0);
        ay.cmp(&ax)
    });
    Some(SyncBlobEntry {
        data: serde_json::to_string(&items).ok()?,
        updated_at: a.updated_at.max(b.updated_at),
    })
}

pub fn merge_blob_entry(kind: &str, a: &SyncBlobEntry, b: &SyncBlobEntry) -> SyncBlobEntry {
    if kind == "chat-archive" {
        if let Some(merged) = merge_chat_archive(a, b) {
            return merged;
        }
    }
    if a.updated_at > b.updated_at {
        return a.clone();
    }
    if b.updated_at > a.updated_at {
        return b.clone();
    }
    if kind == "chat" {
        let len_a = serde_json::from_str::<Vec<serde_json::Value>>(&a.data)
            .map(|v| v.len())
            .unwrap_or(0);
        let len_b = serde_json::from_str::<Vec<serde_json::Value>>(&b.data)
            .map(|v| v.len())
            .unwrap_or(0);
        if len_b > len_a {
            return b.clone();
        }
        if len_a > len_b {
            return a.clone();
        }
    }
    if a.data.is_empty() && !b.data.is_empty() {
        return b.clone();
    }
    if b.data.len() > a.data.len() {
        return b.clone();
    }
    a.clone()
}

fn merge_opt_blob(kind: &str, a: Option<&SyncBlobEntry>, b: Option<&SyncBlobEntry>) -> Option<SyncBlobEntry> {
    match (a, b) {
        (Some(x), Some(y)) => Some(merge_blob_entry(kind, x, y)),
        (Some(x), None) => Some(x.clone()),
        (None, Some(y)) => Some(y.clone()),
        (None, None) => None,
    }
}

pub fn bundle_hash(blobs: &BTreeMap<String, SyncBlobEntry>) -> String {
    let canonical: BTreeMap<&String, &String> = blobs.iter().map(|(k, v)| (k, &v.data)).collect();
    let json = serde_json::to_string(&canonical).unwrap_or_default();
    crate::short_hash(&json, 16)
}

pub fn index_entry_for(bundle: &EpisodeSyncBundle) -> ContentIndexEntry {
    let updated_at = bundle.blobs.values().map(|b| b.updated_at).max().unwrap_or(0);
    let kinds = bundle.blobs.keys().cloned().collect();
    ContentIndexEntry {
        updated_at,
        hash: bundle_hash(&bundle.blobs),
        kinds,
    }
}

pub fn merge_episode_bundles(local: &EpisodeSyncBundle, remote: &EpisodeSyncBundle) -> EpisodeSyncBundle {
    let mut blobs = BTreeMap::new();
    let keys: BTreeSet<&String> = local.blobs.keys().chain(remote.blobs.keys()).collect();
    for k in keys {
        if let Some(merged) = merge_opt_blob(k, local.blobs.get(k), remote.blobs.get(k)) {
            blobs.insert(k.clone(), merged);
        }
    }
    EpisodeSyncBundle {
        episode_id: local.episode_id.clone(),
        blobs,
    }
}

pub fn merge_snapshots(
    local: &SyncSnapshot,
    remote: Option<&SyncSnapshot>,
    sync_progress: bool,
    sync_content: bool,
    now_ms: i64,
) -> SyncSnapshot {
    let mut by_key: BTreeMap<String, SyncSubscription> = BTreeMap::new();
    for s in &local.subscriptions {
        let key = canonical_feed_key(&s.feed_url);
        if key.is_empty() {
            continue;
        }
        by_key
            .entry(key)
            .and_modify(|cur| *cur = merge_sub_pair(cur, s))
            .or_insert_with(|| s.clone());
    }
    if let Some(rem) = remote {
        for s in &rem.subscriptions {
            let key = canonical_feed_key(&s.feed_url);
            if key.is_empty() {
                continue;
            }
            by_key
                .entry(key)
                .and_modify(|cur| *cur = merge_sub_pair(cur, s))
                .or_insert_with(|| s.clone());
        }
    }
    let cutoff = now_ms - TOMBSTONE_TTL_MS;
    let subscriptions: Vec<SyncSubscription> = by_key
        .into_values()
        .filter(|s| !(s.is_deleted() && s.deleted_at.unwrap_or(0) < cutoff))
        .collect();

    let mut episodes: BTreeMap<String, SyncEpisodeState> = BTreeMap::new();
    if sync_progress {
        for (id, st) in &local.episodes {
            episodes.insert(id.clone(), st.clone());
        }
        if let Some(rem) = remote {
            for (id, st) in &rem.episodes {
                episodes
                    .entry(id.clone())
                    .and_modify(|cur| *cur = merge_ep_pair(cur, st))
                    .or_insert_with(|| st.clone());
            }
        }
        if episodes.len() > MAX_SYNC_EPISODES {
            let mut items: Vec<(String, SyncEpisodeState)> = episodes.into_iter().collect();
            items.sort_by(|a, b| b.1.updated_at.cmp(&a.1.updated_at).then_with(|| a.0.cmp(&b.0)));
            items.truncate(MAX_SYNC_EPISODES);
            episodes = items.into_iter().collect();
        }
    } else if let Some(rem) = remote {
        episodes = rem.episodes.clone();
    }

    let (favorites, notes) = if sync_content {
        (
            merge_opt_blob("favorites", local.favorites.as_ref(), remote.and_then(|r| r.favorites.as_ref())),
            merge_opt_blob("notes", local.notes.as_ref(), remote.and_then(|r| r.notes.as_ref())),
        )
    } else {
        (
            remote.and_then(|r| r.favorites.clone()),
            remote.and_then(|r| r.notes.clone()),
        )
    };

    let content_index = if sync_content {
        BTreeMap::new()
    } else {
        remote.map(|r| r.content_index.clone()).unwrap_or_default()
    };

    SyncSnapshot {
        version: SYNC_SCHEMA_VERSION,
        updated_at: now_ms,
        device_id: local.device_id.clone(),
        subscriptions,
        episodes,
        favorites,
        notes,
        content_index,
    }
}

pub fn snapshots_content_equal(a: &SyncSnapshot, b: &SyncSnapshot) -> bool {
    if a.version != b.version
        || a.subscriptions.len() != b.subscriptions.len()
        || a.episodes.len() != b.episodes.len()
        || a.favorites != b.favorites
        || a.notes != b.notes
        || a.content_index != b.content_index
    {
        return false;
    }
    let mut sa: Vec<(&String, &SyncSubscription)> = a.subscriptions.iter().map(|s| (&s.feed_url, s)).collect();
    let mut sb: Vec<(&String, &SyncSubscription)> = b.subscriptions.iter().map(|s| (&s.feed_url, s)).collect();
    sa.sort_by(|x, y| canonical_feed_key(x.0).cmp(&canonical_feed_key(y.0)));
    sb.sort_by(|x, y| canonical_feed_key(x.0).cmp(&canonical_feed_key(y.0)));
    sa.into_iter().zip(sb).all(|((_, x), (_, y))| x == y) && a.episodes == b.episodes
}

// ---- iCloud Drive transport -------------------------------------------------------------------------

pub const ICLOUD_CONTAINER_DIR: &str = "iCloud~ai~qiaomu~subcast";

pub fn default_icloud_dir() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    #[cfg(target_os = "macos")]
    {
        return Some(home.join("Library/Mobile Documents").join(ICLOUD_CONTAINER_DIR).join("Documents"));
    }
    #[cfg(target_os = "windows")]
    {
        for name in ["iCloudDrive", "iCloud Drive"] {
            let base = home.join(name);
            if base.is_dir() {
                return Some(base.join("SubCast"));
            }
        }
        return None;
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = home;
        None
    }
}

fn resolve_icloud_dir(custom_dir: Option<&str>) -> Option<(PathBuf, bool)> {
    let custom = custom_dir.map(str::trim).filter(|s| !s.is_empty());
    if let Some(dir) = custom {
        return Some((PathBuf::from(dir), true));
    }
    default_icloud_dir().map(|d| (d, false))
}

#[cfg(target_os = "macos")]
fn macos_container_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/Mobile Documents").join(ICLOUD_CONTAINER_DIR))
}

pub fn icloud_status(custom_dir: Option<&str>) -> ICloudStatus {
    let default_path = default_icloud_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let Some((dir, is_custom)) = resolve_icloud_dir(custom_dir) else {
        return ICloudStatus {
            available: false,
            path: String::new(),
            default_path,
            has_file: false,
            note: Some("当前系统未检测到 iCloud 云盘".to_string()),
        };
    };
    let available = if is_custom {
        dir.is_dir() || dir.parent().is_some_and(|p| p.is_dir())
    } else {
        #[cfg(target_os = "macos")]
        {
            dir.parent()
                .and_then(|p| p.parent())
                .is_some_and(|p| p.is_dir())
        }
        #[cfg(target_os = "windows")]
        {
            dir.parent().is_some_and(|p| p.is_dir())
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            false
        }
    };
    let placeholder = dir.join(format!(".{SYNC_FILE_NAME}.icloud"));
    let has_file = dir.join(SYNC_FILE_NAME).is_file() || placeholder.is_file();
    let note = if !available {
        Some("未检测到 iCloud 云盘，请在系统设置里登录 iCloud 并开启云盘".to_string())
    } else {
        #[cfg(target_os = "macos")]
        {
            if !is_custom && !macos_container_root().is_some_and(|r| r.is_dir()) {
                Some("等待 iPhone 端开启 iCloud 同步以初始化容器；如已开启请稍候几秒再试".to_string())
            } else {
                None
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            None
        }
    };
    ICloudStatus {
        available,
        path: dir.to_string_lossy().into_owned(),
        default_path,
        has_file,
        note,
    }
}

async fn read_icloud_file(dir: &Path, rel_path: &str) -> Result<Option<String>> {
    let file = dir.join(rel_path);
    #[cfg(target_os = "macos")]
    if !file.exists() {
        if let (Some(parent), Some(name)) = (file.parent(), file.file_name().and_then(|s| s.to_str())) {
            let placeholder = parent.join(format!(".{name}.icloud"));
            if placeholder.exists() {
                let _ = std::process::Command::new("/usr/bin/brctl")
                    .arg("download")
                    .arg(&file)
                    .output();
                for _ in 0..40 {
                    if file.exists() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(250)).await;
                }
                if !file.exists() {
                    return Err(anyhow!("iCloud 正在下载同步文件，请稍后再试"));
                }
            }
        }
    }
    if !file.exists() {
        return Ok(None);
    }
    Ok(Some(tokio::fs::read_to_string(&file).await?))
}

fn write_icloud_file(dir: &Path, rel_path: &str, bytes: &[u8]) -> Result<()> {
    let target = dir.join(rel_path);
    let parent = target.parent().ok_or_else(|| anyhow!("同步路径无效"))?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".subcast-sync.{}.tmp", uuid::Uuid::new_v4()));
    let write_res = (|| -> Result<()> {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &target)?;
        Ok(())
    })();
    if write_res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    write_res
}

async fn read_icloud_snapshot(dir: &Path) -> Result<Option<SyncSnapshot>> {
    match read_icloud_file(dir, SYNC_FILE_NAME).await? {
        Some(raw) => parse_snapshot(&raw).map(Some),
        None => Ok(None),
    }
}

fn write_icloud_snapshot(dir: &Path, snap: &SyncSnapshot) -> Result<()> {
    let json = serde_json::to_vec_pretty(snap)?;
    write_icloud_file(dir, SYNC_FILE_NAME, &json)
}

fn parse_snapshot(raw: &str) -> Result<SyncSnapshot> {
    let snap: SyncSnapshot = serde_json::from_str(raw).map_err(|_| anyhow!("同步文件格式无效"))?;
    if snap.version > SYNC_SCHEMA_VERSION {
        return Err(anyhow!("同步文件版本过新，请先升级 SubCast"));
    }
    Ok(snap)
}

// ---- WebDAV transport -----------------------------------------------------------------------------

pub fn webdav_urls(base_url: &str, remote_path: &str) -> Result<(Vec<String>, String, String)> {
    let base = base_url.trim().trim_end_matches('/');
    if !(base.starts_with("http://") || base.starts_with("https://")) {
        return Err(anyhow!("请填写有效的 WebDAV 服务器地址（以 https:// 开头）"));
    }
    let mut segs: Vec<String> = remote_path
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .map(|s| urlencoding::encode(s).into_owned())
        .collect();
    if segs.is_empty() {
        segs.push("SubCast".to_string());
    }
    let mut dirs = Vec::with_capacity(segs.len());
    let mut cur = base.to_string();
    for s in &segs {
        cur = format!("{cur}/{s}");
        dirs.push(cur.clone());
    }
    let dir = dirs.last().unwrap();
    let file_url = format!("{dir}/{SYNC_FILE_NAME}");
    let tmp_url = format!("{dir}/{SYNC_FILE_NAME}.tmp");
    Ok((dirs, file_url, tmp_url))
}

fn webdav_episode_urls(dir_url: &str, ep_id: &str) -> (String, String, String) {
    let ep_dir = format!("{dir_url}/episodes");
    let file_url = format!("{ep_dir}/{ep_id}.json");
    let tmp_url = format!("{ep_dir}/{ep_id}.json.tmp");
    (ep_dir, file_url, tmp_url)
}

fn resolve_webdav_creds(username: &str, password_override: Option<&str>) -> Result<(String, String)> {
    let user = username.trim().to_string();
    if user.is_empty() {
        return Err(anyhow!("请先填写 WebDAV 用户名"));
    }
    let pass = password_override
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| llm::get_key(WEBDAV_KEY_ACCOUNT))
        .ok_or_else(|| anyhow!("请先填写 WebDAV 密码"))?;
    Ok((user, pass))
}

async fn send_retry<F, Fut>(mut make: F) -> Result<reqwest::Response>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<reqwest::Response, reqwest::Error>>,
{
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..3u64 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(attempt * attempt * 500)).await;
        }
        match make().await {
            Ok(res) if res.status().is_server_error() || res.status().as_u16() == 429 => {
                last = Some(anyhow!("WebDAV 请求失败（HTTP {}）", res.status().as_u16()));
            }
            Ok(res) => return Ok(res),
            Err(e) if e.is_connect() || e.is_timeout() || e.is_request() => {
                last = Some(net::describe(e));
            }
            Err(e) => return Err(net::describe(e)),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("WebDAV 请求失败")))
}

fn auth_err() -> anyhow::Error {
    anyhow!("WebDAV 账号或密码错误（坚果云请使用应用密码）")
}

async fn webdav_dir_exists(dir_url: &str, user: &str, pass: &str) -> Result<bool> {
    let propfind = Method::from_bytes(b"PROPFIND")?;
    let res = send_retry(|| {
        net::client()
            .request(propfind.clone(), dir_url)
            .basic_auth(user, Some(pass))
            .header("Depth", "0")
            .send()
    })
    .await?;
    let code = res.status().as_u16();
    if code == 401 || code == 403 {
        return Err(auth_err());
    }
    Ok(res.status().is_success() || code == 207)
}

async fn webdav_ensure_dirs(dirs: &[String], user: &str, pass: &str) -> Result<()> {
    let mkcol = Method::from_bytes(b"MKCOL")?;
    for dir in dirs {
        let res = send_retry(|| {
            net::client()
                .request(mkcol.clone(), dir)
                .basic_auth(user, Some(pass))
                .send()
        })
        .await?;
        let code = res.status().as_u16();
        match code {
            200 | 201 | 204 | 301 | 302 | 405 => {}
            401 | 403 => return Err(auth_err()),
            409 => {
                if !webdav_dir_exists(dir, user, pass).await? {
                    return Err(anyhow!("无法创建 WebDAV 目录（HTTP 409）"));
                }
            }
            _ => return Err(anyhow!("无法创建 WebDAV 目录（HTTP {code}）")),
        }
    }
    Ok(())
}

pub async fn test_webdav(
    base_url: &str,
    remote_path: &str,
    username: &str,
    password_override: Option<&str>,
) -> Result<String> {
    let (dirs, _, _) = webdav_urls(base_url, remote_path)?;
    let (user, pass) = resolve_webdav_creds(username, password_override)?;
    let base = base_url.trim().trim_end_matches('/');
    let propfind = Method::from_bytes(b"PROPFIND")?;
    let res = send_retry(|| {
        net::client()
            .request(propfind.clone(), base)
            .basic_auth(&user, Some(&pass))
            .header("Depth", "0")
            .send()
    })
    .await?;
    let code = res.status().as_u16();
    if code == 401 || code == 403 {
        return Err(auth_err());
    }
    if code == 404 {
        return Err(anyhow!("WebDAV 地址不存在，请检查服务器地址"));
    }
    if code == 405 {
        let opt = send_retry(|| {
            net::client()
                .request(Method::OPTIONS, base)
                .basic_auth(&user, Some(&pass))
                .send()
        })
        .await?;
        if matches!(opt.status().as_u16(), 401 | 403) {
            return Err(auth_err());
        }
        if !opt.status().is_success() {
            return Err(anyhow!("WebDAV 连接失败（HTTP {}）", opt.status().as_u16()));
        }
    } else if !res.status().is_success() && code != 207 {
        return Err(anyhow!("WebDAV 连接失败（HTTP {code}）"));
    }
    webdav_ensure_dirs(&dirs, &user, &pass).await?;
    Ok("连接成功".to_string())
}

async fn webdav_get_text(file_url: &str, user: &str, pass: &str) -> Result<Option<String>> {
    let res = send_retry(|| net::client().get(file_url).basic_auth(user, Some(pass)).send()).await?;
    let code = res.status().as_u16();
    if code == 404 {
        return Ok(None);
    }
    if code == 401 || code == 403 {
        return Err(auth_err());
    }
    if !res.status().is_success() {
        return Err(anyhow!("读取 WebDAV 同步文件失败（HTTP {code}）"));
    }
    Ok(Some(res.text().await?))
}

async fn webdav_put_atomic(file_url: &str, tmp_url: &str, user: &str, pass: &str, body: Vec<u8>) -> Result<()> {
    let tmp_url = format!("{tmp_url}.{}", uuid::Uuid::new_v4());
    let put_tmp = send_retry(|| {
        net::client()
            .put(&tmp_url)
            .basic_auth(user, Some(pass))
            .header("Content-Type", "application/json; charset=utf-8")
            .body(body.clone())
            .send()
    })
    .await?;
    if matches!(put_tmp.status().as_u16(), 401 | 403) {
        return Err(auth_err());
    }
    if put_tmp.status().is_success() {
        let mv = Method::from_bytes(b"MOVE")?;
        let move_res = send_retry(|| {
            net::client()
                .request(mv.clone(), &tmp_url)
                .basic_auth(user, Some(pass))
                .header("Destination", file_url)
                .header("Overwrite", "T")
                .send()
        })
        .await;
        if let Ok(r) = move_res {
            if r.status().is_success() {
                return Ok(());
            }
        }
    }

    let put_direct = send_retry(|| {
        net::client()
            .put(file_url)
            .basic_auth(user, Some(pass))
            .header("Content-Type", "application/json; charset=utf-8")
            .body(body.clone())
            .send()
    })
    .await;
    let _ = net::client()
        .delete(&tmp_url)
        .basic_auth(user, Some(pass))
        .send()
        .await;
    let status = put_direct?.status();
    let code = status.as_u16();
    if code == 401 || code == 403 {
        return Err(auth_err());
    }
    if !status.is_success() {
        return Err(anyhow!("上传 WebDAV 同步文件失败（HTTP {code}）"));
    }
    Ok(())
}

async fn read_webdav_snapshot(base_url: &str, remote_path: &str, user: &str, pass: &str) -> Result<Option<SyncSnapshot>> {
    let (_, file_url, _) = webdav_urls(base_url, remote_path)?;
    match webdav_get_text(&file_url, user, pass).await? {
        Some(body) => parse_snapshot(&body).map(Some),
        None => Ok(None),
    }
}

async fn write_webdav_snapshot(
    base_url: &str,
    remote_path: &str,
    user: &str,
    pass: &str,
    snap: &SyncSnapshot,
) -> Result<()> {
    let (dirs, file_url, tmp_url) = webdav_urls(base_url, remote_path)?;
    webdav_ensure_dirs(&dirs, user, pass).await?;
    let body = serde_json::to_vec_pretty(snap)?;
    webdav_put_atomic(&file_url, &tmp_url, user, pass, body).await
}

// ---- Storage abstraction for two-tier bundle sync ---------------------------------------------------

enum SyncStorage<'a> {
    ICloud {
        dir: &'a Path,
    },
    WebDav {
        root_dirs: &'a [String],
        base_dir_url: &'a str,
        user: &'a str,
        pass: &'a str,
        episodes_dir_ready: bool,
    },
}

impl SyncStorage<'_> {
    async fn read_episode(&self, ep_id: &str) -> Result<Option<EpisodeSyncBundle>> {
        let raw = match self {
            SyncStorage::ICloud { dir } => read_icloud_file(dir, &format!("episodes/{ep_id}.json")).await?,
            SyncStorage::WebDav { base_dir_url, user, pass, .. } => {
                let (_, file_url, _) = webdav_episode_urls(base_dir_url, ep_id);
                webdav_get_text(&file_url, user, pass).await?
            }
        };
        let Some(text) = raw else { return Ok(None) };
        let bundle = serde_json::from_str::<EpisodeSyncBundle>(&text).map_err(|_| anyhow!("同步文件格式无效"))?;
        Ok(Some(bundle))
    }

    async fn write_episode(&mut self, bundle: &EpisodeSyncBundle) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(bundle)?;
        match self {
            SyncStorage::ICloud { dir } => {
                write_icloud_file(dir, &format!("episodes/{}.json", bundle.episode_id), &bytes)
            }
            SyncStorage::WebDav {
                root_dirs,
                base_dir_url,
                user,
                pass,
                episodes_dir_ready,
            } => {
                let (ep_dir, file_url, tmp_url) = webdav_episode_urls(base_dir_url, &bundle.episode_id);
                if !*episodes_dir_ready {
                    let mut all_dirs = root_dirs.to_vec();
                    all_dirs.push(ep_dir);
                    webdav_ensure_dirs(&all_dirs, user, pass).await?;
                    *episodes_dir_ready = true;
                }
                webdav_put_atomic(&file_url, &tmp_url, user, pass, bytes).await
            }
        }
    }
}

// ---- Local state & orchestrator ---------------------------------------------------------------------

fn read_config(ctx: &CoreCtx) -> Result<(SyncConfig, serde_json::Value)> {
    let eff = settings::load_effective(ctx)?;
    let cfg: SyncConfig = serde_json::from_value(eff["sync"].clone()).unwrap_or_default();
    Ok((cfg, eff))
}

fn load_stored_status(ctx: &CoreCtx) -> StoredSyncStatus {
    db::get_blob(&ctx.db.lock(), "_sync", "status")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_stored_summary(ctx: &CoreCtx, summary: &BackendSyncSummary) {
    let mut cur = load_stored_status(ctx);
    match summary.backend.as_str() {
        "icloud" => cur.icloud = Some(summary.clone()),
        "webdav" => cur.webdav = Some(summary.clone()),
        _ => {}
    }
    if let Ok(json) = serde_json::to_string(&cur) {
        let _ = db::set_blob(&ctx.db.lock(), "_sync", "status", &json);
    }
}

pub fn sync_status(ctx: &CoreCtx, custom_dir: Option<&str>) -> Result<SyncStatus> {
    let (cfg, _) = read_config(ctx)?;
    let dir = custom_dir.or(Some(cfg.icloud_dir.as_str()));
    let icloud = icloud_status(dir);
    let stored = load_stored_status(ctx);
    Ok(SyncStatus {
        icloud,
        last_icloud: stored.icloud,
        last_webdav: stored.webdav,
    })
}

fn build_local_snapshot(
    ctx: &CoreCtx,
    eff_settings: &serde_json::Value,
    sync_progress: bool,
    sync_content: bool,
) -> Result<(SyncSnapshot, BTreeMap<String, EpisodeSyncBundle>)> {
    let conn = ctx.db.lock();
    let device_id = ops::install_id(&conn);
    let shows = db::list_shows(&conn)?;
    let ledger_subs = db::list_sync_subs(&conn)?;
    let ledger_eps = if sync_progress {
        db::list_sync_episodes(&conn)?
    } else {
        Vec::new()
    };
    let synced_blobs = if sync_content {
        db::list_synced_blobs(&conn)?
    } else {
        Vec::new()
    };
    drop(conn);

    let aliases = eff_settings["showAliases"].as_object();
    let mut sub_meta: HashMap<String, &db::SyncSubLedger> = HashMap::new();
    for item in &ledger_subs {
        sub_meta.insert(canonical_feed_key(&item.feed_url), item);
    }

    let mut subs: Vec<SyncSubscription> = Vec::new();
    let mut active_keys = std::collections::HashSet::new();
    for s in &shows {
        if s.kind == "single" || s.feed_url.trim().is_empty() {
            continue;
        }
        let key = canonical_feed_key(&s.feed_url);
        active_keys.insert(key.clone());
        let ledger = sub_meta.get(&key);
        let updated_at = ledger
            .filter(|l| l.deleted_at.is_none())
            .map(|l| l.updated_at)
            .unwrap_or(1);
        let alias = aliases
            .and_then(|m| m.get(&s.id))
            .and_then(|v| v.as_str())
            .and_then(|v| non_empty_opt(Some(v)));
        subs.push(SyncSubscription {
            feed_url: s.feed_url.clone(),
            kind: s.kind.clone(),
            title: s.title.clone(),
            author: non_empty_opt(s.author.as_deref()),
            cover: non_empty_opt(s.cover.as_deref()),
            link: non_empty_opt(s.link.as_deref()),
            alias,
            updated_at,
            deleted_at: None,
        });
    }
    for item in &ledger_subs {
        if item.deleted_at.is_some() {
            let key = canonical_feed_key(&item.feed_url);
            if !active_keys.contains(&key) {
                subs.push(SyncSubscription {
                    feed_url: item.feed_url.clone(),
                    kind: "podcast".to_string(),
                    title: String::new(),
                    author: None,
                    cover: None,
                    link: None,
                    alias: None,
                    updated_at: item.updated_at,
                    deleted_at: item.deleted_at,
                });
            }
        }
    }
    subs.sort_by(|a, b| canonical_feed_key(&a.feed_url).cmp(&canonical_feed_key(&b.feed_url)));

    let mut episodes = BTreeMap::new();
    for ep in ledger_eps {
        episodes.insert(
            ep.episode_id,
            SyncEpisodeState {
                progress: ep.progress,
                watched: ep.watched_override,
                updated_at: ep.updated_at,
            },
        );
    }

    let mut favorites: Option<SyncBlobEntry> = None;
    let mut notes: Option<SyncBlobEntry> = None;
    let mut bundles: BTreeMap<String, EpisodeSyncBundle> = BTreeMap::new();
    for row in synced_blobs {
        let entry = SyncBlobEntry {
            data: row.data,
            updated_at: row.updated_at,
        };
        if row.episode_id == "_app" {
            if row.kind == "favorites" {
                favorites = Some(entry);
            } else if row.kind == "notes" {
                notes = Some(entry);
            }
            continue;
        }
        if !is_safe_ep_id(&row.episode_id) {
            continue;
        }
        bundles
            .entry(row.episode_id.clone())
            .or_insert_with(|| EpisodeSyncBundle {
                episode_id: row.episode_id,
                blobs: BTreeMap::new(),
            })
            .blobs
            .insert(row.kind, entry);
    }
    let mut content_index = BTreeMap::new();
    for (ep_id, bundle) in &bundles {
        content_index.insert(ep_id.clone(), index_entry_for(bundle));
    }

    let snap = SyncSnapshot {
        version: SYNC_SCHEMA_VERSION,
        updated_at: crate::now_ms(),
        device_id,
        subscriptions: subs,
        episodes,
        favorites,
        notes,
        content_index,
    };
    Ok((snap, bundles))
}

async fn apply_merged_to_local(
    ctx: &CoreCtx,
    local: &SyncSnapshot,
    merged: &SyncSnapshot,
    sync_progress: bool,
    sync_content: bool,
) -> Result<(usize, usize, usize, bool, bool)> {
    let existing_shows: HashMap<String, Show> = {
        let conn = ctx.db.lock();
        db::list_shows(&conn)?
            .into_iter()
            .filter(|s| s.kind != "single" && !s.feed_url.trim().is_empty())
            .map(|s| (canonical_feed_key(&s.feed_url), s))
            .collect()
    };

    let mut to_add: Vec<SyncSubscription> = Vec::new();
    let mut to_remove: Vec<(Show, i64)> = Vec::new();
    let mut tombstone_updates: Vec<(String, i64)> = Vec::new();

    for sub in &merged.subscriptions {
        let key = canonical_feed_key(&sub.feed_url);
        if sub.is_deleted() {
            let del_at = sub.deleted_at.unwrap_or(sub.updated_at);
            if let Some(local_show) = existing_shows.get(&key) {
                to_remove.push((local_show.clone(), del_at));
            } else {
                tombstone_updates.push((sub.feed_url.clone(), del_at));
            }
        } else if !existing_shows.contains_key(&key) {
            to_add.push(sub.clone());
        }
    }

    // Fetch newly discovered subscriptions concurrently (bounded to 4 at once).
    let slots = tokio::sync::Semaphore::new(4);
    let fetched = futures_util::future::join_all(to_add.iter().map(|sub| {
        let slots = &slots;
        async move {
            let _permit = slots.acquire().await;
            let res = tokio::time::timeout(
                Duration::from_secs(20),
                ops::add_subscription(ctx, &sub.feed_url),
            )
            .await;
            (sub, res)
        }
    }))
    .await;

    let mut added_shows = 0usize;
    for (sub, res) in fetched {
        let ok = matches!(res, Ok(Ok(ref a)) if a.show.is_some());
        if !ok {
            let id = crate::short_hash(&sub.feed_url, 16);
            let placeholder = Show {
                id,
                kind: if sub.kind.trim().is_empty() { "podcast".into() } else { sub.kind.clone() },
                title: if sub.title.trim().is_empty() { sub.feed_url.clone() } else { sub.title.clone() },
                author: sub.author.clone(),
                cover: sub.cover.clone(),
                feed_url: sub.feed_url.clone(),
                link: sub.link.clone(),
                description: None,
                updated_at: 0,
                unplayed: 0,
            };
            let conn = ctx.db.lock();
            let _ = db::upsert_show(&conn, &placeholder);
        }
        let conn = ctx.db.lock();
        let _ = db::upsert_sync_sub_raw(&conn, &sub.feed_url, None, sub.updated_at);
        added_shows += 1;
    }

    let mut removed_shows = 0usize;
    {
        let conn = ctx.db.lock();
        for (show, del_at) in &to_remove {
            if db::delete_show_with_tombstone(&conn, &show.id, &show.feed_url, *del_at).is_ok() {
                removed_shows += 1;
            }
        }
        for (feed_url, del_at) in &tombstone_updates {
            let _ = db::upsert_sync_sub_raw(&conn, feed_url, Some(*del_at), *del_at);
        }
        let _ = db::prune_sync_sub_tombstones(&conn, crate::now_ms() - TOMBSTONE_TTL_MS);
    }

    let mut settings_changed = false;
    {
        let current_shows: HashMap<String, Show> = {
            let conn = ctx.db.lock();
            db::list_shows(&conn)?
                .into_iter()
                .filter(|s| s.kind != "single" && !s.feed_url.trim().is_empty())
                .map(|s| (canonical_feed_key(&s.feed_url), s))
                .collect()
        };
        let mut stored = ops::get_settings(ctx)?;
        if stored.is_null() {
            stored = serde_json::json!({});
        }
        if let Some(obj) = stored.as_object_mut() {
            let mut aliases = obj
                .get("showAliases")
                .and_then(|v| v.as_object())
                .cloned()
                .unwrap_or_default();
            for sub in &merged.subscriptions {
                if sub.is_deleted() {
                    continue;
                }
                let Some(alias) = sub.alias.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
                    continue;
                };
                if let Some(show) = current_shows.get(&canonical_feed_key(&sub.feed_url)) {
                    let cur = aliases.get(&show.id).and_then(|v| v.as_str()).unwrap_or("");
                    if cur != alias {
                        aliases.insert(show.id.clone(), serde_json::Value::String(alias.to_string()));
                        settings_changed = true;
                    }
                }
            }
            if settings_changed {
                obj.insert("showAliases".to_string(), serde_json::Value::Object(aliases));
                let _ = ops::set_settings(ctx, &stored);
            }
        }
    }

    let mut updated_episodes = 0usize;
    if sync_progress {
        let conn = ctx.db.lock();
        for (ep_id, st) in &merged.episodes {
            let differs = match local.episodes.get(ep_id) {
                Some(cur) => cur != st,
                None => true,
            };
            if differs
                && db::apply_sync_episode_raw(&conn, ep_id, st.progress, st.watched.as_deref(), st.updated_at).is_ok()
            {
                updated_episodes += 1;
            }
        }
    }

    let mut app_blobs_changed = false;
    if sync_content {
        let conn = ctx.db.lock();
        for (kind, local_entry, merged_entry) in [
            ("favorites", local.favorites.as_ref(), merged.favorites.as_ref()),
            ("notes", local.notes.as_ref(), merged.notes.as_ref()),
        ] {
            if let Some(entry) = merged_entry {
                if local_entry != Some(entry)
                    && db::apply_sync_blob_raw(&conn, "_app", kind, &entry.data, entry.updated_at).is_ok()
                {
                    app_blobs_changed = true;
                }
            }
        }
    }

    Ok((added_shows, removed_shows, updated_episodes, settings_changed, app_blobs_changed))
}

async fn sync_episode_contents(
    ctx: &CoreCtx,
    storage: &mut SyncStorage<'_>,
    local_index: &BTreeMap<String, ContentIndexEntry>,
    local_bundles: &BTreeMap<String, EpisodeSyncBundle>,
    remote_index: Option<&BTreeMap<String, ContentIndexEntry>>,
) -> Result<(BTreeMap<String, ContentIndexEntry>, usize)> {
    let mut merged_index = BTreeMap::new();
    let mut updated_content = 0usize;

    let mut all_ids: BTreeSet<&String> = local_index.keys().collect();
    if let Some(ri) = remote_index {
        all_ids.extend(ri.keys());
    }

    for ep_id in all_ids {
        if !is_safe_ep_id(ep_id) {
            continue;
        }
        let loc_entry = local_index.get(ep_id);
        let rem_entry = remote_index.and_then(|ri| ri.get(ep_id));
        match (loc_entry, rem_entry) {
            (Some(l), Some(r)) if l.hash == r.hash => {
                merged_index.insert(ep_id.clone(), l.clone());
            }
            (None, Some(r)) => {
                if let Some(bundle) = storage.read_episode(ep_id).await? {
                    if !bundle.blobs.is_empty() {
                        let conn = ctx.db.lock();
                        let mut changed = false;
                        for (kind, entry) in &bundle.blobs {
                            if db::is_synced_blob(ep_id, kind)
                                && db::apply_sync_blob_raw(&conn, ep_id, kind, &entry.data, entry.updated_at).is_ok()
                            {
                                changed = true;
                            }
                        }
                        if changed {
                            updated_content += 1;
                        }
                        merged_index.insert(ep_id.clone(), index_entry_for(&bundle));
                    } else {
                        merged_index.insert(ep_id.clone(), r.clone());
                    }
                } else {
                    merged_index.insert(ep_id.clone(), r.clone());
                }
            }
            (Some(l), None) => {
                if let Some(bundle) = local_bundles.get(ep_id) {
                    storage.write_episode(bundle).await?;
                }
                merged_index.insert(ep_id.clone(), l.clone());
            }
            (Some(l), Some(_r)) => {
                let Some(loc_bundle) = local_bundles.get(ep_id) else { continue };
                match storage.read_episode(ep_id).await? {
                    None => {
                        storage.write_episode(loc_bundle).await?;
                        merged_index.insert(ep_id.clone(), l.clone());
                    }
                    Some(rem_bundle) => {
                        let merged_bundle = merge_episode_bundles(loc_bundle, &rem_bundle);
                        let mut changed_local = false;
                        {
                            let conn = ctx.db.lock();
                            for (kind, entry) in &merged_bundle.blobs {
                                if loc_bundle.blobs.get(kind) != Some(entry)
                                    && db::is_synced_blob(ep_id, kind)
                                    && db::apply_sync_blob_raw(&conn, ep_id, kind, &entry.data, entry.updated_at)
                                        .is_ok()
                                {
                                    changed_local = true;
                                }
                            }
                        }
                        if changed_local {
                            updated_content += 1;
                        }
                        if merged_bundle.blobs != rem_bundle.blobs {
                            storage.write_episode(&merged_bundle).await?;
                        }
                        merged_index.insert(ep_id.clone(), index_entry_for(&merged_bundle));
                    }
                }
            }
            (None, None) => {}
        }
    }

    Ok((merged_index, updated_content))
}

async fn run_backend(
    ctx: &CoreCtx,
    backend: &str,
    cfg: &SyncConfig,
    eff: &serde_json::Value,
) -> (BackendSyncSummary, bool, bool) {
    let (local, local_bundles) = match build_local_snapshot(ctx, eff, cfg.sync_progress, cfg.sync_content) {
        Ok(s) => s,
        Err(e) => {
            let s = BackendSyncSummary {
                backend: backend.to_string(),
                synced_at: crate::now_ms(),
                added_shows: 0,
                removed_shows: 0,
                updated_episodes: 0,
                updated_content: 0,
                uploaded: false,
                error: Some(e.to_string()),
            };
            save_stored_summary(ctx, &s);
            return (s, false, false);
        }
    };

    let res: Result<(BackendSyncSummary, bool, bool)> = async {
        match backend {
            "icloud" => {
                let st = icloud_status(Some(cfg.icloud_dir.as_str()));
                if !st.available || st.path.is_empty() {
                    return Err(anyhow!("{}", st.note.unwrap_or_else(|| "iCloud 云盘不可用".into())));
                }
                #[cfg(target_os = "macos")]
                if cfg.icloud_dir.trim().is_empty() && !macos_container_root().is_some_and(|r| r.is_dir()) {
                    return Err(anyhow!("iCloud 专属容器尚未同步到此 Mac，请先在 iPhone 端打开 SubCast 并同步一次，几秒后再试"));
                }
                let dir = PathBuf::from(&st.path);
                let remote = read_icloud_snapshot(&dir).await?;
                let mut merged = merge_snapshots(
                    &local,
                    remote.as_ref(),
                    cfg.sync_progress,
                    cfg.sync_content,
                    crate::now_ms(),
                );
                let (added, removed, eps, settings_changed, app_blobs_changed) =
                    apply_merged_to_local(ctx, &local, &merged, cfg.sync_progress, cfg.sync_content).await?;
                let updated_content = if cfg.sync_content {
                    let mut storage = SyncStorage::ICloud { dir: &dir };
                    let (idx, cnt) = sync_episode_contents(
                        ctx,
                        &mut storage,
                        &local.content_index,
                        &local_bundles,
                        remote.as_ref().map(|r| &r.content_index),
                    )
                    .await?;
                    merged.content_index = idx;
                    cnt
                } else {
                    0
                };
                let needs_write = remote
                    .as_ref()
                    .is_none_or(|r| !snapshots_content_equal(r, &merged));
                if needs_write {
                    write_icloud_snapshot(&dir, &merged)?;
                }
                Ok((
                    BackendSyncSummary {
                        backend: "icloud".into(),
                        synced_at: crate::now_ms(),
                        added_shows: added,
                        removed_shows: removed,
                        updated_episodes: eps,
                        updated_content,
                        uploaded: needs_write,
                        error: None,
                    },
                    settings_changed,
                    app_blobs_changed || updated_content > 0,
                ))
            }
            "webdav" => {
                let (user, pass) = resolve_webdav_creds(&cfg.webdav_username, None)?;
                let (root_dirs, _, _) = webdav_urls(&cfg.webdav_url, &cfg.webdav_path)?;
                let base_dir_url = root_dirs.last().unwrap().clone();
                let remote = read_webdav_snapshot(&cfg.webdav_url, &cfg.webdav_path, &user, &pass).await?;
                let mut merged = merge_snapshots(
                    &local,
                    remote.as_ref(),
                    cfg.sync_progress,
                    cfg.sync_content,
                    crate::now_ms(),
                );
                let (added, removed, eps, settings_changed, app_blobs_changed) =
                    apply_merged_to_local(ctx, &local, &merged, cfg.sync_progress, cfg.sync_content).await?;
                let updated_content = if cfg.sync_content {
                    let mut storage = SyncStorage::WebDav {
                        root_dirs: &root_dirs,
                        base_dir_url: &base_dir_url,
                        user: &user,
                        pass: &pass,
                        episodes_dir_ready: false,
                    };
                    let (idx, cnt) = sync_episode_contents(
                        ctx,
                        &mut storage,
                        &local.content_index,
                        &local_bundles,
                        remote.as_ref().map(|r| &r.content_index),
                    )
                    .await?;
                    merged.content_index = idx;
                    cnt
                } else {
                    0
                };
                let needs_write = remote
                    .as_ref()
                    .is_none_or(|r| !snapshots_content_equal(r, &merged));
                if needs_write {
                    write_webdav_snapshot(&cfg.webdav_url, &cfg.webdav_path, &user, &pass, &merged).await?;
                }
                Ok((
                    BackendSyncSummary {
                        backend: "webdav".into(),
                        synced_at: crate::now_ms(),
                        added_shows: added,
                        removed_shows: removed,
                        updated_episodes: eps,
                        updated_content,
                        uploaded: needs_write,
                        error: None,
                    },
                    settings_changed,
                    app_blobs_changed || updated_content > 0,
                ))
            }
            _ => Err(anyhow!("未知的同步方式")),
        }
    }
    .await;

    match res {
        Ok((summary, settings_changed, blobs_changed)) => {
            save_stored_summary(ctx, &summary);
            (summary, settings_changed, blobs_changed)
        }
        Err(err) => {
            let summary = BackendSyncSummary {
                backend: backend.to_string(),
                synced_at: crate::now_ms(),
                added_shows: 0,
                removed_shows: 0,
                updated_episodes: 0,
                updated_content: 0,
                uploaded: false,
                error: Some(err.to_string()),
            };
            save_stored_summary(ctx, &summary);
            (summary, false, false)
        }
    }
}

pub async fn sync_run(ctx: &CoreCtx, target: Option<&str>) -> Result<SyncReport> {
    let _guard = SYNC_LOCK.lock().await;
    let (cfg, eff) = read_config(ctx)?;
    let backends: Vec<&str> = match target {
        Some("icloud") => vec!["icloud"],
        Some("webdav") => vec!["webdav"],
        Some(_) => return Err(anyhow!("未知的同步方式")),
        None => {
            let mut list = Vec::new();
            if cfg.icloud_enabled {
                list.push("icloud");
            }
            if cfg.webdav_enabled {
                list.push("webdav");
            }
            list
        }
    };
    let mut report = SyncReport::default();
    for backend in backends {
        let (summary, settings_changed, blobs_changed) = run_backend(ctx, backend, &cfg, &eff).await;
        if summary.added_shows > 0 || summary.removed_shows > 0 {
            report.shows_changed = true;
        }
        if settings_changed {
            report.settings_changed = true;
        }
        if blobs_changed {
            report.blobs_changed = true;
        }
        report.results.push(summary);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Episode;

    fn sub(url: &str, updated_at: i64, deleted_at: Option<i64>) -> SyncSubscription {
        SyncSubscription {
            feed_url: url.to_string(),
            kind: "podcast".to_string(),
            title: url.to_string(),
            author: None,
            cover: None,
            link: None,
            alias: None,
            updated_at,
            deleted_at,
        }
    }

    #[test]
    fn merge_subscriptions_union_tombstone_and_resubscribe() {
        let now = 2_000_000_000_000;
        let local = SyncSnapshot {
            version: 2,
            updated_at: now,
            device_id: "dev-a".into(),
            subscriptions: vec![
                sub("https://ex.com/keep.xml", now - 5000, None),
                sub("https://ex.com/local-only.xml", now - 4000, None),
                sub("https://ex.com/deleted-on-remote.xml", now - 5000, None),
                sub("https://ex.com/resub.xml", now - 1000, None),
            ],
            episodes: BTreeMap::new(),
            favorites: None,
            notes: None,
            content_index: BTreeMap::new(),
        };
        let remote = SyncSnapshot {
            version: 2,
            updated_at: now,
            device_id: "dev-b".into(),
            subscriptions: vec![
                sub("https://ex.com/keep.xml/", now - 5000, None),
                sub("https://ex.com/remote-only.xml", now - 3500, None),
                sub("https://ex.com/deleted-on-remote.xml", now - 2000, Some(now - 2000)),
                sub("https://ex.com/resub.xml", now - 3000, Some(now - 3000)),
            ],
            episodes: BTreeMap::new(),
            favorites: None,
            notes: None,
            content_index: BTreeMap::new(),
        };

        let merged = merge_snapshots(&local, Some(&remote), true, true, now);
        let map: HashMap<String, &SyncSubscription> = merged
            .subscriptions
            .iter()
            .map(|s| (canonical_feed_key(&s.feed_url), s))
            .collect();
        assert_eq!(map.len(), 5);
        assert!(!map["https://ex.com/keep.xml"].is_deleted());
        assert!(!map["https://ex.com/local-only.xml"].is_deleted());
        assert!(!map["https://ex.com/remote-only.xml"].is_deleted());
        assert!(map["https://ex.com/deleted-on-remote.xml"].is_deleted());
        assert!(!map["https://ex.com/resub.xml"].is_deleted());
    }

    #[test]
    fn merge_prunes_expired_tombstones() {
        let now = 2_000_000_000_000;
        let expired = now - TOMBSTONE_TTL_MS - 10_000;
        let fresh = now - 60_000;
        let local = SyncSnapshot {
            version: 2,
            updated_at: now,
            device_id: "dev-a".into(),
            subscriptions: vec![
                sub("https://ex.com/old-del.xml", expired, Some(expired)),
                sub("https://ex.com/new-del.xml", fresh, Some(fresh)),
            ],
            episodes: BTreeMap::new(),
            favorites: None,
            notes: None,
            content_index: BTreeMap::new(),
        };
        let merged = merge_snapshots(&local, None, true, true, now);
        assert_eq!(merged.subscriptions.len(), 1);
        assert_eq!(merged.subscriptions[0].feed_url, "https://ex.com/new-del.xml");
    }

    #[test]
    fn merge_episodes_lww_and_tiebreak() {
        let now = 2_000_000_000_000;
        let mut ep_a = BTreeMap::new();
        ep_a.insert("e1".into(), SyncEpisodeState { progress: 120.0, watched: None, updated_at: 100 });
        ep_a.insert("e2".into(), SyncEpisodeState { progress: 500.0, watched: None, updated_at: 200 });
        ep_a.insert("e3".into(), SyncEpisodeState { progress: 300.0, watched: None, updated_at: 300 });

        let mut ep_b = BTreeMap::new();
        ep_b.insert("e1".into(), SyncEpisodeState { progress: 240.0, watched: Some("done".into()), updated_at: 150 });
        ep_b.insert("e2".into(), SyncEpisodeState { progress: 100.0, watched: None, updated_at: 100 });
        ep_b.insert("e3".into(), SyncEpisodeState { progress: 350.0, watched: Some("done".into()), updated_at: 300 });

        let local = SyncSnapshot {
            version: 2, updated_at: now, device_id: "a".into(), subscriptions: vec![], episodes: ep_a,
            favorites: None, notes: None, content_index: BTreeMap::new(),
        };
        let remote = SyncSnapshot {
            version: 2, updated_at: now, device_id: "b".into(), subscriptions: vec![], episodes: ep_b,
            favorites: None, notes: None, content_index: BTreeMap::new(),
        };
        let merged = merge_snapshots(&local, Some(&remote), true, true, now);

        assert_eq!(merged.episodes["e1"].progress, 240.0);
        assert_eq!(merged.episodes["e1"].watched.as_deref(), Some("done"));
        assert_eq!(merged.episodes["e2"].progress, 500.0);
        assert_eq!(merged.episodes["e3"].progress, 350.0);
        assert_eq!(merged.episodes["e3"].watched.as_deref(), Some("done"));
    }

    #[test]
    fn merge_episode_bundles_guides_chat_and_archive() {
        let mut b_a = BTreeMap::new();
        b_a.insert("summary".into(), SyncBlobEntry { data: "Old summary".into(), updated_at: 100 });
        b_a.insert("deep-read".into(), SyncBlobEntry { data: "# Deep Read".into(), updated_at: 200 });
        b_a.insert("chat-archive".into(), SyncBlobEntry { data: r#"[{"id":"c1","at":10}]"#.into(), updated_at: 150 });

        let mut b_b = BTreeMap::new();
        b_b.insert("summary".into(), SyncBlobEntry { data: "New summary".into(), updated_at: 300 });
        b_b.insert("chat".into(), SyncBlobEntry { data: r#"[{"role":"user","content":"hi"}]"#.into(), updated_at: 250 });
        b_b.insert("chat-archive".into(), SyncBlobEntry { data: r#"[{"id":"c2","at":20}]"#.into(), updated_at: 180 });

        let merged = merge_episode_bundles(
            &EpisodeSyncBundle { episode_id: "ep1".into(), blobs: b_a },
            &EpisodeSyncBundle { episode_id: "ep1".into(), blobs: b_b },
        );
        assert_eq!(merged.blobs["summary"].data, "New summary");
        assert_eq!(merged.blobs["deep-read"].data, "# Deep Read");
        assert!(merged.blobs.contains_key("chat"));
        let archive: Vec<serde_json::Value> = serde_json::from_str(&merged.blobs["chat-archive"].data).unwrap();
        assert_eq!(archive.len(), 2);
        assert_eq!(archive[0]["id"], "c2");
        assert_eq!(archive[1]["id"], "c1");
    }

    #[tokio::test]
    async fn icloud_roundtrip_two_devices_with_unsubscribe_and_content() {
        let base = std::env::temp_dir().join(format!("subcast-sync-test-{}", uuid::Uuid::new_v4()));
        let sync_dir = base.join("icloud");
        let dir_a = base.join("dev-a");
        let dir_b = base.join("dev-b");
        let ctx_a = CoreCtx::open(Some(dir_a)).unwrap();
        let ctx_b = CoreCtx::open(Some(dir_b)).unwrap();

        for ctx in [&ctx_a, &ctx_b] {
            ops::set_settings(
                ctx,
                &serde_json::json!({
                    "sync": {
                        "icloudEnabled": true,
                        "icloudDir": sync_dir.to_string_lossy(),
                        "syncProgress": true,
                        "syncContent": true
                    }
                }),
            )
            .unwrap();
        }

        {
            let mut conn = ctx_a.db.lock();
            let s1 = Show { id: "s1".into(), kind: "podcast".into(), title: "Show 1".into(), feed_url: "https://ex.com/1.xml".into(), ..Default::default() };
            let s2 = Show { id: "s2".into(), kind: "podcast".into(), title: "Show 2".into(), feed_url: "https://ex.com/2.xml".into(), ..Default::default() };
            db::upsert_show(&conn, &s1).unwrap();
            db::upsert_show(&conn, &s2).unwrap();
            db::upsert_episodes(
                &mut conn,
                &[Episode { id: "ep1".into(), show_id: "s1".into(), guid: "g1".into(), title: "Ep 1".into(), duration: Some(600.0), ..Default::default() }],
            )
            .unwrap();
            db::set_progress(&conn, "ep1", 123.0).unwrap();
            db::set_blob(&conn, "ep1", "summary", "## 导读正文").unwrap();
            db::set_blob(&conn, "ep1", "deep-read", "# 精读长文").unwrap();
            db::set_blob(&conn, "ep1", "chat", r#"[{"role":"user","content":"讲了什么？"}]"#).unwrap();
            db::set_blob(&conn, "_app", "favorites", r#"[{"id":"ep1","at":1000}]"#).unwrap();
        }

        let r_a1 = sync_run(&ctx_a, Some("icloud")).await.unwrap();
        assert!(r_a1.results[0].error.is_none());
        assert!(r_a1.results[0].uploaded);
        assert!(sync_dir.join(SYNC_FILE_NAME).is_file());
        assert!(sync_dir.join("episodes/ep1.json").is_file());

        // Device B syncs from iCloud -> receives shows, progress, summary, deep-read, chat, favorites.
        let r_b1 = sync_run(&ctx_b, Some("icloud")).await.unwrap();
        assert!(r_b1.results[0].error.is_none());
        assert_eq!(r_b1.results[0].added_shows, 2);
        assert_eq!(r_b1.results[0].updated_content, 1);
        assert!(r_b1.blobs_changed);
        assert!(!r_b1.results[0].uploaded);
        {
            let conn = ctx_b.db.lock();
            assert_eq!(db::get_blob(&conn, "ep1", "summary").unwrap().as_deref(), Some("## 导读正文"));
            assert_eq!(db::get_blob(&conn, "ep1", "deep-read").unwrap().as_deref(), Some("# 精读长文"));
            assert!(db::get_blob(&conn, "_app", "favorites").unwrap().unwrap().contains("ep1"));
            let shows_b = db::list_shows(&conn).unwrap();
            assert_eq!(shows_b.len(), 2);
            let s2_id = shows_b.iter().find(|s| s.feed_url == "https://ex.com/2.xml").unwrap().id.clone();
            db::delete_show(&conn, &s2_id, None).unwrap();
        }
        let r_b2 = sync_run(&ctx_b, Some("icloud")).await.unwrap();
        assert!(r_b2.results[0].uploaded);

        // Device A syncs again -> Show 2 removed, ep1 unchanged (updated_content == 0).
        let r_a2 = sync_run(&ctx_a, Some("icloud")).await.unwrap();
        assert_eq!(r_a2.results[0].removed_shows, 1);
        assert_eq!(r_a2.results[0].updated_content, 0);
        assert!(!r_a2.results[0].uploaded);
        let shows_a = db::list_shows(&ctx_a.db.lock()).unwrap();
        assert_eq!(shows_a.len(), 1);
        assert_eq!(shows_a[0].feed_url, "https://ex.com/1.xml");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn webdav_url_sanitization_and_mock_server_roundtrip() {
        let (dirs, file_url, tmp_url) = webdav_urls("https://dav.jianguoyun.com/dav/", "/SubCast/../My Sync/").unwrap();
        assert_eq!(dirs, vec![
            "https://dav.jianguoyun.com/dav/SubCast",
            "https://dav.jianguoyun.com/dav/SubCast/My%20Sync",
        ]);
        assert_eq!(file_url, "https://dav.jianguoyun.com/dav/SubCast/My%20Sync/subcast-sync.json");
        assert_eq!(tmp_url, "https://dav.jianguoyun.com/dav/SubCast/My%20Sync/subcast-sync.json.tmp");
        assert!(webdav_urls("ftp://bad", "/SubCast").is_err());

        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let addr = server.server_addr().to_ip().unwrap();
        let base_url = format!("http://{addr}/dav");
        let files: std::sync::Arc<std::sync::Mutex<HashMap<String, Vec<u8>>>> = Default::default();
        let files_srv = files.clone();
        let reject_move = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reject_move_srv = reject_move.clone();

        std::thread::spawn(move || {
            for mut req in server.incoming_requests() {
                let auth_ok = req
                    .headers()
                    .iter()
                    .any(|h| h.field.equiv("Authorization") && h.value.as_str().starts_with("Basic "));
                if !auth_ok {
                    let _ = req.respond(tiny_http::Response::empty(401));
                    continue;
                }
                let method = req.method().as_str().to_string();
                let path = req.url().to_string();
                match method.as_str() {
                    "PROPFIND" => {
                        let _ = req.respond(tiny_http::Response::empty(207));
                    }
                    "MKCOL" => {
                        let _ = req.respond(tiny_http::Response::empty(201));
                    }
                    "GET" => {
                        let data = files_srv.lock().unwrap().get(&path).cloned();
                        match data {
                            Some(bytes) => {
                                let _ = req.respond(tiny_http::Response::from_data(bytes).with_status_code(200));
                            }
                            None => {
                                let _ = req.respond(tiny_http::Response::empty(404));
                            }
                        }
                    }
                    "PUT" => {
                        let mut buf = Vec::new();
                        let _ = req.as_reader().read_to_end(&mut buf);
                        files_srv.lock().unwrap().insert(path, buf);
                        let _ = req.respond(tiny_http::Response::empty(201));
                    }
                    "MOVE" => {
                        if reject_move_srv.load(std::sync::atomic::Ordering::Relaxed) {
                            let _ = req.respond(tiny_http::Response::empty(502));
                            continue;
                        }
                        let dest = req
                            .headers()
                            .iter()
                            .find(|h| h.field.equiv("Destination"))
                            .map(|h| h.value.as_str().to_string())
                            .unwrap_or_default();
                        let dest_path = dest
                            .split_once(&format!("http://{addr}"))
                            .map(|(_, p)| p.to_string())
                            .unwrap_or(dest);
                        let mut map = files_srv.lock().unwrap();
                        if let Some(bytes) = map.remove(&path) {
                            map.insert(dest_path, bytes);
                            let _ = req.respond(tiny_http::Response::empty(204));
                        } else {
                            let _ = req.respond(tiny_http::Response::empty(404));
                        }
                    }
                    "DELETE" => {
                        files_srv.lock().unwrap().remove(&path);
                        let _ = req.respond(tiny_http::Response::empty(204));
                    }
                    _ => {
                        let _ = req.respond(tiny_http::Response::empty(405));
                    }
                }
            }
        });

        let msg = test_webdav(&base_url, "/SubCast", "alice", Some("secret")).await.unwrap();
        assert_eq!(msg, "连接成功");

        let (root_dirs, _, _) = webdav_urls(&base_url, "/SubCast").unwrap();
        let base_dir_url = root_dirs.last().unwrap().clone();
        let mut storage = SyncStorage::WebDav {
            root_dirs: &root_dirs,
            base_dir_url: &base_dir_url,
            user: "alice",
            pass: "secret",
            episodes_dir_ready: false,
        };
        let mut blobs = BTreeMap::new();
        blobs.insert("summary".into(), SyncBlobEntry { data: "Guide".into(), updated_at: 500 });
        let bundle = EpisodeSyncBundle { episode_id: "ep_dav".into(), blobs };
        storage.write_episode(&bundle).await.unwrap();
        let loaded_bundle = storage.read_episode("ep_dav").await.unwrap().unwrap();
        assert_eq!(loaded_bundle, bundle);

        // Simulate a proxy that rejects WebDAV MOVE -> direct PUT fallback still succeeds.
        reject_move.store(true, std::sync::atomic::Ordering::Relaxed);
        let snap2 = SyncSnapshot {
            version: 2,
            updated_at: 2000,
            device_id: "dev-1".into(),
            subscriptions: vec![sub("https://ex.com/feed2.xml", 2000, None)],
            episodes: BTreeMap::new(),
            favorites: None,
            notes: None,
            content_index: BTreeMap::new(),
        };
        write_webdav_snapshot(&base_url, "/SubCast", "alice", "secret", &snap2).await.unwrap();
        let loaded2 = read_webdav_snapshot(&base_url, "/SubCast", "alice", "secret").await.unwrap().unwrap();
        assert!(snapshots_content_equal(&snap2, &loaded2));
    }
}
