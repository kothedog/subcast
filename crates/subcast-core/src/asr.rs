//! Local transcription with whisper.cpp. The recogniser runs in a child process (this same executable started
//! with `--asr-worker`) so a crash in native code never takes the app down. Audio stays on this computer and is
//! deleted when the job ends; only the text is kept.
use crate::model::Segment;
use crate::net;
use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

pub struct ModelSpec {
    pub id: &'static str,
    pub name: &'static str,
    /// whisper: whisper.cpp in our own worker process; qwen3: Qwen3-ASR through the CrispASR program.
    pub engine: &'static str,
    /// Hugging Face repository the file comes from.
    pub repo: &'static str,
    pub file: &'static str,
    pub size_mb: u32,
    pub note: &'static str,
}

pub const MODELS: &[ModelSpec] = &[
    ModelSpec { id: "qwen3-asr", name: "Qwen3-ASR 0.6B（推荐）", engine: "qwen3", repo: "cstr/qwen3-asr-0.6b-GGUF", file: "qwen3-asr-0.6b-q8_0.gguf", size_mb: 960, note: "中英文都准，自带标点，专有名词识别好；首次使用另需下载约 20 MB 识别程序" },
    ModelSpec { id: "large-v3-turbo", name: "Whisper large-v3-turbo", engine: "whisper", repo: "ggerganov/whisper.cpp", file: "ggml-large-v3-turbo-q5_0.bin", size_mb: 547, note: "支持语言最多，Apple 芯片上 1 小时音频约 3–5 分钟" },
    ModelSpec { id: "small", name: "Whisper small", engine: "whisper", repo: "ggerganov/whisper.cpp", file: "ggml-small-q5_1.bin", size_mb: 181, note: "体积小、速度快，准确率稍低" },
    ModelSpec { id: "base", name: "Whisper base", engine: "whisper", repo: "ggerganov/whisper.cpp", file: "ggml-base-q5_1.bin", size_mb: 57, note: "最快，适合试用或老电脑" },
];

pub fn spec(id: &str) -> Option<&'static ModelSpec> { MODELS.iter().find(|m| m.id == id) }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub size_mb: u32,
    pub note: String,
    pub installed: bool,
    /** The app whose copy is used (found on this computer, linked, not downloaded). */
    pub from: Option<String>,
}

pub fn models_dir(data: &Path) -> PathBuf { data.join("models") }

pub fn list_models(data: &Path) -> Vec<ModelInfo> {
    adopt_found(data);
    let dir = models_dir(data);
    let mut out: Vec<ModelInfo> = MODELS.iter().map(|m| {
        let path = dir.join(m.file);
        let from = linked_from(&path);
        ModelInfo {
            id: m.id.into(), name: m.name.into(), size_mb: m.size_mb,
            note: m.note.into(),
            installed: path.is_file(),
            from,
        }
    }).collect();
    // Other whisper.cpp / Qwen3-ASR files found on this computer can be used as they are.
    for (name, engine) in extra_files(data) {
        let path = dir.join(&name);
        let size_mb = std::fs::metadata(&path).map(|m| (m.len() / 1_048_576) as u32).unwrap_or(0);
        let label = name.trim_start_matches("ggml-").trim_end_matches(".bin").trim_end_matches(".gguf");
        out.push(ModelInfo {
            id: format!("file:{name}"),
            name: if engine == "qwen3" { format!("Qwen3-ASR（{label}）") } else { format!("Whisper {label}") },
            size_mb,
            note: String::new(),
            installed: path.is_file(),
            from: Some(linked_from(&path).unwrap_or_else(|| "本机".into())),
        });
    }
    out
}

/// A model to run: its engine, file and the id it is recorded under.
pub struct Resolved { pub id: String, pub engine: &'static str, pub path: PathBuf }

/// Our own models by id, and files found on this computer as "file:<name>".
pub fn resolve(data: &Path, id: &str) -> Option<Resolved> {
    if let Some(m) = spec(id) { return Some(Resolved { id: m.id.into(), engine: m.engine, path: models_dir(data).join(m.file) }); }
    let name = id.strip_prefix("file:")?;
    if name.contains('/') || name.contains('\\') || name.contains("..") { return None; }
    let engine = engine_of(name)?;
    Some(Resolved { id: id.into(), engine, path: models_dir(data).join(name) })
}

/// whisper.cpp ggml files and Qwen3-ASR GGUF files, by name.
fn engine_of(name: &str) -> Option<&'static str> {
    let n = name.to_lowercase();
    if n.starts_with("ggml-") && n.ends_with(".bin") && !n.contains("silero") && !n.contains("vad") { Some("whisper") }
    else if n.starts_with("qwen3-asr") && n.ends_with(".gguf") { Some("qwen3") }
    else { None }
}

/// The app a linked model file belongs to, from where it lives ("Screen Studio", "Hugging Face 缓存"…).
fn linked_from(path: &Path) -> Option<String> {
    let target = std::fs::read_link(path).ok()?;
    let s = target.to_string_lossy();
    if s.contains("huggingface") { return Some("Hugging Face 缓存".into()); }
    if let Some(i) = s.find("Application Support/") { return s[i + 20..].split('/').next().map(str::to_string); }
    target.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().to_string())
}

/// Model files in our folder that aren't ours: links to files found elsewhere.
fn extra_files(data: &Path) -> Vec<(String, &'static str)> {
    let mut v: Vec<(String, &'static str)> = std::fs::read_dir(models_dir(data)).into_iter().flatten().flatten()
        .filter_map(|e| { let n = e.file_name().to_string_lossy().to_string(); let eng = engine_of(&n)?; (!MODELS.iter().any(|m| m.file == n) && e.path().is_file()).then_some((n, eng)) })
        .collect();
    v.sort();
    v
}

/// Whether a file really is a model of that kind: whisper.cpp files start with "lmgg", GGUF with "GGUF".
fn looks_like_model(path: &Path, engine: &str) -> bool {
    use std::io::Read;
    let Ok(meta) = std::fs::metadata(path) else { return false };
    let min = if engine == "qwen3" { 300 } else { 30 } * 1_048_576;
    if meta.len() < min { return false; }
    let mut magic = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok()
        && if engine == "qwen3" { &magic == b"GGUF" } else { &magic == b"lmgg" || &magic == b"ggjt" || &magic == b"GGUF" }
}

/// Where other apps keep speech models (whisper.cpp front ends, screen recorders, Hugging Face's cache…).
fn model_roots() -> Vec<(PathBuf, usize)> {
    let h = dirs::home_dir().unwrap_or_default();
    let mut v = vec![
        (h.join(".cache/huggingface/hub"), 5), (h.join(".cache"), 3), (h.join("whisper.cpp/models"), 1),
        (h.join("Downloads"), 1), (h.join(".local/share"), 3),
    ];
    if let Some(d) = dirs::data_dir() { v.push((d, 3)); }       // ~/Library/Application Support, %APPDATA%, ~/.local/share
    // Mac App Store apps keep theirs inside their sandbox containers.
    if cfg!(target_os = "macos") {
        for base in [h.join("Library/Containers"), h.join("Library/Group Containers")] {
            for e in std::fs::read_dir(&base).into_iter().flatten().flatten() {
                let p = e.path();
                v.push((p.join("Data/Library/Application Support"), 3));
                v.push((p.join("Data/Documents"), 2));
                v.push((p.join("Library/Application Support"), 3));
            }
        }
    }
    if let Some(d) = dirs::data_local_dir() { v.push((d, 3)); } // %LOCALAPPDATA%
    v
}

/// Links usable model files found elsewhere on this computer into our models folder (nothing is copied or
/// downloaded; deleting the link here never touches the original). Runs at most once a minute.
static OTHER_FORMATS: parking_lot::Mutex<Vec<PathBuf>> = parking_lot::Mutex::new(Vec::new());
static SEEN_ELSEWHERE: parking_lot::Mutex<Vec<Elsewhere>> = parking_lot::Mutex::new(Vec::new());

/// A Qwen3-ASR copy another app has, in a format SubCast can't run.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Elsewhere { pub app: String, pub format: String }

/// Which app a folder belongs to, from its path.
fn app_of(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.contains("huggingface") { return "Hugging Face 缓存".into(); }
    for marker in ["Application Support/", "Containers/", "Group Containers/"] {
        if let Some(i) = s.find(marker) { if let Some(n) = s[i + marker.len()..].split('/').next() { return n.to_string(); } }
    }
    p.parent().and_then(|x| x.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}

fn describe_other(dirs: Vec<PathBuf>) -> Vec<Elsewhere> {
    let mut out: Vec<Elsewhere> = Vec::new();
    for d in dirs {
        let files: Vec<String> = walkdir_names(&d, 3);
        if !files.iter().any(|f| f.ends_with(".safetensors")) { continue; }
        let mlx = d.to_string_lossy().to_lowercase().contains("mlx") || files.iter().any(|f| f.contains("mlx"));
        let e = Elsewhere { app: app_of(&d), format: if mlx { "MLX".into() } else { "safetensors".into() } };
        if !out.contains(&e) { out.push(e); }
    }
    out
}

fn walkdir_names(d: &Path, depth: usize) -> Vec<String> {
    let mut v = Vec::new();
    for e in std::fs::read_dir(d).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() && depth > 0 { v.extend(walkdir_names(&p, depth - 1)); } else { v.push(e.file_name().to_string_lossy().to_lowercase()); }
    }
    v
}

/// Qwen3-ASR copies found in other apps that SubCast can't use (format), from the last scan.
pub fn elsewhere() -> Vec<Elsewhere> { SEEN_ELSEWHERE.lock().clone() }

/// Looks again on the next list (Settings › 重新查找).
pub fn rescan() { *SCANNED.lock() = None; }
static SCANNED: parking_lot::Mutex<Option<std::time::Instant>> = parking_lot::Mutex::new(None);

fn adopt_found(data: &Path) {
    let last_scan = &SCANNED;

    {
        let mut last = last_scan.lock();
        if last.is_some_and(|t| t.elapsed().as_secs() < 60) { return; }
        *last = Some(std::time::Instant::now());
    }
    let dir = models_dir(data);
    let _ = std::fs::create_dir_all(&dir);
    let own = dir.canonicalize().unwrap_or(dir.clone());
    let mut found: Vec<PathBuf> = Vec::new();
    fn walk(p: &Path, depth: usize, own: &Path, out: &mut Vec<PathBuf>, budget: &mut usize) {
        if *budget == 0 { return; }
        let Ok(rd) = std::fs::read_dir(p) else { return };
        for e in rd.flatten() {
            *budget = budget.saturating_sub(1);
            if *budget == 0 { return; }
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_symlink() && !path.to_string_lossy().contains("huggingface") { continue; }
            if path.starts_with(own) { continue; }
            let name = e.file_name().to_string_lossy().to_string();
            if (ft.is_file() || ft.is_symlink()) && engine_of(&name).is_some() { out.push(path.clone()); }
            // Qwen3-ASR in formats CrispASR can't read (Hugging Face / MLX safetensors folders), noted for the
            // settings page so the listener knows why it isn't used.
            if ft.is_dir() && name.to_lowercase().replace(['_', '-'], "").contains("qwen3asr") { OTHER_FORMATS.lock().push(path.clone()); }
            else if ft.is_dir() && depth > 0 && !name.starts_with('.') || (ft.is_dir() && depth > 0 && (name == ".cache" || name.starts_with("models--"))) {
                if matches!(name.as_str(), "node_modules" | "Caches" | "Logs" | "CrashReporter" | "Photos" | "MobileSync") { continue; }
                walk(&path, depth - 1, own, out, budget);
            }
        }
    }
    let mut budget = 60_000;
    for (root, depth) in model_roots() { walk(&root, depth, &own, &mut found, &mut budget); }
    *SEEN_ELSEWHERE.lock() = describe_other(std::mem::take(&mut *OTHER_FORMATS.lock()));
    for src in found {
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().to_string()) else { continue };
        let Some(engine) = engine_of(&name) else { continue };
        let real = src.canonicalize().unwrap_or(src.clone());
        let dest = dir.join(&name);
        if dest.exists() || !looks_like_model(&real, engine) { continue; }
        // llama.cpp-style Qwen3-ASR comes as two files (the audio encoder in an mmproj file); CrispASR needs the
        // single-file GGUF, so those (Cap's, for one) can't be used.
        if engine == "qwen3" && real.parent().and_then(|d| std::fs::read_dir(d).ok()).is_some_and(|rd| rd.flatten().any(|e| e.file_name().to_string_lossy().to_lowercase().starts_with("mmproj"))) {
            if let Some(d) = real.parent() { SEEN_ELSEWHERE.lock().push(Elsewhere { app: app_of(d), format: "GGUF（llama.cpp 分体）".into() }); }
            continue;
        }
        #[cfg(unix)]
        let _ = std::os::unix::fs::symlink(&real, &dest);
        #[cfg(windows)]
        let _ = std::fs::hard_link(&real, &dest).or_else(|_| std::os::windows::fs::symlink_file(&real, &dest));
    }
}

/// Streams `url` into `dest` (via a .part file), reporting (received, total). Stops when `cancel` is set.
pub async fn download(url: &str, dest: &Path, cancel: &AtomicBool, mut progress: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let res = net::client().get(url).send().await?;
    if !res.status().is_success() { return Err(anyhow!("下载失败（{}）", res.status().as_u16())); }
    let total = res.content_length();
    let part = dest.with_extension("part");
    if let Some(dir) = dest.parent() { tokio::fs::create_dir_all(dir).await?; }
    let mut file = tokio::fs::File::create(&part).await?;
    let mut stream = res.bytes_stream();
    let mut received = 0u64;
    let mut last = std::time::Instant::now();
    while let Some(chunk) = stream.next().await {
        if cancel.load(Ordering::Relaxed) {
            drop(file);
            let _ = tokio::fs::remove_file(&part).await;
            return Err(anyhow!("已取消"));
        }
        let chunk = chunk?;
        file.write_all(&chunk).await?;
        received += chunk.len() as u64;
        if last.elapsed().as_millis() > 200 { progress(received, total); last = std::time::Instant::now(); }
    }
    file.flush().await?;
    drop(file);
    if let Some(t) = total { if received < t { return Err(anyhow!("下载不完整")); } }
    tokio::fs::rename(&part, dest).await?;
    progress(received, total);
    Ok(())
}

pub async fn download_model(data: &Path, id: &str, cancel: &AtomicBool, mirror_first: bool, mut progress: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let m = spec(id).ok_or_else(|| anyhow!("未知模型"))?;
    let dest = models_dir(data).join(m.file);
    // Hugging Face is often unreachable from mainland China; hf-mirror.com serves the same files.
    let hosts = if mirror_first { ["hf-mirror.com", "huggingface.co"] } else { ["huggingface.co", "hf-mirror.com"] };
    let mut err = anyhow!("下载失败");
    for host in hosts {
        let url = format!("https://{host}/{}/resolve/main/{}", m.repo, m.file);
        match download(&url, &dest, cancel, &mut progress).await {
            Ok(()) => return Ok(()),
            Err(e) if cancel.load(Ordering::Relaxed) => return Err(e),
            Err(e) => err = e,
        }
    }
    Err(err)
}

// ---- yt-dlp (only for YouTube audio) -------------------------------------------------------------

fn exe(name: &str) -> String { if cfg!(windows) { format!("{name}.exe") } else { name.to_string() } }

pub fn find_ytdlp(data: &Path) -> Option<PathBuf> {
    let own = data.join("bin").join(exe("yt-dlp"));
    if own.is_file() { return Some(own); }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for d in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] { dirs.push(d.into()); }
    if let Some(h) = dirs::home_dir() { dirs.push(h.join(".local/bin")); }
    dirs.into_iter().map(|d| d.join(exe("yt-dlp"))).find(|p| p.is_file())
}

/// Our mirror of the latest release (server/deploy/sync-ytdlp.sh), reachable where GitHub is slow or blocked.
pub const YTDLP_MIRROR: &str = "https://subcast.qiaomu.ai/tools/yt-dlp";
const YTDLP_GITHUB: &str = "https://github.com/yt-dlp/yt-dlp/releases/latest/download";

fn ytdlp_asset() -> &'static str {
    if cfg!(target_os = "macos") { "yt-dlp_macos" } else if cfg!(windows) { "yt-dlp.exe" } else if cfg!(target_arch = "aarch64") { "yt-dlp_linux_aarch64" } else { "yt-dlp_linux" }
}

/// Downloads yt-dlp from the mirror, else GitHub, and keeps it only if it matches that release's SHA2-256SUMS.
pub async fn install_ytdlp(data: &Path, cancel: &AtomicBool, progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf> {
    install_ytdlp_from(data, &[YTDLP_MIRROR, YTDLP_GITHUB], cancel, progress).await
}

async fn install_ytdlp_from(data: &Path, sources: &[&str], cancel: &AtomicBool, mut progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf> {
    let name = ytdlp_asset();
    let dest = data.join("bin").join(exe("yt-dlp"));
    let mut failure = None;
    for &base in sources {
        match fetch_checked(base, name, &dest, cancel, &mut progress).await {
            Ok(()) => { failure = None; break; }
            Err(e) if e.to_string().contains("已取消") => return Err(e),
            Err(e) => failure = Some(e),
        }
    }
    if let Some(e) = failure { return Err(e); }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(dest)
}

async fn fetch_checked(base: &str, name: &str, dest: &Path, cancel: &AtomicBool, progress: &mut impl FnMut(u64, Option<u64>)) -> Result<()> {
    use sha2::{Digest, Sha256};
    let sums = net::get_text(&format!("{base}/SHA2-256SUMS")).await?;
    let want = sums.lines().find_map(|l| {
        let mut it = l.split_whitespace();
        let (hash, file) = (it.next()?, it.next()?);
        (file.trim_start_matches('*') == name).then(|| hash.to_lowercase())
    }).ok_or_else(|| anyhow!("校验清单里没有 {name}"))?;
    let tmp = dest.with_extension("download");
    download(&format!("{base}/{name}"), &tmp, cancel, |a, b| progress(a, b)).await?;
    let got = hex::encode(Sha256::digest(tokio::fs::read(&tmp).await?));
    if got != want {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(anyhow!("yt-dlp 校验失败（{base}）"));
    }
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

/// YouTube changes often and old yt-dlp builds stop working, so the app keeps its own copy (in `bin/`) and
/// refreshes it at most once a day; a failed YouTube job also forces a check and retries once.
pub async fn ytdlp(data: &Path) -> Result<PathBuf> { ytdlp_with(data, |_, _| {}).await }

/// Only one first-time install at a time: the video page prepares yt-dlp while the player may also need it.
static INSTALLING: once_cell::sync::Lazy<tokio::sync::Mutex<()>> = once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(()));

/// Like `ytdlp`, reporting the first-time download (about 35 MB) so the UI can show it.
pub async fn ytdlp_with(data: &Path, progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf> {
    let own = data.join("bin").join(exe("yt-dlp"));
    if !own.is_file() {
        let _one = INSTALLING.lock().await;
        if !own.is_file() {
            match install_ytdlp(data, &AtomicBool::new(false), progress).await {
                Ok(p) => return Ok(p),
                // GitHub unreachable: a system-wide copy (Homebrew, pip) is better than nothing.
                Err(e) => return find_ytdlp(data).ok_or(e),
            }
        }
    }
    let d = data.to_path_buf();
    tokio::spawn(async move { let _ = update_ytdlp(&d, false).await; });
    Ok(own)
}

pub async fn ytdlp_version(bin: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version").kill_on_drop(true);
    #[cfg(windows)]
    { cmd.creation_flags(0x08000000); }
    let out = tokio::time::timeout(std::time::Duration::from_secs(20), cmd.output()).await.ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Installs the newest release when ours is older. Returns whether it changed anything.
pub async fn update_ytdlp(data: &Path, force: bool) -> Result<bool> {
    let own = data.join("bin").join(exe("yt-dlp"));
    let stamp = data.join("bin").join("yt-dlp.checked");
    let fresh = std::fs::metadata(&stamp).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() < 86_400);
    if !force && fresh { return Ok(false); }
    if force && std::fs::metadata(&stamp).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_secs() < 600) {
        return Ok(false); // a forced check already ran a few minutes ago
    }
    if let Some(dir) = stamp.parent() { std::fs::create_dir_all(dir)?; }
    std::fs::write(&stamp, b"")?;
    // Both are asked: the mirror can fall behind GitHub when its sync job stops, and an old yt-dlp is exactly what
    // YouTube breaks, so then only GitHub has a release that works.
    let (mirror, github) = tokio::join!(mirror_ytdlp_version(), github_ytdlp_version());
    let (latest, sources): (String, &[&str]) = match (mirror, github) {
        (Ok(m), Ok(g)) if newer_version(&g, &m) => (g, &[YTDLP_GITHUB]),
        (Ok(m), _) => (m, &[YTDLP_MIRROR, YTDLP_GITHUB]),
        (Err(_), Ok(g)) => (g, &[YTDLP_MIRROR, YTDLP_GITHUB]),
        (Err(_), Err(e)) => return Err(e),
    };
    if latest.is_empty() { return Ok(false); }
    // Never back to an older one: a copy taken from GitHub stays when only the stale mirror answers later.
    if own.is_file() && ytdlp_version(&own).await.is_some_and(|cur| cur == latest || newer_version(&cur, &latest)) { return Ok(false); }
    install_ytdlp_from(data, sources, &AtomicBool::new(false), |_, _| {}).await?;
    Ok(true)
}

async fn mirror_ytdlp_version() -> Result<String> {
    let v = net::get_text(&format!("{YTDLP_MIRROR}/VERSION")).await?.trim().to_string();
    // Only GitHub's error is ever shown (when both fail), so this one needs no translation.
    if v.is_empty() { return Err(anyhow!("mirror VERSION is empty")); }
    Ok(v)
}

async fn github_ytdlp_version() -> Result<String> {
    let res = net::client().get("https://api.github.com/repos/yt-dlp/yt-dlp/releases/latest")
        .header("User-Agent", "SubCast").header("Accept", "application/vnd.github+json").send().await?;
    if !res.status().is_success() { return Err(anyhow!("GitHub {}", res.status().as_u16())); }
    Ok(res.json::<serde_json::Value>().await?["tag_name"].as_str().unwrap_or_default().to_string())
}

/// yt-dlp versions are dates with an optional build number (`2026.11.23`, `2026.11.22.232654`); anything else
/// counts as not newer.
fn newer_version(a: &str, b: &str) -> bool {
    let parse = |v: &str| v.split('.').map(|p| p.parse::<u64>()).collect::<Result<Vec<_>, _>>();
    matches!((parse(a), parse(b)), (Ok(x), Ok(y)) if x > y)
}

/// Runs a yt-dlp job; when it fails, updates yt-dlp (if a newer one exists) and tries once more.
pub async fn with_ytdlp<T, F, Fut>(data: &Path, job: F) -> Result<T>
where F: Fn(PathBuf) -> Fut, Fut: std::future::Future<Output = Result<T>> {
    let bin = ytdlp(data).await?;
    match job(bin.clone()).await {
        Ok(v) => Ok(v),
        // Cancelled or timed out: a newer yt-dlp wouldn't help, and updating would only double the wait.
        Err(e) if e.to_string().contains("已取消") || e.to_string().contains("超时") => Err(e),
        Err(e) => match update_ytdlp(data, true).await {
            Ok(true) => job(data.join("bin").join(exe("yt-dlp"))).await,
            _ => Err(e),
        },
    }
}
/// The browser whose YouTube sign-in yt-dlp may use when YouTube asks to "confirm you're not a bot" (Settings ›
/// 通用; empty = never). Read from the browser each time, never stored by SubCast.
pub static YT_BROWSER: parking_lot::RwLock<String> = parking_lot::RwLock::new(String::new());

/// Explicit Netscape cookie file path supplied by CLI or settings (never deleted by SubCast).
pub static YT_COOKIES_FILE: once_cell::sync::Lazy<parking_lot::RwLock<Option<PathBuf>>> =
    once_cell::sync::Lazy::new(|| parking_lot::RwLock::new(None));

/// Optional desktop cookie provider callback (creates a temporary Netscape jar that SubCast deletes after one run).
pub type CookieProvider = Box<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<PathBuf>> + Send>>
        + Send
        + Sync,
>;

static COOKIE_PROVIDER: std::sync::OnceLock<CookieProvider> = std::sync::OnceLock::new();

pub fn set_cookie_provider(provider: CookieProvider) {
    let _ = COOKIE_PROVIDER.set(provider);
}

/// A yt-dlp command with what YouTube needs today: a JavaScript runtime for its signature challenges (deno, node
/// or bun, whichever this computer has; an app opened from the Dock doesn't see the terminal's PATH, so the usual
/// install folders are added) and the app's proxy.
pub fn ytdlp_command(ytdlp: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(ytdlp);
    cmd.env("PATH", crate::agents::child_path());
    // Older copies (a system-wide yt-dlp used as a fallback) don't know the option and would refuse to run.
    if knows_js_runtimes(ytdlp) { cmd.args(["--js-runtimes", "deno", "--js-runtimes", "node", "--js-runtimes", "bun"]); }
    let proxy = net::effective_proxy();
    if !proxy.is_empty() { cmd.args(["--proxy", &proxy]); }
    // A dead connection fails in seconds instead of hanging the player on "正在获取视频地址".
    cmd.args(["--socket-timeout", "15"]);
    #[cfg(windows)]
    { cmd.creation_flags(0x08000000); }
    cmd.kill_on_drop(true);
    cmd
}

/// Runs a yt-dlp job and, when YouTube answers with its "confirm you're not a bot" check, tries other YouTube
/// clients, then a configured cookies file, desktop WebView sign-in, or the listener's browser sign-in.
pub async fn with_bot_fallback<T, F, Fut>(job: F) -> Result<T>
where F: Fn(Vec<String>) -> Fut, Fut: std::future::Future<Output = Result<T>> {
    match job(vec![]).await {
        Err(e) if bot_check(&e.to_string()) => {
            let alt = vec!["--extractor-args".to_string(), "youtube:player_client=tv,web_safari,mweb".to_string()];
            match job(alt).await {
                Err(e2) if bot_check(&e2.to_string()) => {
                    let file_cookie = YT_COOKIES_FILE.read().clone().filter(|p| p.is_file());
                    if let Some(jar) = file_cookie {
                        return job(vec!["--cookies".into(), jar.to_string_lossy().into_owned()]).await;
                    }
                    // Signed in to YouTube in SubCast: that sign-in first, it needs no browser.
                    if let Some(provider) = COOKIE_PROVIDER.get() {
                        if let Some(jar) = provider().await {
                            let r = job(vec!["--cookies".into(), jar.to_string_lossy().into_owned()]).await;
                            let _ = std::fs::remove_file(&jar);
                            return r.map_err(|e3| if bot_check(&e3.to_string()) { anyhow!(YT_SIGN_IN_AGAIN) } else { e3 });
                        }
                    }
                    let browser = YT_BROWSER.read().clone();
                    if browser.is_empty() { return Err(anyhow!("YouTube 要求验证「不是机器人」。选一个已登录 YouTube 的浏览器，SubCast 借用它的登录再试")); }
                    { let b = browser.clone(); tokio::task::spawn_blocking(move || crate::import::ensure_browser_access(&b)).await??; }
                    job(vec!["--cookies-from-browser".into(), browser.clone()]).await
                        .map_err(|e3| {
                            let m = e3.to_string();
                            if bot_check(&m) { anyhow!("YouTube 仍然要求验证。请在 {browser} 里打开 youtube.com 确认已登录后再试") }
                            else if browser_locked(&m) { anyhow!("YouTube 要求验证「不是机器人」。{browser} 把登录信息加密了，SubCast 读不到，请在 SubCast 里登录 YouTube") }
                            else { e3 }
                        })
                }
                other => other,
            }
        }
        other => other,
    }
}

/// Asks a yt-dlp once (off the async workers) which options it knows, so the first real job doesn't pay an
/// extra cold start of the program inside a command. Call before `ytdlp_command` on an async path.
pub async fn warm_ytdlp(ytdlp: &Path) {
    let p = ytdlp.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || knows_js_runtimes(&p)).await;
}

fn knows_js_runtimes(ytdlp: &Path) -> bool {
    static SEEN: once_cell::sync::Lazy<parking_lot::Mutex<std::collections::HashMap<PathBuf, bool>>> = once_cell::sync::Lazy::new(Default::default);
    if let Some(v) = SEEN.lock().get(ytdlp) { return *v; }
    let mut c = std::process::Command::new(ytdlp);
    c.arg("--help");
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; c.creation_flags(0x08000000); }
    let ok = c.output().map(|o| String::from_utf8_lossy(&o.stdout).contains("--js-runtimes")).unwrap_or(false);
    SEEN.lock().insert(ytdlp.to_path_buf(), ok);
    ok
}

/// The sign-in made in SubCast no longer satisfies YouTube (signed out elsewhere, or expired).
pub const YT_SIGN_IN_AGAIN: &str = "YouTube 要求验证「不是机器人」。在 SubCast 里登录的 YouTube 已失效，请重新登录"; // i18n-ignore (the interface shows its own text)

/// yt-dlp could not read the browser's cookies because the browser encrypts them for itself (Chrome 127+ on
/// Windows, yt-dlp issue 10927), or the browser holds its cookie file open.
fn browser_locked(msg: &str) -> bool {
    let m = msg.to_lowercase();
    m.contains("dpapi") || m.contains("failed to decrypt") || m.contains("app-bound") || m.contains("could not copy") && m.contains("cookie")
}

fn bot_check(msg: &str) -> bool { let m = msg.to_lowercase(); m.contains("confirm you") && m.contains("bot") || m.contains("sign in to confirm") }

/// Downloads a video's audio (with the bot-check fallbacks).
pub async fn youtube_audio(ytdlp: &Path, video_id: &str, dir: &Path, cancel: Arc<AtomicBool>) -> Result<PathBuf> {
    with_bot_fallback(|extra| { let c = cancel.clone(); async move { let x: Vec<&str> = extra.iter().map(String::as_str).collect(); youtube_audio_with(ytdlp, video_id, dir, c, &x).await } }).await
}

async fn youtube_audio_with(ytdlp: &Path, video_id: &str, dir: &Path, cancel: Arc<AtomicBool>, extra: &[&str]) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let template = dir.join(format!("{video_id}.%(ext)s"));
    warm_ytdlp(ytdlp).await;
    let mut cmd = ytdlp_command(ytdlp);
    // m4a (AAC) decodes without ffmpeg; format 18 (mp4 with AAC audio) is the fallback every video has.
    cmd.args(["-f", "bestaudio[ext=m4a]/bestaudio[acodec^=mp4a]/18", "--no-playlist", "--no-part"]).args(extra).arg("-o").arg(&template);
    cmd.arg(format!("https://www.youtube.com/watch?v={video_id}")).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().context("无法启动 yt-dlp")?;
    let stderr = child.stderr.take();
    let tail = tokio::spawn(async move {
        let mut text = String::new();
        if let Some(s) = stderr { let mut lines = BufReader::new(s).lines(); while let Ok(Some(l)) = lines.next_line().await { text = l; } }
        text
    });
    loop {
        if cancel.load(Ordering::Relaxed) { let _ = child.kill().await; return Err(anyhow!("已取消")); }
        if let Some(status) = child.try_wait()? {
            let last = tail.await.unwrap_or_default();
            if !status.success() {
                return Err(anyhow!("下载 YouTube 音频失败：{}", last.chars().take(200).collect::<String>()));
            }
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(e) = entries.next_entry().await? {
        if e.file_name().to_string_lossy().starts_with(video_id) { return Ok(e.path()); }
    }
    Err(anyhow!("没有找到下载的音频"))
}

/// Downloads a video to keep (720p H.264 with the original AAC sound, one mp4) so a saved video episode plays
/// from disk. Without FFmpeg to join the two streams, format 18 (360p, already one file) is the fallback.
/// `progress` gets 0–1 from yt-dlp's own percentages (picture first, then sound).
pub async fn youtube_video(ytdlp: &Path, ffmpeg: Option<&Path>, video_id: &str, dir: &Path, cancel: Arc<AtomicBool>, progress: impl Fn(f64) + Send + Sync + Clone + 'static) -> Result<PathBuf> {
    with_bot_fallback(|extra| {
        let (c, p) = (cancel.clone(), progress.clone());
        async move { youtube_video_with(ytdlp, ffmpeg, video_id, dir, c, &extra, p).await }
    }).await
}

async fn youtube_video_with(ytdlp: &Path, ffmpeg: Option<&Path>, video_id: &str, dir: &Path, cancel: Arc<AtomicBool>, extra: &[String], progress: impl Fn(f64)) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let template = dir.join(format!("{video_id}.%(ext)s"));
    warm_ytdlp(ytdlp).await;
    let mut cmd = ytdlp_command(ytdlp);
    match ffmpeg {
        // yt-dlp's format sort puts the original-language audio first, so an AI dub isn't what gets saved.
        Some(ff) => { cmd.args(["-f", "bestvideo[height<=720][vcodec^=avc1]+bestaudio[ext=m4a]/best[height<=720][ext=mp4]/18", "--merge-output-format", "mp4", "--ffmpeg-location"]).arg(ff); }
        None => { cmd.args(["-f", "18/best[ext=mp4]"]); }
    }
    cmd.args(["--no-playlist", "--no-part", "--newline"]).args(extra).arg("-o").arg(&template);
    cmd.arg(format!("https://www.youtube.com/watch?v={video_id}")).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().context("无法启动 yt-dlp")?;
    let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<f64>();
    let reader = tokio::spawn(async move {
        let Some(s) = stdout else { return };
        let re = regex::Regex::new(r"\[download\]\s+([\d.]+)%").unwrap();
        let mut lines = BufReader::new(s).lines();
        while let Ok(Some(l)) = lines.next_line().await {
            if let Some(v) = re.captures(&l).and_then(|c| c[1].parse::<f64>().ok()) { let _ = tx.send(v / 100.0); }
        }
    });
    let tail = tokio::spawn(async move {
        let mut text = String::new();
        if let Some(s) = stderr { let mut lines = BufReader::new(s).lines(); while let Ok(Some(l)) = lines.next_line().await { text = l; } }
        text
    });
    // Picture then sound, each 0–100%: the picture is most of the bytes.
    let (mut part, mut last) = (0usize, 0.0f64);
    loop {
        while let Ok(v) = rx.try_recv() {
            if v + 0.2 < last { part += 1; }
            last = v;
            progress(if part == 0 { v * 0.9 } else { 0.9 + v * 0.1 }.min(0.99));
        }
        if cancel.load(Ordering::Relaxed) { let _ = child.kill().await; return Err(anyhow!("已取消")); }
        if let Some(status) = child.try_wait()? {
            let _ = reader.await;
            let last = tail.await.unwrap_or_default();
            if !status.success() { return Err(anyhow!("下载 YouTube 视频失败：{}", last.chars().take(200).collect::<String>())); }
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    }
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(e) = entries.next_entry().await? {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with(video_id) && name.ends_with(".mp4") { return Ok(e.path()); }
    }
    Err(anyhow!("没有找到下载的视频"))
}

/// A playable address for videos whose owners block the embedded player: YouTube's HLS master playlist
/// (adaptive quality, plays natively in WebKit) or, failing that, a single file with both audio and video.
/// A stream the app's own <video> can play. `hls`: the WebView plays HLS natively (WebKit on macOS). Elsewhere
/// (WebView2, most WebKitGTK) only a progressive MP4 works: YouTube's HLS manifests send no CORS header, so
/// hls.js can't load them.
pub async fn youtube_stream_url(ytdlp: &Path, video_id: &str, hls: bool, fresh: bool) -> Result<String> {
    // An address is reused until 20 minutes before YouTube's own expiry (about 6 hours), across restarts.
    let key = format!("{video_id}|{hls}");
    if !fresh { if let Some(url) = streams::get(&key) { return Ok(url); } }
    let url = with_bot_fallback(|extra| async move { stream_once(ytdlp, video_id, hls, &extra).await }).await?;
    streams::put(&key, &url);
    Ok(url)
}

/// Each video's length (seconds) as yt-dlp last reported it, for episodes whose feed gave none.
static DURATIONS: once_cell::sync::Lazy<parking_lot::Mutex<std::collections::HashMap<String, f64>>> = once_cell::sync::Lazy::new(Default::default);
pub fn video_duration(video_id: &str) -> Option<f64> { DURATIONS.lock().get(video_id).copied() }

/// Resolved video addresses, kept in memory and in `streams.json` next to the app's data.
pub mod streams {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use parking_lot::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    static FILE: once_cell::sync::OnceCell<PathBuf> = once_cell::sync::OnceCell::new();
    static MAP: once_cell::sync::Lazy<Mutex<HashMap<String, (String, u64)>>> = once_cell::sync::Lazy::new(|| {
        let saved = FILE.get().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Mutex::new(saved)
    });
    const MARGIN: u64 = 20 * 60;
    const DEFAULT_LIFE: u64 = 4 * 3600;

    fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

    /// Where to persist; call once before the first lookup.
    pub fn init(data: &std::path::Path) { let _ = FILE.set(data.join("streams.json")); }

    /// YouTube's expiry time in a stream address: `expire=1700000000` in the query or `/expire/1700000000/` in an HLS path.
    pub fn expiry(url: &str) -> Option<u64> {
        let tail = url.split_once("expire=").or_else(|| url.split_once("/expire/"))?.1;
        let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    }

    pub fn get(key: &str) -> Option<String> {
        let map = MAP.lock();
        map.get(key).filter(|(_, until)| *until > now()).map(|(u, _)| u.clone())
    }

    pub fn forget(key: &str) {
        let mut map = MAP.lock();
        if map.remove(key).is_some() {
            if let (Some(f), Ok(b)) = (FILE.get(), serde_json::to_vec(&*map)) { let _ = std::fs::write(f, b); }
        }
    }

    pub fn put(key: &str, url: &str) {
        let t = now();
        let until = expiry(url).map(|e| e.saturating_sub(MARGIN)).unwrap_or(t + DEFAULT_LIFE);
        let mut map = MAP.lock();
        map.retain(|_, (_, u)| *u > t);
        if until > t { map.insert(key.to_string(), (url.to_string(), until)); }
        if let (Some(f), Ok(b)) = (FILE.get(), serde_json::to_vec(&*map)) { let _ = std::fs::write(f, b); }
    }

    #[cfg(test)]
    mod tests {
        use super::expiry;
        #[test]
        fn reads_expiry() {
            assert_eq!(expiry("https://rr1.googlevideo.com/videoplayback?expire=1700000000&ei=x"), Some(1700000000));
            assert_eq!(expiry("https://manifest.googlevideo.com/api/manifest/hls_variant/expire/1700001234/ei/abc/file/index.m3u8"), Some(1700001234));
            assert_eq!(expiry("https://example.com/a.mp4"), None);
        }
    }
}

/// How much a format's sound is the video's own: yt-dlp gives the original-language track a higher
/// `language_preference` than YouTube's AI dubs (same bitrate, other language), so this goes first in any choice.
pub fn audio_rank(f: &serde_json::Value) -> i64 {
    let note = f["format_note"].as_str().unwrap_or("").to_lowercase();
    f["language_preference"].as_i64().unwrap_or(0) + if note.contains("original") { 100 } else { 0 } - if note.contains("dubbed") { 100 } else { 0 }
}

async fn stream_once(ytdlp: &Path, video_id: &str, hls: bool, extra: &[String]) -> Result<String> {
    warm_ytdlp(ytdlp).await;
    let mut cmd = ytdlp_command(ytdlp);
    cmd.args(["-j", "--no-playlist", "--no-warnings"]).args(extra);
    cmd.arg(format!("https://www.youtube.com/watch?v={video_id}"));
    let out = tokio::time::timeout(std::time::Duration::from_secs(45), cmd.output()).await.map_err(|_| anyhow!("获取视频地址超时"))?.context("无法启动 yt-dlp")?;
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| {
        anyhow!("无法获取视频地址：{}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").chars().take(200).collect::<String>())
    })?;
    if let Some(d) = info["duration"].as_f64().filter(|d| *d > 0.0) { DURATIONS.lock().insert(video_id.to_string(), d); }
    let formats = info["formats"].as_array().cloned().unwrap_or_default();
    if hls {
        if let Some(u) = formats.iter().filter(|f| f["protocol"].as_str().is_some_and(|p| p.starts_with("m3u8"))).find_map(|f| f["manifest_url"].as_str()) {
            return Ok(u.to_string());
        }
    }
    // Progressive files with both audio and video (format 18, 360p, is the one nearly every video has).
    formats.iter().rev()
        .filter(|f| f["vcodec"].as_str().is_some_and(|v| v != "none") && f["acodec"].as_str().is_some_and(|a| a != "none") && f["protocol"].as_str() == Some("https"))
        .max_by_key(|f| (audio_rank(f), f["ext"].as_str() == Some("mp4"), f["height"].as_i64().unwrap_or(0)))
        .and_then(|f| f["url"].as_str().map(str::to_string))
        .ok_or_else(|| anyhow!("这个视频没有可直接播放的格式"))
}

// ---- CrispASR (runs Qwen3-ASR) ----------------------------------------------------------------------

const CRISPASR_VERSION: &str = "v0.8.54";

fn crispasr_asset() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) { Some("crispasr-macos-arm64.tar.gz") }
    else if cfg!(target_os = "macos") { Some("crispasr-macos-x86_64.tar.gz") }
    else if cfg!(windows) { Some("crispasr-windows-x86_64-cpu.zip") }
    else if cfg!(all(target_os = "linux", target_arch = "aarch64")) { Some("crispasr-linux-arm64.tar.gz") }
    else if cfg!(target_os = "linux") { Some("crispasr-linux-x86_64.tar.gz") }
    else { None }
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() { if let Some(f) = find_file(&p, name) { return Some(f); } }
        else if p.file_name().is_some_and(|n| n == name) { return Some(p); }
    }
    None
}

pub fn find_crispasr(data: &Path) -> Option<PathBuf> {
    find_file(&data.join("bin").join(format!("crispasr-{CRISPASR_VERSION}")), &exe("crispasr"))
}

pub async fn install_crispasr(data: &Path, cancel: &AtomicBool, progress: impl FnMut(u64, Option<u64>)) -> Result<PathBuf> {
    let asset = crispasr_asset().ok_or_else(|| anyhow!("这个系统暂不支持 Qwen3-ASR"))?;
    let archive = data.join("bin").join(asset);
    download(&format!("https://github.com/CrispStrobe/CrispASR/releases/download/{CRISPASR_VERSION}/{asset}"), &archive, cancel, progress).await?;
    let dest = data.join("bin").join(format!("crispasr-{CRISPASR_VERSION}"));
    let (a, d) = (archive.clone(), dest.clone());
    tokio::task::spawn_blocking(move || -> Result<()> {
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d)?;
        let file = std::fs::File::open(&a)?;
        if a.extension().is_some_and(|e| e == "zip") {
            zip::ZipArchive::new(file)?.extract(&d)?;
        } else {
            tar::Archive::new(flate2::read::GzDecoder::new(file)).unpack(&d)?;
        }
        Ok(())
    }).await??;
    let _ = std::fs::remove_file(&archive);
    let bin = find_crispasr(data).ok_or_else(|| anyhow!("识别程序安装不完整"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(bin)
}

/// Writes 16 kHz mono samples as a 16-bit PCM WAV file (what CrispASR reads most reliably).
pub fn write_wav(path: &Path, samples: &[f32]) -> Result<()> {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    let data_len = (samples.len() * 2) as u32;
    f.write_all(b"RIFF")?; f.write_all(&(36 + data_len).to_le_bytes())?; f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?; f.write_all(&1u16.to_le_bytes())?; f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&16000u32.to_le_bytes())?; f.write_all(&32000u32.to_le_bytes())?; f.write_all(&2u16.to_le_bytes())?; f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?; f.write_all(&data_len.to_le_bytes())?;
    for s in samples { f.write_all(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes())?; }
    f.flush()?;
    Ok(())
}

/// Windows: the app-local Visual C++ runtime the installer ships (resources/vcrt). CrispASR's release build
/// imports MSVCP140, VCRUNTIME140(_1) and VCOMP140, which PCs without the Redistributable lack.
pub static VCRT_DIR: once_cell::sync::OnceCell<PathBuf> = once_cell::sync::OnceCell::new();

/// Copies the runtime DLLs next to an executable that needs them (the first place Windows looks).
pub fn place_runtime(bin: &Path) {
    let (Some(src), Some(dir)) = (VCRT_DIR.get(), bin.parent()) else { return };
    for entry in std::fs::read_dir(src).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("dll")) {
            let to = dir.join(entry.file_name());
            if !to.exists() { let _ = std::fs::copy(&p, &to); }
        }
    }
}

/// Runs Qwen3-ASR on a 16 kHz WAV. CrispASR transcribes in ~30 s slices; each slice is split into sentences.
/// Background for a recogniser, in each form one reads (built by asr_context.rs; empty when there is none).
#[derive(Default, Clone, Debug)]
pub struct AsrContext {
    /// Whisper-style initial prompt (short; the end matters most).
    pub prompt: String,
    /// Qwen3-ASR `--context`, read whole.
    pub background: String,
    /// Doubao `corpus.context` JSON string.
    pub doubao: String,
    /// Names and terms from the episode's glossary, when one was made.
    pub hotwords: Vec<String>,
}

pub async fn run_crispasr(bin: &Path, model: &Path, wav: &Path, language: &str, ctx: &AsrContext, cancel: Arc<AtomicBool>, mut on_progress: impl FnMut(f64)) -> Result<Vec<Segment>> {
    let out = wav.with_extension("");
    if cfg!(windows) { place_runtime(bin); }
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("-m").arg(model).arg("-f").arg(wav).args(["-oj", "-np", "-pp"]).arg("-of").arg(&out);
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8);
    cmd.args(["-t", &threads.to_string()]);
    if language == "auto" { cmd.args(["-l", "auto", "--lid-backend", "off"]); } else { cmd.args(["-l", language]); }
    // Qwen3-ASR reads --context (the show notes) and --hotwords; --prompt is whisper-only in CrispASR.
    if !ctx.background.is_empty() { cmd.arg("--context").arg(&ctx.background); }
    let words: Vec<&str> = ctx.hotwords.iter().map(|w| w.trim()).filter(|w| !w.is_empty() && !w.contains(',')).take(60).collect();
    if !words.is_empty() { cmd.arg("--hotwords").arg(words.join(",")); }
    cmd.kill_on_drop(true).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    { cmd.creation_flags(0x08000000); }
    let mut child = cmd.spawn().context("无法启动 Qwen3-ASR 识别程序")?;
    let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
    let re = regex::Regex::new(r"progress\s*=\s*(\d+)%").unwrap();
    let mut tail: Vec<String> = Vec::new();
    loop {
        // Checked every turn: with output arriving faster than the 300 ms tick, the tick branch would never run.
        if cancel.load(Ordering::Relaxed) { let _ = child.kill().await; return Err(anyhow!("已取消")); }
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                if let Some(c) = re.captures(&line) { on_progress(c[1].parse::<f64>().unwrap_or(0.0) / 100.0); }
                else if !line.starts_with("ggml_") { tail.push(line); if tail.len() > 6 { tail.remove(0); } }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {
                if cancel.load(Ordering::Relaxed) { let _ = child.kill().await; return Err(anyhow!("已取消")); }
            }
        }
    }
    let status = child.wait().await?;
    let json_path = out.with_extension("json");
    if !status.success() || !json_path.is_file() {
        return Err(anyhow!("Qwen3-ASR 识别失败：{}", tail.last().cloned().unwrap_or_default()));
    }
    let v: serde_json::Value = serde_json::from_str(&tokio::fs::read_to_string(&json_path).await?)?;
    let mut segs = Vec::new();
    for t in v["transcription"].as_array().into_iter().flatten() {
        let start = t["offsets"]["from"].as_f64().unwrap_or(0.0) / 1000.0;
        let end = t["offsets"]["to"].as_f64().unwrap_or(0.0) / 1000.0;
        let text = t["text"].as_str().unwrap_or("").trim();
        if !text.is_empty() { segs.extend(crate::transcript::split_timed(text, start, end)); }
    }
    Ok(segs)
}

// ---- running the worker ---------------------------------------------------------------------------

pub enum WorkerEvent {
    Progress(f64),
    Segment(Segment),
}

pub struct WorkerResult {
    pub language: String,
    pub segments: Vec<Segment>,
}

pub async fn run_worker(audio: &Path, model: &Path, language: &str, prompt: &str, cancel: Arc<AtomicBool>, mut on: impl FnMut(WorkerEvent)) -> Result<WorkerResult> {
    let me = std::env::current_exe()?;
    let mut cmd = tokio::process::Command::new(me);
    cmd.arg("--asr-worker").arg(audio).arg(model).arg(language).arg(prompt).kill_on_drop(true)
        .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    #[cfg(windows)]
    { cmd.creation_flags(0x08000000); }
    let mut child = cmd.spawn()?;
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let stderr = child.stderr.take().unwrap();
    let errtail = tokio::spawn(async move {
        let mut tail = Vec::new();
        let mut l = BufReader::new(stderr).lines();
        while let Ok(Some(x)) = l.next_line().await { tail.push(x); if tail.len() > 8 { tail.remove(0); } }
        tail.join("\n")
    });
    let mut segments = Vec::new();
    let mut language_out = language.to_string();
    let mut error: Option<String> = None;
    loop {
        // Checked every turn: with output arriving faster than the 300 ms tick, the tick branch would never run.
        if cancel.load(Ordering::Relaxed) { let _ = child.kill().await; return Err(anyhow!("已取消")); }
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                match v["type"].as_str() {
                    Some("progress") => on(WorkerEvent::Progress(v["p"].as_f64().unwrap_or(0.0))),
                    Some("segment") => {
                        let s = Segment { start: v["start"].as_f64().unwrap_or(0.0), end: v["end"].as_f64().unwrap_or(0.0), text: v["text"].as_str().unwrap_or("").trim().to_string() };
                        if !s.text.is_empty() { on(WorkerEvent::Segment(s.clone())); segments.push(s); }
                    }
                    Some("language") => language_out = v["language"].as_str().unwrap_or(language).to_string(),
                    Some("error") => error = v["message"].as_str().map(String::from),
                    _ => {}
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {
                if cancel.load(Ordering::Relaxed) { let _ = child.kill().await; return Err(anyhow!("已取消")); }
            }
        }
    }
    let status = child.wait().await?;
    let tail = errtail.await.unwrap_or_default();
    if let Some(e) = error { return Err(anyhow!(e)); }
    if !status.success() { return Err(anyhow!("转写进程异常退出：{}", tail.lines().last().unwrap_or(""))); }
    Ok(WorkerResult { language: language_out, segments })
}

// ---- the worker itself -----------------------------------------------------------------------------

fn emit(v: serde_json::Value) {
    use std::io::Write;
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

/// Decodes any mp3/m4a/aac/wav/flac/ogg file to 16 kHz mono f32, the input whisper.cpp wants.
pub fn decode_16k_mono(path: &Path) -> Result<Vec<f32>> {
    // Mixed down and resampled packet by packet: only the 16 kHz result is kept.
    let mut resampler: Option<Resampler> = None;
    decode_mono(path, |rate, samples| {
        let r = resampler.get_or_insert_with(|| Resampler::new(rate, 16000));
        for &x in samples { r.push(x); }
    })?;
    Ok(resampler.map(Resampler::finish).unwrap_or_default())
}

/// Decodes an audio file and hands it to `f` packet by packet, mixed down to mono at the file's own sample rate.
pub fn decode_mono(path: &Path, mut f: impl FnMut(u32, &[f32])) -> Result<()> {
    use symphonia::core::{audio::SampleBuffer, codecs::DecoderOptions, errors::Error, formats::FormatOptions, io::MediaSourceStream, meta::MetadataOptions, probe::Hint};
    let file = std::fs::File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) { hint.with_extension(ext); }
    let probed = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()).map_err(|e| anyhow!("无法识别音频格式：{e}"))?;
    let mut format = probed.format;
    // A saved video carries a picture track too: the sound is the track with a sample rate.
    let track = format.tracks().iter().find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some()).ok_or_else(|| anyhow!("没有音轨"))?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| anyhow!("不支持的音频编码：{e}"))?;
    let mut buf: Option<SampleBuffer<f32>> = None;
    let mut mono: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(anyhow!("读取音频失败：{e}")),
        };
        if packet.track_id() != track_id { continue; }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(anyhow!("解码失败：{e}")),
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        if buf.as_ref().is_none_or(|b| b.capacity() < decoded.capacity() * channels) {
            buf = Some(SampleBuffer::new(decoded.capacity() as u64, spec));
        }
        let b = buf.as_mut().unwrap();
        b.copy_interleaved_ref(decoded);
        mono.clear();
        mono.extend(b.samples().chunks(channels).map(|frame| frame.iter().sum::<f32>() / channels as f32));
        f(spec.rate, &mono);
    }
    Ok(())
}

/// Box-filtered linear resampling of a whole buffer (tests compare `Resampler` against it).
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() { return input.to_vec(); }
    let mut r = Resampler::new(from, to);
    for &x in input { r.push(x); }
    r.finish()
}

/// `resample` one sample at a time, so a long episode never has to sit in memory at its original rate:
/// only the 16 kHz output is kept (a 3-hour episode at 48 kHz was over 2 GB as intermediate buffers).
pub struct Resampler { ratio: f64, width: usize, hist: std::collections::VecDeque<f32>, acc: f32, count: usize, prev: Option<f32>, next: usize, out: Vec<f32> }

impl Resampler {
    pub fn new(from: u32, to: u32) -> Self {
        let ratio = from as f64 / to as f64;
        let width = ratio.ceil().max(1.0) as usize;
        Self { ratio, width, hist: std::collections::VecDeque::with_capacity(width + 1), acc: 0.0, count: 0, prev: None, next: 0, out: Vec::new() }
    }
    pub fn push(&mut self, x: f32) {
        // Moving average over the last `width` inputs (fewer at the very start).
        let f = if self.width > 1 {
            self.acc += x;
            self.hist.push_back(x);
            if self.hist.len() > self.width { self.acc -= self.hist.pop_front().unwrap_or(0.0); }
            self.acc / self.hist.len() as f32
        } else { x };
        let i = self.count;
        self.count += 1;
        // Every output that falls between the previous filtered sample and this one.
        if let Some(p) = self.prev {
            loop {
                let pos = self.next as f64 * self.ratio;
                let j = pos as usize;
                if j + 1 != i { break; }
                let frac = (pos - j as f64) as f32;
                self.out.push(p + (f - p) * frac);
                self.next += 1;
            }
        }
        self.prev = Some(f);
    }
    pub fn finish(mut self) -> Vec<f32> {
        let n = (self.count as f64 / self.ratio) as usize;
        if let Some(p) = self.prev { while self.next < n { self.out.push(p); self.next += 1; } }
        self.out.truncate(n);
        self.out
    }
}

#[cfg(test)]
mod decode_bench {
    /// SUBCAST_AUDIO: a long audio file. Prints how long decoding takes and how many 16 kHz samples come out.
    #[test]
    #[ignore]
    fn decode_long_file() {
        let path = std::env::var("SUBCAST_AUDIO").unwrap();
        let t = std::time::Instant::now();
        let s = super::decode_16k_mono(std::path::Path::new(&path)).unwrap();
        println!("decoded {} samples ({:.0} min) in {:?}", s.len(), s.len() as f64 / 16000.0 / 60.0, t.elapsed());
    }
}

#[cfg(test)]
mod resample_tests {
    /// The previous all-in-memory version, kept to check the streaming one against.
    fn batch(input: &[f32], from: u32, to: u32) -> Vec<f32> {
        if from == to || input.is_empty() { return input.to_vec(); }
        let ratio = from as f64 / to as f64;
        let width = ratio.ceil().max(1.0) as usize;
        let filtered: Vec<f32> = if width > 1 {
            let mut acc = 0.0f32;
            let mut out = Vec::with_capacity(input.len());
            for i in 0..input.len() { acc += input[i]; if i >= width { acc -= input[i - width]; } out.push(acc / width.min(i + 1) as f32); }
            out
        } else { input.to_vec() };
        let n = (input.len() as f64 / ratio) as usize;
        (0..n).map(|i| {
            let pos = i as f64 * ratio; let j = pos as usize; let frac = (pos - j as f64) as f32;
            let a = filtered[j.min(filtered.len() - 1)]; let b = filtered[(j + 1).min(filtered.len() - 1)];
            a + (b - a) * frac
        }).collect()
    }
    #[test]
    fn streaming_matches_batch() {
        let input: Vec<f32> = (0..100_003).map(|i| ((i as f32) * 0.013).sin() * 0.7 + ((i * 7919 % 1000) as f32 / 1000.0 - 0.5) * 0.2).collect();
        for (from, to) in [(48_000, 16_000), (44_100, 16_000), (22_050, 16_000), (8_000, 16_000), (16_000, 16_000)] {
            let a = batch(&input, from, to);
            let b = super::resample(&input, from, to);
            assert_eq!(a.len(), b.len(), "{from}->{to} length");
            let worst = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
            assert!(worst < 1e-4, "{from}->{to} differs by {worst}");
        }
    }
}

pub fn worker_main(args: &[String]) -> i32 {
    let run = || -> Result<()> {
        let [audio, model, language, prompt] = args else { return Err(anyhow!("参数错误")) };
        let samples = decode_16k_mono(Path::new(audio))?;
        emit(serde_json::json!({"type": "progress", "p": 0.02}));
        use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
        let ctx = WhisperContext::new_with_params(model, WhisperContextParameters::default()).map_err(|e| anyhow!("加载模型失败：{e}"))?;
        let mut state = ctx.create_state().map_err(|e| anyhow!("{e}"))?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).min(8) as i32;
        params.set_n_threads(threads);
        params.set_language(if language == "auto" { None } else { Some(language.as_str()) });
        if !prompt.is_empty() { params.set_initial_prompt(prompt); }
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_special(false);
        params.set_print_timestamps(false);
        params.set_suppress_blank(true);
        // Keeps one stuck hallucination from echoing through the rest of the file.
        params.set_no_context(true);
        params.set_progress_callback_safe(|p: i32| emit(serde_json::json!({"type": "progress", "p": (p as f64 / 100.0).max(0.02)})));
        params.set_segment_callback_safe_lossy(|s: whisper_rs::SegmentCallbackData| {
            emit(serde_json::json!({"type": "segment", "start": s.start_timestamp as f64 / 100.0, "end": s.end_timestamp as f64 / 100.0, "text": s.text}));
        });
        state.full(params, &samples).map_err(|e| anyhow!("转写失败：{e}"))?;
        if let Ok(id) = state.full_lang_id_from_state() {
            if let Some(l) = whisper_rs::get_lang_str(id) { emit(serde_json::json!({"type": "language", "language": l})); }
        }
        emit(serde_json::json!({"type": "progress", "p": 1.0}));
        Ok(())
    };
    match run() {
        Ok(()) => 0,
        Err(e) => { emit(serde_json::json!({"type": "error", "message": e.to_string()})); 1 }
    }
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    #[test]
    fn links_models_found_elsewhere_and_unlinks_without_touching_them() {
        let home = std::env::temp_dir().join(format!("subcast-scan-{}", uuid::Uuid::new_v4()));
        let other = home.join("OtherApp/models");
        std::fs::create_dir_all(&other).unwrap();
        let mut ggml = b"lmgg".to_vec(); ggml.resize(31 * 1_048_576, 0);
        std::fs::write(other.join("ggml-medium.bin"), &ggml).unwrap();
        std::fs::write(other.join("ggml-tiny.bin"), b"lmgg too small").unwrap();
        let mut fake = b"XXXX".to_vec(); fake.resize(31 * 1_048_576, 0);
        std::fs::write(other.join("ggml-small-q5_1.bin"), &fake).unwrap();
        let data = home.join("data");
        std::fs::create_dir_all(models_dir(&data)).unwrap();
        let mut found = Vec::new();
        // The same linking as adopt_found, rooted at the test folder.
        for e in std::fs::read_dir(&other).unwrap().flatten() { found.push(e.path()); }
        for src in found {
            let name = src.file_name().unwrap().to_string_lossy().to_string();
            let Some(engine) = engine_of(&name) else { continue };
            if looks_like_model(&src, engine) { std::os::unix::fs::symlink(&src, models_dir(&data).join(&name)).unwrap(); }
        }
        let list = list_models(&data);
        let medium = list.iter().find(|m| m.id == "file:ggml-medium.bin").expect("found model listed");
        assert!(medium.installed && medium.from.is_some(), "{:?}", medium.from);
        assert!(!list.iter().any(|m| m.id == "file:ggml-tiny.bin"), "too small is not a model");
        assert!(!list.iter().find(|m| m.id == "small").unwrap().installed, "wrong magic is not adopted");
        let r = resolve(&data, "file:ggml-medium.bin").unwrap();
        assert_eq!(r.engine, "whisper");
        std::fs::remove_file(&r.path).unwrap();
        assert!(other.join("ggml-medium.bin").is_file(), "the original stays");
        assert!(resolve(&data, "file:../x.bin").is_none());
        let _ = std::fs::remove_dir_all(&home);
    }
    /// On this computer: what a real scan finds (prints, doesn't assert).
    #[test]
    #[ignore]
    fn scan_this_computer() {
        let data = std::env::temp_dir().join(format!("subcast-scan-real-{}", uuid::Uuid::new_v4()));
        let t0 = std::time::Instant::now();
        for m in list_models(&data) { println!("{} installed={} {} from={:?}", m.id, m.installed, m.size_mb, m.from); }
        println!("elsewhere: {:?}", elsewhere());
        println!("scan took {:?}", t0.elapsed());
        let _ = std::fs::remove_dir_all(&data);
    }
}


#[cfg(test)]
mod video_file_tests {
    use super::*;
    /// A saved YouTube episode is now an mp4 with H.264 picture and AAC sound: transcription reads the sound.
    #[test]
    fn decodes_the_sound_of_a_video_file() {
        let Some(ff) = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg", "/usr/bin/ffmpeg"].iter().map(Path::new).find(|p| p.exists()) else { return };
        let dir = std::env::temp_dir().join(format!("subcast-asr-mp4-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("v.mp4");
        let ok = std::process::Command::new(ff).args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=320x180:rate=25:duration=3", "-f", "lavfi", "-i", "sine=frequency=440:duration=3",
            "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest"]).arg(&f).status().unwrap().success();
        assert!(ok);
        let pcm = decode_16k_mono(&f).unwrap();
        assert!((pcm.len() as f64 / 16000.0 - 3.0).abs() < 0.3, "{} samples", pcm.len());
        assert!(pcm.iter().any(|x| x.abs() > 0.05));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod ytdlp_version_tests {
    use super::newer_version;
    #[test]
    fn compares_release_dates() {
        assert!(newer_version("2026.11.23", "2026.10.26"));
        assert!(newer_version("2026.11.22.232654", "2026.11.22"));
        assert!(newer_version("2027.01.02", "2026.12.31"));
        assert!(!newer_version("2026.10.26", "2026.11.23"));
        assert!(!newer_version("2026.11.23", "2026.11.23"));
        assert!(!newer_version("", "2026.11.23"));
        assert!(!newer_version("2026.11.23", "nightly"));
    }
}
