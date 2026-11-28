//! Video highlight clipping & bilingual overlay rendering using yt-dlp + on-demand FFmpeg.
//!
//! Only the requested `[start, end]` range is fetched via `yt-dlp --download-sections` and cached
//! under `<data>/work/clips/<video_id>/`. Subtitles and title overlays are rendered in the WebView
//! onto transparent PNG canvases and composited by FFmpeg's `overlay` filter so every FFmpeg
//! build (even without `libass`/`drawtext`) produces crisp bilingual H.264 + AAC MP4 clips.
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::asr;

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct FfmpegStatus {
    pub installed: bool,
    pub path: Option<String>,
    pub hardware_h264: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PreparedClip {
    pub path: String,
    /// The clip's own length (the file may start `lead` seconds earlier).
    pub duration: f64,
    /// Seconds before the clip starts in the file: a stream copy begins at a keyframe; the export trims it.
    pub lead: f64,
    pub width: u32,
    pub height: u32,
    pub has_burned_subtitles: bool,
    pub cached: bool,
    pub frames: Vec<String>,
    pub best_frame: usize,
    /// Pauses in the clip (clip time, after `lead`): where "remove pauses" may cut.
    pub silences: Vec<(f64, f64)>,
    /// Where the speaker sits across the clip: (clip time, 0–1 across the picture), eased between moves.
    /// One point when they stay put; empty when no one was found.
    pub subject_track: Vec<(f64, f64)>,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CueOverlay {
    pub start: f64,
    pub end: f64,
    pub y: i32,
    pub png_base64: String,
}

/// The output canvas and where the picture goes on it (computed by the dialog, which draws the overlays to match).
/// `fit`: "fill" crops the picture to cover the canvas (panned by `crop_x`), "card" fits it into the box
/// `vx,vy,vw,vh` over a blurred or plain background.
#[derive(Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub fit: String,
    #[serde(default)] pub vx: u32,
    #[serde(default)] pub vy: u32,
    #[serde(default)] pub vw: u32,
    #[serde(default)] pub vh: u32,
    /// Card only: crop the picture to the box's shape (panned by `crop_x`) instead of fitting it whole.
    #[serde(default)] pub crop_box: bool,
}

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RenderRequest {
    #[serde(default)]
    pub frame: Option<Frame>,
    /// Seconds to skip at the start of `raw_path` (see PreparedClip::lead).
    #[serde(default)]
    pub lead: f64,
    /// Playback speed of the export (0.5–2); cue times arrive already on the sped-up clock.
    #[serde(default)]
    pub speed: f64,
    /// Where clips go unless the user picked a file (Settings › 存储); Downloads when empty.
    #[serde(default)]
    pub output_dir: Option<String>,
    pub raw_path: String,
    #[serde(default)]
    pub output_path: Option<String>,
    pub default_name: String,
    pub layout: String,
    pub card_bg: String,
    pub crop_x: f64,
    pub duration: f64,
    #[serde(default)]
    pub static_overlay_png: Option<String>,
    #[serde(default)]
    pub cues: Vec<CueOverlay>,
    #[serde(default)]
    pub cover_png: Option<String>,
    #[serde(default)]
    pub cover_mode: String,
    #[serde(default)]
    pub embed_thumbnail: bool,
    #[serde(default)]
    pub save_cover_sidecar: bool,
    #[serde(default)]
    pub landscape_cover_png: Option<String>,
    /// Stretches of the clip to keep (clip time, before speed), in order; empty keeps it whole.
    /// Cue times arrive already on the cut (and sped-up) clock.
    #[serde(default)]
    pub keep_ranges: Vec<(f64, f64)>,
    /// Bring the sound to −14 LUFS (what the short-video platforms normalise to), so every clip plays at the
    /// same level whatever show it came from.
    #[serde(default)]
    pub loudness: bool,
    /// Smart framing: the crop follows these (clip time, 0–1) points instead of staying at `crop_x`.
    #[serde(default)]
    pub pan: Vec<(f64, f64)>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct RenderResult {
    pub video_path: String,
    pub cover_path: Option<String>,
    pub landscape_cover_path: Option<String>,
}

fn exe(name: &str) -> String {
    if cfg!(windows) { format!("{name}.exe") } else { name.to_string() }
}

/// Finds `ffmpeg` in SubCast's own `bin/`, PATH, or standard package-manager directories.
pub fn find_ffmpeg(data: &Path) -> Option<PathBuf> {
    let name = exe("ffmpeg");
    let own = data.join("bin").join(&name);
    if own.is_file() { return Some(own); }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for d in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/opt/local/bin"] {
        dirs.push(d.into());
    }
    if let Some(h) = dirs::home_dir() { dirs.push(h.join(".local/bin")); }
    dirs.into_iter().map(|d| d.join(&name)).find(|p| p.is_file())
}

static VT_CACHE: parking_lot::Mutex<Option<(PathBuf, bool)>> = parking_lot::Mutex::new(None);

/// Checks whether the FFmpeg binary supports macOS hardware H.264 encoding (`h264_videotoolbox`).
pub fn has_videotoolbox(bin: &Path) -> bool {
    if !cfg!(target_os = "macos") { return false; }
    if let Some((cached, ok)) = VT_CACHE.lock().as_ref() {
        if cached == bin { return *ok; }
    }
    let mut cmd = std::process::Command::new(bin);
    cmd.args(["-hide_banner", "-encoders"]);
    let ok = cmd.output().map(|o| {
        o.status.success() && String::from_utf8_lossy(&o.stdout).contains("h264_videotoolbox")
    }).unwrap_or(false);
    *VT_CACHE.lock() = Some((bin.to_path_buf(), ok));
    ok
}

pub fn env_status(data: &Path) -> FfmpegStatus {
    match find_ffmpeg(data) {
        Some(bin) => {
            let hardware_h264 = has_videotoolbox(&bin);
            FfmpegStatus {
                installed: true,
                path: Some(bin.to_string_lossy().into_owned()),
                hardware_h264,
            }
        }
        None => FfmpegStatus { installed: false, path: None, hardware_h264: false },
    }
}

const FFMPEG_MIRROR: &str = "https://subcast.qiaomu.ai/tools/ffmpeg";

fn ffmpeg_urls() -> Vec<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        vec![
            "https://subcast.qiaomu.ai/tools/ffmpeg/ffmpeg-macos-arm64.zip",
            "https://www.osxexperts.net/ffmpeg711arm.zip",
        ]
    } else if cfg!(target_os = "macos") {
        vec![
            "https://subcast.qiaomu.ai/tools/ffmpeg/ffmpeg-macos-x64.zip",
            "https://evermeet.cx/ffmpeg/getrelease/zip",
        ]
    } else if cfg!(target_os = "windows") {
        vec![
            "https://subcast.qiaomu.ai/tools/ffmpeg/ffmpeg-windows-x64.zip",
            "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip",
        ]
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        vec!["https://subcast.qiaomu.ai/tools/ffmpeg/ffmpeg-linux-arm64.tar.gz"]
    } else {
        vec!["https://subcast.qiaomu.ai/tools/ffmpeg/ffmpeg-linux-x64.tar.gz"]
    }
}

static FFMPEG_INSTALL_LOCK: once_cell::sync::Lazy<tokio::sync::Mutex<()>> =
    once_cell::sync::Lazy::new(|| tokio::sync::Mutex::new(()));

/// Finds FFmpeg, or downloads a static build into `<data>/bin/` on first use.
pub async fn ensure_ffmpeg(
    data: &Path,
    cancel: &AtomicBool,
    mut progress: impl FnMut(u64, Option<u64>),
) -> Result<PathBuf> {
    if let Some(p) = find_ffmpeg(data) { return Ok(p); }
    let _guard = FFMPEG_INSTALL_LOCK.lock().await;
    if let Some(p) = find_ffmpeg(data) { return Ok(p); }

    let _ = FFMPEG_MIRROR;
    let target_name = exe("ffmpeg");
    let dest = data.join("bin").join(&target_name);
    let tmp_dir = data.join("bin").join(".ffmpeg-dl");

    for url in ffmpeg_urls() {
        if cancel.load(Ordering::Relaxed) { bail!("已取消"); }
        let ext = if url.ends_with(".tar.gz") { "tar.gz" } else { "zip" };
        let archive = tmp_dir.join(format!("ffmpeg.{ext}"));
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        if asr::download(url, &archive, cancel, |a, b| progress(a, b)).await.is_err() {
            let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
            continue;
        }
        let a = archive.clone();
        let d = dest.clone();
        let tn = target_name.clone();
        let extracted = tokio::task::spawn_blocking(move || extract_ffmpeg_archive(&a, &d, &tn)).await;
        let _ = tokio::fs::remove_dir_all(&tmp_dir).await;
        if let Ok(Ok(())) = extracted {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
            }
            if dest.is_file() { return Ok(dest); }
        }
    }
    Err(anyhow!("未找到 ffmpeg，请先安装后重试"))
}

fn extract_ffmpeg_archive(archive: &Path, dest: &Path, bin_name: &str) -> Result<()> {
    if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent)?; }
    let tmp = dest.with_extension("part");
    let file = std::fs::File::open(archive)?;
    if archive.to_string_lossy().ends_with(".tar.gz") {
        let mut ar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        let mut found = false;
        for entry in ar.entries()? {
            let mut e = entry?;
            let path = e.path()?;
            if path.file_name().and_then(|n| n.to_str()) == Some(bin_name) && e.header().entry_type().is_file() {
                let mut out = std::fs::File::create(&tmp)?;
                std::io::copy(&mut e, &mut out)?;
                found = true;
                break;
            }
        }
        if !found { bail!("missing ffmpeg in tar.gz"); } // i18n-ignore
    } else {
        let mut zip = zip::ZipArchive::new(file)?;
        let mut found = false;
        for i in 0..zip.len() {
            let mut f = zip.by_index(i)?;
            if !f.is_file() { continue; }
            let name = Path::new(f.name()).file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == bin_name {
                let mut out = std::fs::File::create(&tmp)?;
                std::io::copy(&mut f, &mut out)?;
                found = true;
                break;
            }
        }
        if !found { bail!("missing ffmpeg in zip"); } // i18n-ignore
    }
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

pub fn safe_slug(s: &str) -> String {
    let cleaned: String = s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(64)
        .collect();
    if cleaned.is_empty() { "clip".into() } else { cleaned }
}

pub fn clips_work_dir(data: &Path, video_id: &str) -> PathBuf {
    data.join("work").join("clips").join(safe_slug(video_id))
}

fn ffmpeg_cmd(bin: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(bin);
    #[cfg(windows)]
    { cmd.creation_flags(0x08000000); }
    cmd.kill_on_drop(true);
    cmd
}

#[derive(Clone, Debug)]
pub struct VideoMeta {
    pub duration: f64,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub pix_fmt: Option<String>,
}

impl VideoMeta {
    /// True when the clip is already H.264 yuv420p (even dims) + AAC/silent, so we can remux with `-c copy`.
    pub fn is_web_ready(&self) -> bool {
        self.video_codec.as_deref() == Some("h264")
            && self.pix_fmt.as_deref().is_some_and(|p| p.starts_with("yuv420p"))
            && self.width > 0
            && self.height > 0
            && self.width % 2 == 0
            && self.height % 2 == 0
            && (!self.has_audio || self.audio_codec.as_deref() == Some("aac"))
    }
}

/// Parses duration, dimensions, codecs, and pixel format from `ffmpeg -hide_banner -i <file>` stderr.
pub fn parse_video_detail(stderr: &str) -> VideoMeta {
    let duration = regex::Regex::new(r"Duration:\s*(\d+):(\d{2}):(\d{2}(?:\.\d+)?)")
        .ok()
        .and_then(|re| re.captures(stderr))
        .and_then(|c| {
            let h: f64 = c[1].parse().ok()?;
            let m: f64 = c[2].parse().ok()?;
            let s: f64 = c[3].parse().ok()?;
            Some(h * 3600.0 + m * 60.0 + s)
        })
        .unwrap_or(0.0);

    let video_line = stderr.lines().find(|l| l.contains("Video:"));
    let (width, height) = video_line
        .and_then(|line| {
            regex::Regex::new(r"\b(\d{3,5})x(\d{3,5})\b").ok()?.captures(line).and_then(|c| {
                Some((c[1].parse::<u32>().ok()?, c[2].parse::<u32>().ok()?))
            })
        })
        .unwrap_or((1920, 1080));

    let video_codec = video_line
        .and_then(|line| line.split("Video:").nth(1))
        .and_then(|rest| rest.trim().split(|c: char| c.is_whitespace() || c == ',' || c == '(').next())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());

    let pix_fmt = video_line.and_then(|line| {
        regex::Regex::new(r"\b(yuv[a-z0-9_]+|rgb[a-z0-9_]+|nv12|gbrp[a-z0-9_]*)\b")
            .ok()?
            .captures(line)
            .map(|c| c[1].to_ascii_lowercase())
    });

    let audio_line = stderr.lines().find(|l| l.contains("Audio:"));
    let has_audio = audio_line.is_some();
    let audio_codec = audio_line
        .and_then(|line| line.split("Audio:").nth(1))
        .and_then(|rest| rest.trim().split(|c: char| c.is_whitespace() || c == ',' || c == '(').next())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase());

    VideoMeta { duration, width, height, has_audio, video_codec, audio_codec, pix_fmt }
}

/// Parses `Duration: HH:MM:SS.xx`, video `WxH`, and audio presence from `ffmpeg -hide_banner -i <file>` stderr.
pub fn parse_video_meta(stderr: &str) -> (f64, u32, u32, bool) {
    let m = parse_video_detail(stderr);
    (m.duration, m.width, m.height, m.has_audio)
}

pub async fn probe_video_detail(ffmpeg: &Path, path: &Path, fallback_dur: f64) -> VideoMeta {
    let mut cmd = ffmpeg_cmd(ffmpeg);
    cmd.args(["-hide_banner", "-i"]).arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let out = tokio::time::timeout(std::time::Duration::from_secs(15), cmd.output()).await;
    if let Ok(Ok(o)) = out {
        let stderr = String::from_utf8_lossy(&o.stderr);
        let mut m = parse_video_detail(&stderr);
        if m.duration <= 0.1 { m.duration = fallback_dur; }
        return m;
    }
    VideoMeta {
        duration: fallback_dur,
        width: 1920,
        height: 1080,
        has_audio: true,
        video_codec: None,
        audio_codec: None,
        pix_fmt: None,
    }
}

pub async fn probe_video_meta(ffmpeg: &Path, path: &Path, fallback_dur: f64) -> (f64, u32, u32, bool) {
    let m = probe_video_detail(ffmpeg, path, fallback_dur).await;
    (m.duration, m.width, m.height, m.has_audio)
}

/// Detects whether the source frame already has changing burned-in captions in its lower 35% band.
/// Static watermarks/logos fail the overlap threshold because their pixels stay identical across samples.
pub fn decide_burned_subtitles(frames: &[&[u8]], width: usize, height: usize) -> bool {
    const BRIGHT: u8 = 200;
    const DARK: u8 = 80;
    const REACH: usize = 2;
    const MIN_FRAC: f64 = 0.0012;
    const MAX_OVERLAP: f64 = 0.5;

    if frames.len() < 3 || width == 0 || height == 0 { return false; }
    let left = (width as f64 * 0.15) as usize;
    let right = (width as f64 * 0.85) as usize;
    let threshold = (MIN_FRAC * (width * height) as f64) as usize;

    let mut masks: Vec<HashSet<usize>> = Vec::new();
    for &pix in frames {
        if pix.len() != width * height { continue; }
        let mut mask = HashSet::new();
        for y in 0..height {
            let row = &pix[y * width..(y + 1) * width];
            for x in left..right.min(width) {
                if row[x] < BRIGHT { continue; }
                let lo = x.saturating_sub(REACH);
                let hi = (x + REACH + 1).min(width);
                if row[lo..hi].iter().copied().min().unwrap_or(255) < DARK {
                    mask.insert(y * width + x);
                }
            }
        }
        if mask.len() >= threshold { masks.push(mask); }
    }
    if masks.len() < 3 || (masks.len() as f64) < frames.len() as f64 * 0.5 { return false; }
    let mut overlaps: Vec<f64> = masks.windows(2).map(|w| {
        let inter = w[0].intersection(&w[1]).count();
        let uni = w[0].union(&w[1]).count().max(1);
        inter as f64 / uni as f64
    }).collect();
    overlaps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    overlaps[overlaps.len() / 2] < MAX_OVERLAP
}

pub async fn detect_burned_subtitles(ffmpeg: &Path, path: &Path, duration: f64) -> bool {
    const W: usize = 480;
    const H: usize = 96;
    const SAMPLES: usize = 6;
    if duration <= 0.6 { return false; }

    let mut set = tokio::task::JoinSet::new();
    for i in 0..SAMPLES {
        let frac = 0.08 + 0.84 * (i as f64 / (SAMPLES - 1) as f64);
        let at = duration * frac;
        let ff = ffmpeg.to_path_buf();
        let p = path.to_path_buf();
        set.spawn(async move {
            let mut cmd = ffmpeg_cmd(&ff);
            cmd.args(["-v", "error", "-ss", &format!("{at:.2}"), "-i"]).arg(&p)
                .args([
                    "-frames:v", "1",
                    "-vf", &format!("crop=iw:ih*0.35:0:ih*0.65,scale={W}:{H},format=gray"),
                    "-f", "rawvideo", "-",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            if let Ok(Ok(out)) = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output()).await {
                if out.status.success() && out.stdout.len() == W * H {
                    return Some((i, out.stdout));
                }
            }
            None
        });
    }
    let mut indexed: Vec<(usize, Vec<u8>)> = Vec::with_capacity(SAMPLES);
    while let Some(res) = set.join_next().await {
        if let Ok(Some(item)) = res { indexed.push(item); }
    }
    indexed.sort_by_key(|(i, _)| *i);
    let refs: Vec<&[u8]> = indexed.iter().map(|(_, v)| v.as_slice()).collect();
    decide_burned_subtitles(&refs, W, H)
}

/// Looks for an already-cached clip file `[cs, ce]` in `dir` that completely covers `[start_ms, end_ms]`.
pub fn find_covering_cache(dir: &Path, start_ms: u64, end_ms: u64) -> Option<(PathBuf, u64, u64)> {
    let rd = std::fs::read_dir(dir).ok()?;
    let mut best: Option<(PathBuf, u64, u64, u64)> = None;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("mp4") { continue; }
        let len = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        if len < 10_000 { continue; }
        let Some(stem) = p.file_stem().and_then(|s| s.to_str()) else { continue };
        let Some((a, b)) = stem.split_once('-') else { continue };
        let (Ok(cs), Ok(ce)) = (a.parse::<u64>(), b.parse::<u64>()) else { continue };
        if cs <= start_ms && ce >= end_ms {
            let span = ce - cs;
            if best.as_ref().is_none_or(|(_, _, _, bs)| span < *bs) {
                best = Some((p, cs, ce, span));
            }
        }
    }
    best.map(|(p, cs, ce, _)| (p, cs, ce))
}

#[derive(Serialize, Deserialize)]
struct CachedMeta {
    duration: f64,
    width: u32,
    height: u32,
    has_burned_subtitles: bool,
}

#[derive(Serialize, Deserialize)]
struct CachedFrames {
    frames: Vec<String>,
    best_frame: usize,
}

/// Extracts 5 candidate still frames concurrently as base64 JPEG data URLs and picks the sharpest non-blank frame.
pub async fn extract_candidate_frames(ffmpeg: &Path, path: &Path, duration: f64) -> (Vec<String>, usize) {
    const FRACS: [f64; 5] = [0.10, 0.28, 0.48, 0.68, 0.85];
    let dur = duration.max(0.2);

    let mut set = tokio::task::JoinSet::new();
    for (idx, frac) in FRACS.into_iter().enumerate() {
        let at = (dur * frac).max(0.05).min((dur - 0.08).max(0.05));
        let ff = ffmpeg.to_path_buf();
        let p = path.to_path_buf();
        set.spawn(async move {
            let mut cmd = ffmpeg_cmd(&ff);
            cmd.args(["-v", "error", "-ss", &format!("{at:.2}"), "-i"]).arg(&p)
                .args(["-frames:v", "1", "-vf", "scale=1280:-2", "-q:v", "4", "-f", "image2pipe", "-vcodec", "mjpeg", "-"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            if let Ok(Ok(out)) = tokio::time::timeout(std::time::Duration::from_secs(10), cmd.output()).await {
                if out.status.success() && out.stdout.len() > 256 {
                    let len = out.stdout.len();
                    let b64 = base64::engine::general_purpose::STANDARD.encode(&out.stdout);
                    return Some((idx, len, format!("data:image/jpeg;base64,{b64}")));
                }
            }
            None
        });
    }

    let mut indexed: Vec<(usize, usize, String)> = Vec::with_capacity(FRACS.len());
    while let Some(res) = set.join_next().await {
        if let Ok(Some(item)) = res { indexed.push(item); }
    }
    indexed.sort_by_key(|(idx, _, _)| *idx);

    let mut frames: Vec<String> = Vec::with_capacity(indexed.len());
    let mut best_idx = 0usize;
    let mut best_bytes = 0usize;
    for (_, len, url) in indexed {
        if len > best_bytes {
            best_bytes = len;
            best_idx = frames.len();
        }
        frames.push(url);
    }
    (frames, best_idx)
}

/// Parses `silence_start: X` / `silence_end: Y` pairs from ffmpeg `silencedetect` stderr (file time).
/// A pause still open at the end of the file closes at `file_end`.
pub fn parse_silences(stderr: &str, file_end: f64) -> Vec<(f64, f64)> {
    let num_after = |line: &str, key: &str| -> Option<f64> {
        let i = line.find(key)?;
        line[i + key.len()..].trim_start().split(|c: char| c.is_whitespace() || c == '|').next()?.parse().ok()
    };
    let mut out = Vec::new();
    let mut open: Option<f64> = None;
    for line in stderr.lines() {
        if let Some(v) = num_after(line, "silence_start:") { open = Some(v.max(0.0)); }
        else if let Some(v) = num_after(line, "silence_end:") {
            if let Some(a) = open.take() { if v > a { out.push((a, v)); } }
        }
    }
    if let Some(a) = open { if file_end > a { out.push((a, file_end)); } }
    out
}

/// Pauses longer than 0.3 s in the clip, in clip time (the file decoded from 0, then `lead` taken off).
pub async fn detect_silences(ffmpeg: &Path, path: &Path, lead: f64, duration: f64) -> Vec<(f64, f64)> {
    let mut cmd = ffmpeg_cmd(ffmpeg);
    cmd.args(["-hide_banner", "-nostats", "-i"]).arg(path)
        .args(["-vn", "-af", "silencedetect=noise=-32dB:d=0.3", "-f", "null", "-"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let Ok(Ok(out)) = tokio::time::timeout(std::time::Duration::from_secs(30), cmd.output()).await else { return Vec::new(); };
    let text = String::from_utf8_lossy(&out.stderr);
    to_clip_time(&parse_silences(&text, lead + duration), lead, duration)
}

/// File-time ranges moved onto the clip's clock (`lead` off) and clamped to `0..duration`.
pub fn to_clip_time(ranges: &[(f64, f64)], lead: f64, duration: f64) -> Vec<(f64, f64)> {
    ranges.iter()
        .map(|&(a, b)| ((a - lead).max(0.0), (b - lead).min(duration)))
        .filter(|&(a, b)| b - a > 0.05)
        .collect()
}

/// Seconds before the clip proper in a fetched file (a `.lead` file beside it; none means 0).
pub fn read_lead(path: &Path) -> f64 {
    std::fs::read_to_string(path.with_extension("lead")).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0.0)
}

/// Seconds between the stills the speaker is looked for in.
const TRACK_STEP: f64 = 2.0;
const TRACK_W: usize = 96;
const TRACK_H: usize = 54;

/// Where a person sits across a small RGB still (0–1), from skin-coloured columns: the densest stretch, not the
/// mean (two people would average to the empty middle). `None` with too little skin to go on. Same rule as the
/// interface's `subjectCenterX`.
pub fn subject_x(rgb: &[u8], w: usize, h: usize) -> Option<f64> {
    let mut cols = vec![0f64; w];
    let mut total = 0f64;
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 3;
            let (r, g, b) = (rgb[i] as f64, rgb[i + 1] as f64, rgb[i + 2] as f64);
            let cb = 128.0 - 0.168736 * r - 0.331264 * g + 0.5 * b;
            let cr = 128.0 + 0.5 * r - 0.418688 * g - 0.081312 * b;
            let yy = 0.299 * r + 0.587 * g + 0.114 * b;
            if yy > 50.0 && (77.0..=127.0).contains(&cb) && (135.0..=173.0).contains(&cr) { cols[x] += 1.0; total += 1.0; }
        }
    }
    if total <= (w * h) as f64 * 0.004 { return None; }
    let win = ((w as f64) * 0.22).round().max(1.0) as usize;
    let (mut best, mut at, mut run) = (-1f64, 0f64, 0f64);
    for x in 0..w {
        run += cols[x] - if x >= win { cols[x - win] } else { 0.0 };
        if x + 1 >= win && run > best { best = run; at = x as f64 - win as f64 / 2.0 + 0.5; }
    }
    Some((at / w as f64).clamp(0.05, 0.95))
}

/// How far (share of the width) the speaker must move, seen twice, before the frame follows. Interviews
/// wobble ±10% between camera cuts; following that reads as drift.
const TRACK_MOVE: f64 = 0.15;
/// Stills in a row a new place must hold before the frame goes there.
const TRACK_HOLD: usize = 3;
/// Half the time a move takes: short, since most moves are camera cuts that land at once.
const TRACK_EASE: f64 = 0.15;

/// Raw samples into a calm track: gaps take the last known place, single-frame outliers go (median of three),
/// and the frame only moves when the speaker has clearly moved (`TRACK_MOVE`, held `TRACK_HOLD` stills), over 0.3 s.
/// Returns (time, x) points to interpolate between; one point when nothing moves.
pub fn smooth_track(samples: &[(f64, Option<f64>)], duration: f64) -> Vec<(f64, f64)> {
    let first = match samples.iter().find_map(|s| s.1) { Some(x) => x, None => return vec![] };
    let mut last = first;
    let filled: Vec<(f64, f64)> = samples.iter().map(|&(t, x)| { if let Some(v) = x { last = v; } (t, last) }).collect();
    let med: Vec<(f64, f64)> = (0..filled.len()).map(|i| {
        let mut w = [filled[i.saturating_sub(1)].1, filled[i].1, filled[(i + 1).min(filled.len() - 1)].1];
        w.sort_by(f64::total_cmp);
        (filled[i].0, w[1])
    }).collect();
    let mut points = vec![(0.0, med[0].1)];
    let mut cur = med[0].1;
    // A new place counts once it has held for TRACK_HOLD samples: props in skin tones (a row of yellow cans)
    // or a raised hand can win one or two stills. The move is placed back at its first sample.
    let mut pending: Option<(usize, f64, usize)> = None; // (first sample, place, samples seen)
    for i in 1..med.len() {
        let x = med[i].1;
        if (x - cur).abs() < TRACK_MOVE { pending = None; continue; }
        pending = match pending {
            Some((first, p, n)) if (x - p).abs() < TRACK_MOVE => Some((first, p, n + 1)),
            _ => Some((i, x, 1)),
        };
        if let Some((first, p, n)) = pending {
            if n >= TRACK_HOLD {
                let at = med[first].0;
                let (a, b) = ((at - TRACK_EASE).max(points.last().unwrap().0 + 0.05), (at + TRACK_EASE).min(duration));
                if b > a {
                    points.push((a, cur));
                    points.push((b, p));
                    cur = p;
                }
                pending = None;
            }
        }
    }
    if points.len() == 1 { return points; }
    points
}

/// Looks for the speaker every `TRACK_STEP` s of the clip (clip time) and returns the eased track.
pub async fn track_subject(ffmpeg: &Path, path: &Path, lead: f64, duration: f64) -> Vec<(f64, f64)> {
    let mut cmd = ffmpeg_cmd(ffmpeg);
    cmd.args(["-hide_banner", "-loglevel", "error", "-ss", &format!("{:.3}", lead.max(0.0)), "-t", &format!("{duration:.3}"), "-i"]).arg(path)
        .args(["-an", "-vf", &format!("fps=1/{TRACK_STEP},scale={TRACK_W}:{TRACK_H}"), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(Ok(out)) = tokio::time::timeout(std::time::Duration::from_secs(40), cmd.output()).await else { return vec![]; };
    let size = TRACK_W * TRACK_H * 3;
    let samples: Vec<(f64, Option<f64>)> = out.stdout.chunks_exact(size).enumerate()
        .map(|(k, f)| (k as f64 * TRACK_STEP, subject_x(f, TRACK_W, TRACK_H)))
        .collect();
    smooth_track(&samples, duration)
}

/// Clip time onto the export's clock: the cut stretches taken out, then the speed.
pub fn to_output_time(t: f64, keep: &[(f64, f64)], speed: f64) -> f64 {
    let kept = if keep.is_empty() { t } else {
        keep.iter().map(|&(a, b)| if t >= b { b - a } else if t > a { t - a } else { 0.0 }).sum()
    };
    kept / speed.max(0.01)
}

/// An FFmpeg expression in `t` that walks the track: linear between points, held before the first and after
/// the last, kept inside 0–1. `None` for a track that never moves.
pub fn pan_expr(points: &[(f64, f64)]) -> Option<String> {
    if points.len() < 2 { return None; }
    let n = points.len();
    let mut e = format!("{:.4}", points[n - 1].1);
    for i in (0..n - 1).rev() {
        let ((t0, x0), (t1, x1)) = (points[i], points[i + 1]);
        let seg = if (x1 - x0).abs() < 1e-4 || t1 - t0 < 1e-3 { format!("{x0:.4}") }
            else { format!("{x0:.4}+({:.4})*(t-{t0:.3})/{:.3}", x1 - x0, t1 - t0) };
        e = format!("if(lt(t,{t1:.3}),{seg},{e})");
    }
    Some(format!("clip(if(lt(t,{:.3}),{:.4},{e}),0,1)", points[0].0, points[0].1))
}

async fn finish_prepared(ffmpeg: &Path, dest: &Path, fallback_dur: f64, cached: bool) -> PreparedClip {
    let lead = read_lead(dest);
    let meta_path = dest.with_extension("meta.json");
    let frames_path = dest.with_extension("frames.json");
    let silence_path = dest.with_extension("silence.json");

    let (meta_raw, frames_raw) = tokio::join!(
        tokio::fs::read_to_string(&meta_path),
        tokio::fs::read_to_string(&frames_path),
    );
    let cached_meta = meta_raw.ok().and_then(|raw| serde_json::from_str::<CachedMeta>(&raw).ok());
    let cached_frames = frames_raw
        .ok()
        .and_then(|raw| serde_json::from_str::<CachedFrames>(&raw).ok())
        .filter(|cf| !cf.frames.is_empty());

    let (duration, width, height, need_burned) = if let Some(ref m) = cached_meta {
        (m.duration, m.width, m.height, false)
    } else {
        let (d, w, h, _) = probe_video_meta(ffmpeg, dest, fallback_dur).await;
        (d, w, h, true)
    };

    let clip_dur = (duration - lead).max(0.1);
    // "track3": made with TRACK_MOVE / TRACK_HOLD / TRACK_EASE; an older track is redone.
    let track_path = dest.with_extension("track3.json");
    let (has_burned_subtitles, (frames, best_frame), silences, subject_track) = tokio::join!(
        async {
            if let Some(m) = cached_meta {
                m.has_burned_subtitles
            } else {
                let burned = detect_burned_subtitles(ffmpeg, dest, duration).await;
                if need_burned {
                    let _ = tokio::fs::write(&meta_path, serde_json::to_string(&CachedMeta {
                        duration, width, height, has_burned_subtitles: burned,
                    }).unwrap_or_default()).await;
                }
                burned
            }
        },
        async {
            if let Some(cf) = cached_frames {
                let best = cf.best_frame.min(cf.frames.len() - 1);
                (cf.frames, best)
            } else {
                let res = extract_candidate_frames(ffmpeg, dest, duration).await;
                if !res.0.is_empty() {
                    let _ = tokio::fs::write(&frames_path, serde_json::to_string(&CachedFrames {
                        frames: res.0.clone(), best_frame: res.1,
                    }).unwrap_or_default()).await;
                }
                res
            }
        },
        async {
            let cached = tokio::fs::read_to_string(&silence_path).await.ok()
                .and_then(|raw| serde_json::from_str::<Vec<(f64, f64)>>(&raw).ok());
            match cached {
                Some(s) => s,
                None => {
                    let s = detect_silences(ffmpeg, dest, lead, clip_dur).await;
                    // Only a found result is kept: an empty one may be a failed run.
                    if !s.is_empty() { let _ = tokio::fs::write(&silence_path, serde_json::to_string(&s).unwrap_or_default()).await; }
                    s
                }
            }
        },
        async {
            let cached = tokio::fs::read_to_string(&track_path).await.ok()
                .and_then(|raw| serde_json::from_str::<Vec<(f64, f64)>>(&raw).ok());
            match cached {
                Some(t) => t,
                None => {
                    let t = track_subject(ffmpeg, dest, lead, clip_dur).await;
                    if !t.is_empty() { let _ = tokio::fs::write(&track_path, serde_json::to_string(&t).unwrap_or_default()).await; }
                    t
                }
            }
        }
    );

    PreparedClip {
        path: dest.to_string_lossy().into_owned(),
        duration: (duration - lead).max(0.1),
        lead,
        width,
        height,
        has_burned_subtitles,
        cached,
        frames,
        best_frame,
        silences,
        subject_track,
    }
}

/// Parses FFmpeg stderr progress line `... time=HH:MM:SS.xx ...` into seconds.
pub fn parse_ffmpeg_time(line: &str) -> Option<f64> {
    let idx = line.find("time=")?;
    let rest = line[idx + 5..].trim_start();
    let token = rest.split_whitespace().next()?;
    let mut parts = token.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let m: f64 = parts.next()?.parse().ok()?;
    let s: f64 = parts.next()?.parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + s)
}

async fn run_ffmpeg_with_progress(
    ffmpeg: &Path,
    work_dir: Option<&Path>,
    args: &[String],
    total_dur: f64,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f64),
) -> Result<()> {
    let mut cmd = ffmpeg_cmd(ffmpeg);
    if let Some(d) = work_dir { cmd.current_dir(d); }
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    let mut child = cmd.spawn().context("无法启动 ffmpeg")?;
    let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
    let mut tail: Vec<String> = Vec::new();
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill().await;
            return Err(anyhow!("已取消"));
        }
        tokio::select! {
            line = stderr.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        if let Some(sec) = parse_ffmpeg_time(&l) {
                            if total_dur > 0.1 { on_progress((sec / total_dur).clamp(0.0, 0.99)); }
                        }
                        if !l.trim().is_empty() {
                            tail.push(l);
                            if tail.len() > 10 { tail.remove(0); }
                        }
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(200)) => {
                if cancel.load(Ordering::Relaxed) {
                    let _ = child.kill().await;
                    return Err(anyhow!("已取消"));
                }
            }
        }
    }
    let status = child.wait().await?;
    if !status.success() {
        let msg = tail.last().cloned().unwrap_or_default();
        return Err(anyhow!("视频处理失败：{}", msg.chars().take(180).collect::<String>()));
    }
    Ok(())
}

/// Separate video and audio as one MP4 without re-encoding the picture: the video is copied from its first
/// keyframe (`v0`, a little before the stretch), the audio (cheap to encode) is aligned to it. The export trims
/// the lead exactly, so the picture is encoded once in all. Writes `<dest>.lead`.
pub async fn mux_copy(ffmpeg: &Path, video: &Path, v0: f64, audio: &Path, a0: f64, start: f64, end: f64, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    let tmp = dest.with_extension("part.mp4");
    let mut args: Vec<String> = vec!["-y".into(), "-i".into(), video.to_string_lossy().into_owned()];
    // Audio starting before the video is cut to it; starting after, it is delayed (inside the lead).
    if a0 < v0 { args.extend(["-ss".into(), format!("{:.3}", v0 - a0)]); } else if a0 > v0 { args.extend(["-itsoffset".into(), format!("{:.3}", a0 - v0)]); }
    args.extend(["-i".into(), audio.to_string_lossy().into_owned()]);
    args.extend([
        "-t".into(), format!("{:.3}", end - v0), "-map".into(), "0:v:0".into(), "-map".into(), "1:a:0".into(),
        "-c:v".into(), "copy".into(), "-c:a".into(), "aac".into(), "-b:a".into(), "192k".into(),
        "-movflags".into(), "+faststart".into(), tmp.to_string_lossy().into_owned(),
    ]);
    run_ffmpeg_with_progress(ffmpeg, None, &args, end - v0, cancel, |_| {}).await?;
    tokio::fs::rename(&tmp, dest).await?;
    tokio::fs::write(dest.with_extension("lead"), format!("{:.3}", (start - v0).max(0.0))).await?;
    Ok(())
}

#[allow(dead_code)]
pub async fn encode_exact(ffmpeg: &Path, video: &Path, v_ss: f64, audio: &Path, a_ss: f64, dur: f64, dest: &Path, cancel: &AtomicBool) -> Result<()> {
    let tmp = dest.with_extension("part.mp4");
    for hw in [has_videotoolbox(ffmpeg), false] {
        let mut args: Vec<String> = vec![
            "-y".into(), "-ss".into(), format!("{:.3}", v_ss.max(0.0)), "-i".into(), video.to_string_lossy().into_owned(),
            "-ss".into(), format!("{:.3}", a_ss.max(0.0)), "-i".into(), audio.to_string_lossy().into_owned(),
            "-t".into(), format!("{dur:.3}"), "-map".into(), "0:v:0".into(), "-map".into(), "1:a:0".into(),
            "-vf".into(), "scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p".into(),
        ];
        if hw { args.extend(["-c:v".into(), "h264_videotoolbox".into(), "-b:v".into(), "12M".into()]); }
        else { args.extend(["-c:v".into(), "libx264".into(), "-preset".into(), "veryfast".into(), "-crf".into(), "18".into()]); }
        args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "192k".into(), "-movflags".into(), "+faststart".into(), tmp.to_string_lossy().into_owned()]);
        match run_ffmpeg_with_progress(ffmpeg, None, &args, dur, cancel, |_| {}).await {
            Ok(()) => { tokio::fs::rename(&tmp, dest).await?; return Ok(()); }
            Err(e) if cancel.load(Ordering::Relaxed) || !hw => { let _ = tokio::fs::remove_file(&tmp).await; return Err(e); }
            Err(_) => { let _ = tokio::fs::remove_file(&tmp).await; }
        }
    }
    unreachable!()
}

/// Fast stream-copy remux with `+faststart` when source is already H.264/yuv420p/AAC.
async fn fast_remux_mp4(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<()> {
    let tmp = output.with_extension("part.mp4");
    let _ = tokio::fs::remove_file(&tmp).await;
    let args: Vec<String> = vec![
        "-y".into(),
        "-i".into(), input.to_string_lossy().into_owned(),
        "-c".into(), "copy".into(),
        "-movflags".into(), "+faststart".into(),
        tmp.to_string_lossy().into_owned(),
    ];
    match run_ffmpeg_with_progress(ffmpeg, None, &args, 0.0, cancel, |_| {}).await {
        Ok(()) => {
            tokio::fs::rename(&tmp, output).await?;
            Ok(())
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp).await;
            Err(e)
        }
    }
}

/// Encodes `input` (optionally trimmed by `trim_ss` / `trim_t`) into a standard H.264/yuv420p/AAC MP4.
async fn normalize_mp4(
    ffmpeg: &Path,
    input: &Path,
    output: &Path,
    trim: Option<(f64, f64)>,
    total_dur: f64,
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(f64),
) -> Result<()> {
    let tmp = output.with_extension("part.mp4");
    let _ = tokio::fs::remove_file(&tmp).await;
    let use_vt = has_videotoolbox(ffmpeg);

    for attempt in [use_vt, false] {
        let mut args = vec!["-y".into()];
        if let Some((ss, dur)) = trim {
            args.extend(["-ss".into(), format!("{ss:.3}"), "-i".into(), input.to_string_lossy().into_owned(), "-t".into(), format!("{dur:.3}")]);
        } else {
            args.extend(["-i".into(), input.to_string_lossy().into_owned()]);
        }
        args.extend(["-vf".into(), "scale=trunc(iw/2)*2:trunc(ih/2)*2,format=yuv420p".into()]);
        if attempt {
            args.extend(["-c:v".into(), "h264_videotoolbox".into(), "-b:v".into(), "5000k".into()]);
        } else {
            args.extend(["-c:v".into(), "libx264".into(), "-preset".into(), "superfast".into(), "-crf".into(), "20".into()]);
        }
        args.extend([
            "-c:a".into(), "aac".into(), "-b:a".into(), "192k".into(),
            "-movflags".into(), "+faststart".into(),
            tmp.to_string_lossy().into_owned(),
        ]);
        match run_ffmpeg_with_progress(ffmpeg, None, &args, total_dur, cancel, &mut on_progress).await {
            Ok(()) => {
                tokio::fs::rename(&tmp, output).await?;
                return Ok(());
            }
            Err(e) if cancel.load(Ordering::Relaxed) || !attempt => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(e);
            }
            Err(_) => {
                let _ = tokio::fs::remove_file(&tmp).await;
            }
        }
    }
    unreachable!()
}

async fn download_youtube_section(
    ytdlp: &Path,
    ffmpeg: &Path,
    video_id: &str,
    start: f64,
    end: f64,
    dir: &Path,
    cancel: Arc<AtomicBool>,
) -> Result<PathBuf> {
    asr::with_bot_fallback(|extra| {
        let c = cancel.clone();
        async move {
            let x: Vec<&str> = extra.iter().map(String::as_str).collect();
            download_youtube_section_once(ytdlp, ffmpeg, video_id, start, end, dir, c, &x).await
        }
    }).await
}

async fn download_youtube_section_once(
    ytdlp: &Path,
    ffmpeg: &Path,
    video_id: &str,
    start: f64,
    end: f64,
    dir: &Path,
    cancel: Arc<AtomicBool>,
    extra: &[&str],
) -> Result<PathBuf> {
    tokio::fs::create_dir_all(dir).await?;
    let tag = format!("raw-{}", uuid::Uuid::new_v4().simple());
    let tpl = dir.join(format!("{tag}.%(ext)s"));
    let mut cmd = asr::ytdlp_command(ytdlp);
    let section = format!("*{start:.2}-{end:.2}");
    cmd.args([
        "--download-sections", &section,
        "--force-keyframes-at-cuts",
        "--ffmpeg-location", &ffmpeg.to_string_lossy(),
        "-f", "bestvideo[height<=1080][vcodec^=avc1]+bestaudio[ext=m4a]/bestvideo[height<=1080][ext=mp4]+bestaudio[ext=m4a]/bestvideo[height<=1080]+bestaudio/best[height<=1080]/best",
        "--merge-output-format", "mp4",
        "--no-playlist",
        "--no-part",
    ])
    .args(extra)
    .arg("-o").arg(&tpl)
    .arg(format!("https://www.youtube.com/watch?v={video_id}"))
    .stdout(std::process::Stdio::null())
    .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().context("无法启动 yt-dlp")?;
    let stderr = child.stderr.take();
    let tail = tokio::spawn(async move {
        let mut text = String::new();
        if let Some(s) = stderr {
            let mut lines = BufReader::new(s).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if !l.trim().is_empty() { text = l; }
            }
        }
        text
    });
    loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill().await;
            return Err(anyhow!("已取消"));
        }
        if let Some(status) = child.try_wait()? {
            let last = tail.await.unwrap_or_default();
            if !status.success() {
                return Err(anyhow!("获取视频片段失败：{}", last.chars().take(180).collect::<String>()));
            }
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    let mut rd = tokio::fs::read_dir(dir).await?;
    while let Some(e) = rd.next_entry().await? {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with(&tag) && !name.ends_with(".part") {
            return Ok(e.path());
        }
    }
    Err(anyhow!("没有找到下载的视频片段"))
}

/// Several stretches of one video cut together in order (a montage): each is fetched (and cached) on its own,
/// then joined with one encode. Burned-in subtitle detection and frames come from the joined file.
pub async fn prepare_montage(
    data: &Path,
    video_id: &str,
    ranges: &[(f64, f64)],
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(&str, f64, &str),
) -> Result<PreparedClip> {
    if ranges.len() < 2 { bail!("片段时间范围无效"); }
    let dir = clips_work_dir(data, video_id);
    tokio::fs::create_dir_all(&dir).await?;
    let key: String = ranges.iter().map(|(a, b)| format!("{}-{}", (a * 1000.0).round() as u64, (b * 1000.0).round() as u64)).collect::<Vec<_>>().join("_");
    let dest = dir.join(format!("montage-{:x}.mp4", { use std::hash::{Hash, Hasher}; let mut h = std::collections::hash_map::DefaultHasher::new(); key.hash(&mut h); h.finish() }));
    let total: f64 = ranges.iter().map(|(a, b)| b - a).sum();
    let ffmpeg = ensure_ffmpeg(data, &cancel, |_, _| {}).await?;
    if dest.is_file() && std::fs::metadata(&dest).map(|m| m.len() > 10_000).unwrap_or(false) {
        on_progress("done", 1.0, "已复用缓存片段");
        return Ok(finish_prepared(&ffmpeg, &dest, total, true).await);
    }
    let n = ranges.len() as f64;
    let mut paths = Vec::new();
    for (k, &(a, b)) in ranges.iter().enumerate() {
        let base = k as f64 / n;
        let part = prepare_segment(data, video_id, a, b, cancel.clone(), |stage, p, m| {
            if stage != "done" { on_progress("download", (base + p / n) * 0.85, m); }
        }).await?;
        paths.push((PathBuf::from(part.path), part.lead, part.duration));
    }
    on_progress("trim", 0.88, "正在拼接片段…");
    let tmp = dest.with_extension("part.mp4");
    let mut args: Vec<String> = vec!["-y".into()];
    // Each part from its own lead, for its own length.
    for (p, lead, d) in &paths { args.extend(["-ss".into(), format!("{lead:.3}"), "-t".into(), format!("{d:.3}"), "-i".into(), p.to_string_lossy().into_owned()]); }
    // Same size for every part (they come from one video), then one concat.
    let mut graph = String::new();
    for k in 0..paths.len() { graph.push_str(&format!("[{k}:v]setsar=1,format=yuv420p[v{k}];")); }
    for k in 0..paths.len() { graph.push_str(&format!("[v{k}][{k}:a]")); }
    graph.push_str(&format!("concat=n={}:v=1:a=1[v][a]", paths.len()));
    args.extend(["-filter_complex".into(), graph, "-map".into(), "[v]".into(), "-map".into(), "[a]".into()]);
    if has_videotoolbox(&ffmpeg) { args.extend(["-c:v".into(), "h264_videotoolbox".into(), "-b:v".into(), "12M".into()]); }
    else { args.extend(["-c:v".into(), "libx264".into(), "-preset".into(), "veryfast".into(), "-crf".into(), "18".into()]); }
    args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "192k".into(), "-movflags".into(), "+faststart".into(), tmp.to_string_lossy().into_owned()]);
    run_ffmpeg_with_progress(&ffmpeg, None, &args, total, &cancel, |p| on_progress("trim", 0.88 + p * 0.1, "正在拼接片段…")).await?;
    tokio::fs::rename(&tmp, &dest).await?;
    on_progress("done", 1.0, "片段已就绪");
    Ok(finish_prepared(&ffmpeg, &dest, total, false).await)
}

/// Fetches (or reuses cached) `[start, end]` raw H.264 clip for `video_id`.
pub async fn prepare_segment(
    data: &Path,
    video_id: &str,
    start: f64,
    end: f64,
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(&str, f64, &str),
) -> Result<PreparedClip> {
    if !(end > start) || end - start < 0.5 || end - start > 900.0 {
        bail!("片段时间范围无效");
    }
    let start = start.max(0.0);
    let dur = end - start;
    let start_ms = (start * 1000.0).round() as u64;
    let end_ms = (end * 1000.0).round() as u64;

    let dir = clips_work_dir(data, video_id);
    tokio::fs::create_dir_all(&dir).await?;
    let dest = dir.join(format!("{start_ms}-{end_ms}.mp4"));
    // One fetch per file at a time: a background prefetch and the user's own click on the same clip share it.
    let lock = {
        static LOCKS: once_cell::sync::Lazy<parking_lot::Mutex<std::collections::HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = once_cell::sync::Lazy::new(Default::default);
        LOCKS.lock().entry(dest.clone()).or_default().clone()
    };
    let _held = lock.lock().await;

    on_progress("ffmpeg", 0.05, "正在检查视频工具…");
    let ffmpeg = ensure_ffmpeg(data, &cancel, |got, total| {
        let pct = total.map(|t| (got as f64 / t.max(1) as f64) * 0.2).unwrap_or(0.08);
        on_progress("ffmpeg", pct, "正在准备视频导出组件（首次使用需要下载）…");
    }).await?;

    if dest.is_file() && std::fs::metadata(&dest).map(|m| m.len() > 10_000).unwrap_or(false) {
        on_progress("done", 1.0, "已复用缓存片段");
        return Ok(finish_prepared(&ffmpeg, &dest, dur, true).await);
    }

    if let Some((parent, cs, _ce)) = find_covering_cache(&dir, start_ms, end_ms) {
        let ss = (start_ms - cs) as f64 / 1000.0 + read_lead(&parent);
        on_progress("trim", 0.3, "正在从缓存片段截取…");
        normalize_mp4(&ffmpeg, &parent, &dest, Some((ss, dur)), dur, &cancel, |p| {
            on_progress("trim", 0.3 + p * 0.65, "正在整理视频画面…");
        }).await?;
        on_progress("done", 1.0, "片段已就绪");
        return Ok(finish_prepared(&ffmpeg, &dest, dur, true).await);
    }

    // Fast path: only this stretch's bytes, over parallel connections (fastclip.rs). Falls back to yt-dlp's own
    // section download when the format has no index or anything else goes wrong.
    on_progress("download", 0.1, "正在获取这一段原视频…");
    if let Ok(bin) = asr::ytdlp(data).await {
        let fast = crate::fastclip::fetch_section(data, &bin, &ffmpeg, video_id, start, end, &dest, cancel.clone(), |p, m| on_progress("download", 0.1 + p * 0.85, m)).await;
        match fast {
            Ok(()) => {
                on_progress("done", 1.0, "片段已就绪");
                return Ok(finish_prepared(&ffmpeg, &dest, dur, false).await);
            }
            Err(e) if cancel.load(Ordering::Relaxed) => return Err(e),
            Err(e) => eprintln!("fast section download failed, using yt-dlp: {e}"),
        }
    }
    on_progress("download", 0.15, "正在获取这一段原视频…");
    let raw = asr::with_ytdlp(data, |bin| {
        let (ff, vid, d, c) = (ffmpeg.clone(), video_id.to_string(), dir.clone(), cancel.clone());
        async move { download_youtube_section(&bin, &ff, &vid, start, end, &d, c).await }
    }).await?;

    on_progress("trim", 0.72, "正在整理片段封装…");
    let meta = probe_video_detail(&ffmpeg, &raw, dur).await;
    let norm = if meta.is_web_ready() {
        match fast_remux_mp4(&ffmpeg, &raw, &dest, &cancel).await {
            Ok(()) => Ok(()),
            Err(e) if cancel.load(Ordering::Relaxed) => Err(e),
            Err(_) => normalize_mp4(&ffmpeg, &raw, &dest, None, dur, &cancel, |p| {
                on_progress("trim", 0.72 + p * 0.23, "正在标准化片段画面…");
            }).await,
        }
    } else {
        normalize_mp4(&ffmpeg, &raw, &dest, None, dur, &cancel, |p| {
            on_progress("trim", 0.72 + p * 0.23, "正在标准化片段画面…");
        }).await
    };
    let _ = tokio::fs::remove_file(&raw).await;
    norm?;
    on_progress("done", 1.0, "片段已就绪");
    Ok(finish_prepared(&ffmpeg, &dest, dur, false).await)
}

/// Reads `(width, height)` from a PNG header (IHDR chunk).
pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[0..8] != b"\x89PNG\r\n\x1a\n" || &bytes[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    if w > 0 && h > 0 { Some((w, h)) } else { None }
}

/// Generates a minimal fully-transparent RGBA PNG of `(w, h)` for FFmpeg concat demuxer gaps.
fn make_transparent_png(w: u32, h: u32) -> Vec<u8> {
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    let row_len = w as usize * 4 + 1;
    let raw = vec![0u8; row_len * h as usize];
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
    let _ = enc.write_all(&raw);
    let idat = enc.finish().unwrap_or_default();

    fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(12 + data.len());
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(tag);
        out.extend_from_slice(data);
        let mut crc = 0xffff_ffffu32;
        for &byte in tag.iter().chain(data.iter()) {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
        out
    }

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&chunk(b"IHDR", &ihdr));
    png.extend_from_slice(&chunk(b"IDAT", &idat));
    png.extend_from_slice(&chunk(b"IEND", &[]));
    png
}

/// Builds an FFmpeg `concat` demuxer script that sequences all timed cue PNGs into a single O(1) overlay stream.
pub fn build_cues_concat_script(cues: &[(f64, f64)], total_dur: f64) -> String {
    let mut lines = vec!["ffconcat version 1.0".to_string()];
    let mut cursor = 0.0f64;
    for (idx, &(s, e)) in cues.iter().enumerate() {
        let start = s.max(cursor);
        if start - cursor > 0.015 {
            lines.push("file 'blank.png'".into());
            lines.push(format!("duration {:.3}", start - cursor));
        }
        let dur = (e - start).max(0.05);
        lines.push(format!("file 'c{idx}.png'"));
        lines.push(format!("duration {dur:.3}"));
        cursor = start + dur;
    }
    let tail = (total_dur - cursor).max(0.1);
    lines.push("file 'blank.png'".into());
    lines.push(format!("duration {tail:.3}"));
    // Concat demuxer requires the final file entry repeated without duration so the previous item keeps its full duration.
    lines.push("file 'blank.png'".into());
    lines.join("\n")
}

/// Builds the FFmpeg `-filter_complex` graph.
/// When `cue_stream_y` is `Some(y)`, all cues come from one concat demuxer stream (O(1) overlay).
pub fn build_filter_graph(
    layout: &str,
    card_bg: &str,
    crop_x: f64,
    frame: Option<&Frame>,
    has_static: bool,
    cues: &[(f64, f64, i32)],
    cue_stream_y: Option<i32>,
    cover_mode: &str,
    cover_input_idx: Option<usize>,
    has_audio: bool,
    pan: Option<&str>,
) -> String {
    // The crop's left edge for an output `ow` wide: a fixed share of the slack (`crop_x`, 0 = left, 1 = right),
    // or (smart framing) centred on the speaker's place in `t`, kept inside the picture. Quoted for its commas.
    let cx = crop_x.clamp(0.0, 1.0);
    let crop_left = |ow: u32| match pan {
        Some(e) => format!("'clip(({e})*iw-{ow}/2,0,iw-{ow})'"),
        None => format!("(iw-{ow})*{cx:.4}"),
    };
    let mut parts: Vec<String> = Vec::new();
    let even = |v: u32| (v / 2 * 2).max(2);

    if let Some(f) = frame {
        let (w, h) = (even(f.w), even(f.h));
        if f.fit == "card" && card_bg != "blur" {
            // A plain colour around the picture: pad it (no full-frame drawing per frame, ~40% faster).
            let hex = match card_bg { "paper" => "#f5f2eb", "white" => "#ffffff", _ => "#141315" };
            let (vw, vh) = (even(f.vw.max(2)), even(f.vh.max(2)));
            let fit_box = if f.crop_box {
                format!("scale={vw}:{vh}:force_original_aspect_ratio=increase,crop={vw}:{vh}:{}:(ih-{vh})/2", crop_left(vw))
            } else {
                format!("scale={vw}:{vh}:force_original_aspect_ratio=decrease,pad={vw}:{vh}:(ow-iw)/2:(oh-ih)/2:color={hex}")
            };
            parts.push(format!("[0:v]{fit_box},setsar=1,pad={w}:{h}:{vx}:{vy}:color={hex}[base]", vx = f.vx, vy = f.vy));
        } else if f.fit == "card" {
            // Background blurred at 1/8 size with fast bilinear scaling (~4x cheaper than 1/4, ~64x cheaper than full frame).
            let bg = match card_bg {
                "blur" => format!(
                    "[bg]scale={qw}:{qh}:force_original_aspect_ratio=increase:flags=fast_bilinear,crop={qw}:{qh},boxblur=4:2,eq=brightness=-0.22:saturation=0.85,scale={w}:{h}:flags=bilinear,setsar=1[bg2]",
                    qw = even((w / 8).max(64)), qh = even((h / 8).max(64))
                ),
                other => {
                    let hex = match other { "paper" => "#f5f2eb", "white" => "#ffffff", _ => "#141315" };
                    format!("[bg]scale={w}:{h},drawbox=x=0:y=0:w=iw:h=ih:color={hex}:t=fill,setsar=1[bg2]")
                }
            };
            let (vw, vh) = (even(f.vw.max(2)), even(f.vh.max(2)));
            let fg = if f.crop_box {
                format!("[fg]scale={vw}:{vh}:force_original_aspect_ratio=increase,crop={vw}:{vh}:{}:(ih-{vh})/2,setsar=1[fg2]", crop_left(vw))
            } else {
                format!("[fg]scale={vw}:{vh}:force_original_aspect_ratio=decrease,setsar=1[fg2]")
            };
            parts.push(format!(
                "[0:v]split[fg][bg];{bg};{fg};[bg2][fg2]overlay={vx}+({vw}-w)/2:{vy}+({vh}-h)/2[base]",
                vx = f.vx, vy = f.vy
            ));
        } else {
            parts.push(format!(
                "[0:v]scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}:{}:(ih-{h})/2,setsar=1[base]", crop_left(w)
            ));
        }
    } else {
    match layout {
        "portrait_card" => {
            if card_bg == "blur" {
                parts.push(
                    "[0:v]split[fg][bg];\
                     [bg]scale=136:240:force_original_aspect_ratio=increase:flags=fast_bilinear,crop=136:240,boxblur=4:2,eq=brightness=-0.22:saturation=0.85,scale=1080:1920:flags=bilinear,setsar=1[bg2];\
                     [fg]scale=1080:640:force_original_aspect_ratio=decrease,setsar=1[fg2];\
                     [bg2][fg2]overlay=(W-w)/2:560+(640-h)/2[base]".into()
                );
            } else {
                let hex = if card_bg == "paper" { "#f5f2eb" } else { "#141315" };
                parts.push(format!(
                    "[0:v]scale=1032:620:force_original_aspect_ratio=decrease,pad=1080:1920:(ow-iw)/2:568+(620-ih)/2:color={hex},setsar=1[base]"
                ));
            }
        }
        "portrait_fill" => {
            parts.push(format!(
                "[0:v]scale=1080:1920:force_original_aspect_ratio=increase,crop=1080:1920:{}:(ih-1920)/2,setsar=1[base]", crop_left(1080)
            ));
        }
        _ => {
            parts.push(
                "[0:v]scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2:black,setsar=1[base]".into()
            );
        }
    }
    }

    let mut cur = "base".to_string();
    let mut next_input = 1usize;

    if has_static {
        parts.push(format!("[{cur}][{next_input}:v]overlay=0:0:eof_action=repeat:format=auto[v0]"));
        cur = "v0".into();
        next_input += 1;
    }

    if let Some(y) = cue_stream_y {
        parts.push(format!(
            "[{cur}][{next_input}:v]overlay=0:{y}:eof_action=pass:format=auto[vc]"
        ));
        cur = "vc".into();
    } else {
        for (idx, &(s, e, y)) in cues.iter().enumerate() {
            let out_label = format!("vc{idx}");
            let start = s.max(0.0);
            let end = e.max(start + 0.05);
            parts.push(format!(
                "[{cur}][{next_input}:v]overlay=0:{y}:enable='between(t,{start:.3},{end:.3})':eof_action=repeat:format=auto[{out_label}]"
            ));
            cur = out_label;
            next_input += 1;
        }
    }

    match (cover_input_idx, cover_mode) {
        (Some(cidx), "flash") => {
            parts.push(format!(
                "[{cur}][{cidx}:v]overlay=0:0:enable='lt(t,0.140)':eof_action=repeat:format=auto,format=yuv420p[vout]"
            ));
        }
        (Some(cidx), "hold") => {
            parts.push(format!(
                "[{cur}]tpad=start_duration=1.000:start_mode=clone[vtpad];\
                 [vtpad][{cidx}:v]overlay=0:0:enable='lt(t,1.000)':eof_action=repeat:format=auto,format=yuv420p[vout]"
            ));
            if has_audio {
                parts.push("[0:a]adelay=1000:all=1[aout]".into());
            }
        }
        _ => {
            parts.push(format!("[{cur}]format=yuv420p[vout]"));
        }
    }

    parts.join(";")
}

/// Builds the FFmpeg `-filter_complex` graph for layout framing + static overlay + timed cue PNGs + optional cover intro.
pub fn build_filter_script(
    layout: &str,
    card_bg: &str,
    crop_x: f64,
    frame: Option<&Frame>,
    has_static: bool,
    cues: &[(f64, f64, i32)],
    cover_mode: &str,
    cover_input_idx: Option<usize>,
    has_audio: bool,
) -> String {
    build_filter_graph(layout, card_bg, crop_x, frame, has_static, cues, None, cover_mode, cover_input_idx, has_audio, None)
}

/// Plays the source at `speed`: picture timestamps divided, audio through atempo; the rest of the graph
/// reads the sped-up streams.
pub fn with_speed(filter: &str, speed: f64, has_audio: bool) -> String {
    if (speed - 1.0).abs() < 0.001 { return filter.to_string(); }
    let mut f = format!("[0:v]setpts=PTS/{speed:.4}[sv];") + &filter.replace("[0:v]", "[sv]");
    if has_audio {
        if f.contains("[0:a]") { f = format!("[0:a]atempo={speed:.4}[sa];") + &f.replace("[0:a]", "[sa]"); }
        else { f.push_str(&format!(";[0:a]atempo={speed:.4}[aout]")); }
    }
    f
}

/// Integrated loudness the platforms play at (YouTube, TikTok/Douyin, Bilibili all sit around −14 LUFS).
pub const TARGET_LUFS: f64 = -14.0;

/// Ends the sound chain with a fixed gain to the target loudness and a limiter so the peaks stay under
/// −1.5 dBTP; whatever left as `[aout]` (or the untouched `[0:a]`) goes through it. Last, so speed and cuts are
/// already in. A measured gain plus a limiter is steadier than loudnorm's single pass, which pumps on speech
/// and fell 2 LU short on a real clip.
pub fn with_loudness(filter: &str, has_audio: bool, gain_db: f64) -> String {
    if !has_audio { return filter.to_string(); }
    let tail = format!("volume={gain_db:.2}dB,alimiter=limit=0.84:level=disabled,aresample=48000[aout]");
    if filter.contains("[aout]") { format!("{};[apre]{tail}", filter.replace("[aout]", "[apre]")) }
    else { format!("{filter};[0:a]{tail}") }
}

/// Integrated loudness (LUFS) of `path` from `lead` for `duration` s; `None` for silence or a failed read.
/// EBU R128 gating leaves pauses out, so cutting them later barely moves it.
async fn measure_lufs(ffmpeg: &Path, path: &Path, lead: f64, duration: f64) -> Option<f64> {
    let out = tokio::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-nostats", "-ss", &format!("{:.3}", lead.max(0.0)), "-t", &format!("{duration:.3}"), "-i"])
        .arg(path)
        .args(["-vn", "-af", "ebur128", "-f", "null", "-"])
        .output().await.ok()?;
    parse_lufs(&String::from_utf8_lossy(&out.stderr))
}

fn parse_lufs(log: &str) -> Option<f64> {
    let v: f64 = log.rsplit("I:").next()?.split_whitespace().next()?.parse().ok()?;
    (v.is_finite() && v > -60.0).then_some(v)
}

/// The kept stretches, sorted, clamped to `0..duration`, overlaps merged; empty when they keep it all.
pub fn normalize_keep(keep: &[(f64, f64)], duration: f64) -> Vec<(f64, f64)> {
    let mut v: Vec<(f64, f64)> = keep.iter()
        .map(|&(a, b)| (a.max(0.0), b.min(duration)))
        .filter(|&(a, b)| b - a > 0.04)
        .collect();
    v.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a <= last.1 + 0.01 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    if out.len() == 1 && out[0].0 < 0.01 && out[0].1 > duration - 0.01 { return Vec::new(); }
    out
}

/// Cuts the source to `keep` before the rest of the graph: each stretch trimmed (picture and sound) and
/// joined, the joined streams read where the graph read `[0:v]`/`[0:a]`. Run after `with_speed`, so the
/// speed change applies to the cut. Sound always leaves through `[aout]` (it can't be stream-copied).
pub fn with_keep(filter: &str, keep: &[(f64, f64)], has_audio: bool) -> String {
    if keep.is_empty() { return filter.to_string(); }
    let n = keep.len();
    let mut pre: Vec<String> = Vec::new();
    let vs: String = (0..n).map(|i| format!("[kv{i}i]")).collect();
    pre.push(format!("[0:v]split={n}{vs}"));
    if has_audio {
        let as_: String = (0..n).map(|i| format!("[ka{i}i]")).collect();
        pre.push(format!("[0:a]asplit={n}{as_}"));
    }
    let mut joined = String::new();
    for (i, &(a, b)) in keep.iter().enumerate() {
        pre.push(format!("[kv{i}i]trim=start={a:.3}:end={b:.3},setpts=PTS-STARTPTS[kv{i}]"));
        joined.push_str(&format!("[kv{i}]"));
        if has_audio {
            pre.push(format!("[ka{i}i]atrim=start={a:.3}:end={b:.3},asetpts=PTS-STARTPTS[ka{i}]"));
            joined.push_str(&format!("[ka{i}]"));
        }
    }
    let a = if has_audio { 1 } else { 0 };
    let outs = if has_audio { "[kv][ka]" } else { "[kv]" };
    pre.push(format!("{joined}concat=n={n}:v=1:a={a}{outs}"));
    let mut rest = filter.replace("[0:v]", "[kv]");
    if has_audio {
        if rest.contains("[0:a]") { rest = rest.replace("[0:a]", "[ka]"); }
        else { rest.push_str(";[ka]anull[aout]"); }
    }
    format!("{};{rest}", pre.join(";"))
}

fn decode_png_base64(raw: &str) -> Result<Vec<u8>> {
    let trimmed = raw.trim();
    let b64 = trimmed.strip_prefix("data:image/png;base64,").unwrap_or(trimmed);
    base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| anyhow!("PNG 解码失败：{e}")) // i18n-ignore
}

/// A free `<name>.mp4` in `dir` (the folder set in Settings), or else the Downloads folder.
fn default_mp4_path(name: &str, dir: Option<&str>) -> Result<PathBuf> {
    let dir = match dir.map(str::trim).filter(|d| !d.is_empty()).map(PathBuf::from).filter(|d| d.is_dir()) {
        Some(d) => d,
        None => dirs::download_dir().or_else(dirs::desktop_dir).ok_or_else(|| anyhow!("找不到下载文件夹"))?,
    };
    let name = name.trim();
    let name = name.strip_suffix(".mp4").unwrap_or(name); // callers may pass a full file name
    let safe: String = name.chars().filter(|c| !"/\\:*?\"<>|\0".contains(*c)).take(80).collect();
    let safe = safe.trim();
    let base = if safe.is_empty() { "SubCast 切片" } else { safe }; // i18n-ignore
    let mut path = dir.join(format!("{base}.mp4"));
    let mut i = 2usize;
    while path.exists() {
        path = dir.join(format!("{base} {i}.mp4"));
        i += 1;
    }
    Ok(path)
}

fn sidecar_png_path(mp4_path: &Path, suffix: &str) -> PathBuf {
    let parent = mp4_path.parent().unwrap_or_else(|| Path::new("."));
    let stem = mp4_path.file_stem().and_then(|s| s.to_str()).unwrap_or("clip");
    parent.join(format!("{stem}{suffix}.png"))
}

/// Composites layout framing, static overlay PNG, timed subtitle cue PNGs, cover intro/thumbnail, and sidecar PNGs.
pub async fn render_clip(
    data: &Path,
    req: RenderRequest,
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(&str, f64, &str),
) -> Result<RenderResult> {
    let raw_path = PathBuf::from(&req.raw_path);
    if !raw_path.is_file() { bail!("找不到源视频片段，请先重新获取"); }

    on_progress("ffmpeg", 0.04, "正在准备视频导出组件…");
    let ffmpeg = ensure_ffmpeg(data, &cancel, |got, total| {
        let pct = total.map(|t| (got as f64 / t.max(1) as f64) * 0.15).unwrap_or(0.06);
        on_progress("ffmpeg", pct, "正在准备视频导出组件（首次使用需要下载）…");
    }).await?;

    let final_out = match req.output_path.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => default_mp4_path(&req.default_name, req.output_dir.as_deref())?,
    };
    if let Some(parent) = final_out.parent() { tokio::fs::create_dir_all(parent).await?; }

    let cover_bytes = req.cover_png.as_deref().filter(|s| !s.is_empty()).map(decode_png_base64).transpose()?;
    let landscape_cover_bytes = req.landscape_cover_png.as_deref().filter(|s| !s.is_empty()).map(decode_png_base64).transpose()?;

    let work_dir = data.join("work").join(format!("render-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir_all(&work_dir).await?;

    let res = render_into(&ffmpeg, &raw_path, &work_dir, &final_out, &req, cover_bytes.as_deref(), &cancel, &mut on_progress).await;
    let _ = tokio::fs::remove_dir_all(&work_dir).await;
    res?;

    let mut cover_path = None;
    if req.save_cover_sidecar {
        if let Some(bytes) = cover_bytes.as_deref() {
            let cp = sidecar_png_path(&final_out, "-封面"); // i18n-ignore (filename suffix)
            tokio::fs::write(&cp, bytes).await?;
            cover_path = Some(cp.to_string_lossy().into_owned());
        }
    }
    let mut landscape_cover_path = None;
    if let Some(bytes) = landscape_cover_bytes.as_deref() {
        let lp = sidecar_png_path(&final_out, "-横版封面"); // i18n-ignore (filename suffix)
        tokio::fs::write(&lp, bytes).await?;
        landscape_cover_path = Some(lp.to_string_lossy().into_owned());
    }

    on_progress("done", 1.0, "导出完成");
    Ok(RenderResult {
        video_path: final_out.to_string_lossy().into_owned(),
        cover_path,
        landscape_cover_path,
    })
}

enum ExtraInput {
    File(String),
    Concat(String),
}

async fn render_into(
    ffmpeg: &Path,
    raw_path: &Path,
    work_dir: &Path,
    final_out: &Path,
    req: &RenderRequest,
    cover_bytes: Option<&[u8]>,
    cancel: &AtomicBool,
    on_progress: &mut impl FnMut(&str, f64, &str),
) -> Result<()> {
    let mut extra_inputs: Vec<ExtraInput> = Vec::new();
    let has_static = if let Some(png) = req.static_overlay_png.as_deref().filter(|s| !s.is_empty()) {
        tokio::fs::write(work_dir.join("s.png"), decode_png_base64(png)?).await?;
        extra_inputs.push(ExtraInput::File("s.png".into()));
        true
    } else {
        false
    };

    // Decode & write all cue PNGs concurrently, recording dimensions to check if O(1) concat stream applies.
    let mut write_set = tokio::task::JoinSet::new();
    for (idx, cue) in req.cues.iter().enumerate() {
        let dir = work_dir.to_path_buf();
        let b64 = cue.png_base64.clone();
        write_set.spawn(async move {
            let bytes = decode_png_base64(&b64)?;
            let dims = png_dimensions(&bytes);
            let name = format!("c{idx}.png");
            tokio::fs::write(dir.join(&name), &bytes).await?;
            Ok::<(usize, Option<(u32, u32)>), anyhow::Error>((idx, dims))
        });
    }
    let mut cue_dims: Vec<Option<(u32, u32)>> = vec![None; req.cues.len()];
    while let Some(res) = write_set.join_next().await {
        let (idx, dims) = res.context("字幕图层写入任务异常")??;
        cue_dims[idx] = dims;
    }

    let speed = if req.speed > 0.0 { req.speed.clamp(0.5, 2.0) } else { 1.0 };
    let keep = normalize_keep(&req.keep_ranges, req.duration);
    // The clip's length once cut (before speed).
    let kept_dur = if keep.is_empty() { req.duration } else { keep.iter().map(|(a, b)| b - a).sum() };
    let cue_meta: Vec<(f64, f64, i32)> = req.cues.iter().map(|c| (c.start, c.end, c.y)).collect();
    let uniform_concat = if req.cues.len() >= 2 {
        let first_y = req.cues[0].y;
        if let Some((w, h)) = cue_dims[0] {
            if req.cues.iter().all(|c| c.y == first_y) && cue_dims.iter().all(|d| *d == Some((w, h))) {
                Some((first_y, w, h))
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    let cue_stream_y = if let Some((y, w, h)) = uniform_concat {
        let blank = make_transparent_png(w, h);
        let times: Vec<(f64, f64)> = req.cues.iter().map(|c| (c.start, c.end)).collect();
        let script = build_cues_concat_script(&times, kept_dur.max(1.0) / speed);
        tokio::try_join!(
            tokio::fs::write(work_dir.join("blank.png"), &blank),
            tokio::fs::write(work_dir.join("cues.txt"), script.as_bytes()),
        )?;
        extra_inputs.push(ExtraInput::Concat("cues.txt".into()));
        Some(y)
    } else {
        for idx in 0..req.cues.len() {
            extra_inputs.push(ExtraInput::File(format!("c{idx}.png")));
        }
        None
    };

    let cover_mode = req.cover_mode.as_str();
    let need_cover_input = cover_bytes.is_some() && (matches!(cover_mode, "flash" | "hold") || req.embed_thumbnail);
    let cover_input_idx = if need_cover_input {
        tokio::fs::write(work_dir.join("cover.png"), cover_bytes.unwrap()).await?;
        extra_inputs.push(ExtraInput::File("cover.png".into()));
        Some(extra_inputs.len())
    } else {
        None
    };

    let meta = probe_video_detail(ffmpeg, raw_path, req.duration).await;
    let has_audio = meta.has_audio;
    // The crop reads time after the speed change and the cuts, so the track goes onto that clock first.
    let pan_points: Vec<(f64, f64)> = req.pan.iter().map(|&(t, x)| (to_output_time(t, &keep, speed), x)).collect();
    let pan = pan_expr(&pan_points);
    let filter = build_filter_graph(
        &req.layout,
        &req.card_bg,
        req.crop_x,
        req.frame.as_ref(),
        has_static,
        &cue_meta,
        cue_stream_y,
        cover_mode,
        cover_input_idx,
        has_audio,
        pan.as_deref(),
    );
    let tmp_out = work_dir.join("out.mp4");
    let use_vt = has_videotoolbox(ffmpeg);
    let filter = with_speed(&filter, speed, has_audio);
    let filter = with_keep(&filter, &keep, has_audio);
    let gain = if req.loudness && has_audio {
        measure_lufs(ffmpeg, raw_path, req.lead, req.duration).await.map(|l| (TARGET_LUFS - l).clamp(-20.0, 20.0))
    } else { None };
    let filter = match gain { Some(g) => with_loudness(&filter, has_audio, g), None => filter };
    let audio_map = if has_audio && filter.contains("[aout]") { "[aout]" } else { "0:a?" };
    let can_copy_audio = audio_map == "0:a?" && meta.audio_codec.as_deref() == Some("aac");
    let dur = kept_dur.max(1.0) / speed + if cover_input_idx.is_some() && cover_mode == "hold" { 1.0 } else { 0.0 };

    on_progress("render", 0.12, "正在合成视频…");

    for attempt in [use_vt, false] {
        // The clip proper (after the stream copy's lead), exactly.
        let mut args: Vec<String> = vec!["-y".into(), "-ss".into(), format!("{:.3}", req.lead.max(0.0)), "-t".into(), format!("{:.3}", req.duration), "-i".into(), raw_path.to_string_lossy().into_owned()];
        for inp in &extra_inputs {
            match inp {
                ExtraInput::File(name) => {
                    args.push("-i".into());
                    args.push(name.clone());
                }
                ExtraInput::Concat(list) => {
                    args.extend(["-f".into(), "concat".into(), "-safe".into(), "0".into(), "-i".into(), list.clone()]);
                }
            }
        }
        args.extend([
            "-filter_complex".into(), filter.clone(),
            "-map".into(), "[vout]".into(),
            "-map".into(), audio_map.into(),
        ]);
        if attempt {
            args.extend(["-c:v:0".into(), "h264_videotoolbox".into(), "-b:v:0".into(), "10M".into(), "-maxrate:v:0".into(), "14M".into(), "-bufsize:v:0".into(), "20M".into()]);
        } else {
            args.extend(["-c:v:0".into(), "libx264".into(), "-preset".into(), "superfast".into(), "-crf".into(), "19".into()]);
        }
        if let Some(cidx) = cover_input_idx.filter(|_| req.embed_thumbnail) {
            args.extend([
                "-map".into(), format!("{cidx}:v"),
                "-c:v:1".into(), "mjpeg".into(),
                "-q:v:1".into(), "3".into(),
                "-disposition:v:1".into(), "attached_pic".into(),
            ]);
        }
        if can_copy_audio {
            args.extend(["-c:a".into(), "copy".into()]);
        } else {
            args.extend(["-c:a".into(), "aac".into(), "-b:a".into(), "192k".into()]);
        }
        args.extend([
            "-movflags".into(), "+faststart".into(),
            tmp_out.to_string_lossy().into_owned(),
        ]);

        match run_ffmpeg_with_progress(ffmpeg, Some(work_dir), &args, dur, cancel, |p| {
            on_progress("render", 0.12 + p * 0.85, "正在合成视频…");
        }).await {
            Ok(()) => {
                // Rename across directories falls back to copy + remove if final_out is on another volume.
                if tokio::fs::rename(&tmp_out, final_out).await.is_err() {
                    tokio::fs::copy(&tmp_out, final_out).await?;
                }
                return Ok(());
            }
            Err(e) if cancel.load(Ordering::Relaxed) || !attempt => {
                let _ = tokio::fs::remove_file(&tmp_out).await;
                return Err(e);
            }
            Err(_) => {
                let _ = tokio::fs::remove_file(&tmp_out).await;
            }
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_script_variants() {
        let l = build_filter_script("landscape", "blur", 0.5, None, true, &[(0.5, 2.0, 820)], "flash", Some(3), true);
        assert!(l.contains("scale=1920:1080"));
        assert!(l.contains("[base][1:v]overlay=0:0:eof_action=repeat:format=auto[v0]"));
        assert!(l.contains("[v0][2:v]overlay=0:820:enable='between(t,0.500,2.000)'"));
        assert!(l.contains("[3:v]overlay=0:0:enable='lt(t,0.140)'"));
        assert!(l.ends_with("format=yuv420p[vout]"));

        let c_blur = build_filter_script("portrait_card", "blur", 0.5, None, false, &[(0.0, 1.5, 1300)], "hold", Some(2), true);
        assert!(c_blur.contains("boxblur=4:2"));
        assert!(c_blur.contains("[base][1:v]overlay=0:1300"));
        assert!(c_blur.contains("tpad=start_duration=1.000:start_mode=clone"));
        assert!(c_blur.contains("[0:a]adelay=1000:all=1[aout]"));

        let c_concat = build_filter_graph("portrait_card", "blur", 0.5, None, true, &[], Some(1236), "flash", Some(3), true, None);
        assert!(c_concat.contains("[v0][2:v]overlay=0:1236:eof_action=pass:format=auto[vc]"));
        assert!(c_concat.contains("[vc][3:v]overlay=0:0:enable='lt(t,0.140)'"));

        let c_hold_silent = build_filter_script("portrait_card", "night", 0.5, None, true, &[], "hold", Some(2), false);
        assert!(c_hold_silent.contains("pad=1080:1920"));
        assert!(c_hold_silent.contains("color=#141315"));
        assert!(c_hold_silent.contains("tpad=start_duration=1.000"));
        assert!(!c_hold_silent.contains("adelay="));

        let c_paper = build_filter_script("portrait_card", "paper", 0.5, None, false, &[], "none", None, false);
        assert!(c_paper.contains("pad=1080:1920"));
        assert!(c_paper.contains("color=#f5f2eb"));

        let fill = build_filter_script("portrait_fill", "blur", 0.25, None, false, &[], "none", None, true);
        let card = Frame { w: 1080, h: 1440, fit: "card".into(), vx: 0, vy: 380, vw: 1080, vh: 608, crop_box: false };
        let g = build_filter_script("x", "blur", 0.5, Some(&card), false, &[], "none", None, true);
        assert!(g.contains("scale=134:180") && g.contains("scale=1080:1440:flags=bilinear") && g.contains("overlay=0+(1080-w)/2:380+(608-h)/2[base]"), "{g}");
        let sq = Frame { w: 1080, h: 1080, fit: "fill".into(), vx: 0, vy: 0, vw: 0, vh: 0, crop_box: false };
        let zoomed = Frame { w: 1080, h: 1920, fit: "card".into(), vx: 0, vy: 560, vw: 1080, vh: 810, crop_box: true };
        assert!(build_filter_script("x", "night", 0.7, Some(&zoomed), false, &[], "none", None, true).contains("crop=1080:810:(iw-1080)*0.7000"));
        assert!(build_filter_script("x", "blur", 0.3, Some(&sq), false, &[], "none", None, true).contains("crop=1080:1080:(iw-1080)*0.3000"));
        assert!(fill.contains("crop=1080:1920:(iw-1080)*0.2500:(ih-1920)/2"));
    }

    #[test]
    fn parse_meta_and_time() {
        let stderr = "\
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'clip.mp4':
  Duration: 00:01:23.45, start: 0.000000, bitrate: 1543 kb/s
  Stream #0:0[0x1](und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, bt709), 1280x720 [SAR 1:1 DAR 16:9], 30 fps
  Stream #0:1[0x2](und): Audio: aac (LC), 48000 Hz, stereo, fltp
";
        let (d, w, h, has_audio) = parse_video_meta(stderr);
        assert!((d - 83.45).abs() < 0.01);
        assert_eq!((w, h), (1280, 720));
        assert!(has_audio);
        assert!((parse_ffmpeg_time("frame=  120 fps= 60 q=-0.0 size= 256kB time=00:00:04.25 bitrate= 493.2kbits/s").unwrap() - 4.25).abs() < 0.001);
    }

    #[test]
    fn burned_sub_detector() {
        let w = 100usize;
        let h = 40usize;
        // Static corner watermark at same coordinates in all frames -> overlap = 1.0 -> false
        let mut static_frame = vec![100u8; w * h];
        for y in 5..12 {
            for x in 20..30 {
                static_frame[y * w + x] = 240;
            }
            static_frame[y * w + 19] = 10;
        }
        assert!(!decide_burned_subtitles(&[&static_frame, &static_frame, &static_frame], w, h));

        // Shifting bright outlined glyphs across frames -> overlap < 0.5 -> true
        let make_sub = |x_off: usize| -> Vec<u8> {
            let mut f = vec![110u8; w * h];
            for y in 24..34 {
                for x in x_off..x_off + 20 {
                    f[y * w + x] = 245;
                }
                f[y * w + x_off - 1] = 15;
            }
            f
        };
        let f1 = make_sub(18);
        let f2 = make_sub(42);
        let f3 = make_sub(62);
        assert!(decide_burned_subtitles(&[&f1, &f2, &f3], w, h));
    }

    #[test]
    fn covering_cache_picks_tightest() {
        let d = std::env::temp_dir().join(format!("subcast-cache-match-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("10000-90000.mp4"), vec![0u8; 20_000]).unwrap();
        std::fs::write(d.join("20000-50000.mp4"), vec![0u8; 20_000]).unwrap();
        let hit = find_covering_cache(&d, 22000, 45000).unwrap();
        assert_eq!((hit.1, hit.2), (20000, 50000));
        assert!(find_covering_cache(&d, 5000, 45000).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn minimal_rgba_png_b64(w: u32, h: u32, r: u8, g: u8, b: u8, a: u8) -> String {
        use flate2::{write::ZlibEncoder, Compression};
        use std::io::Write;
        let mut raw = Vec::with_capacity((w as usize * 4 + 1) * h as usize);
        for _ in 0..h {
            raw.push(0);
            for _ in 0..w { raw.extend_from_slice(&[r, g, b, a]); }
        }
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&raw).unwrap();
        let idat = enc.finish().unwrap();
        fn chunk(tag: &[u8; 4], data: &[u8]) -> Vec<u8> {
            let mut out = Vec::with_capacity(12 + data.len());
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(tag);
            out.extend_from_slice(data);
            // Simple CRC32
            let mut crc = 0xffff_ffffu32;
            for &byte in tag.iter().chain(data.iter()) {
                crc ^= byte as u32;
                for _ in 0..8 { crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 }; }
            }
            out.extend_from_slice(&(!crc).to_be_bytes());
            out
        }
        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&w.to_be_bytes());
        ihdr.extend_from_slice(&h.to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&chunk(b"IHDR", &ihdr));
        png.extend_from_slice(&chunk(b"IDAT", &idat));
        png.extend_from_slice(&chunk(b"IEND", &[]));
        base64::engine::general_purpose::STANDARD.encode(png)
    }

    #[tokio::test]
    async fn real_ffmpeg_render_smoke() {
        let data = std::env::temp_dir().join(format!("subcast-vclip-smoke-{}", uuid::Uuid::new_v4()));
        let Some(ffmpeg) = find_ffmpeg(&data) else { return };
        std::fs::create_dir_all(&data).unwrap();
        let src = data.join("src.mp4");
        let out = data.join("out.mp4");

        let mk = std::process::Command::new(&ffmpeg)
            .args([
                "-y", "-f", "lavfi", "-i", "testsrc=size=640x360:rate=30:duration=2",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=2",
                "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac",
            ])
            .arg(&src)
            .output()
            .unwrap();
        assert!(mk.status.success(), "generate test source failed");

        let req_flash = RenderRequest { frame: None, output_dir: None, lead: 0.0, speed: 1.0,
            raw_path: src.to_string_lossy().into_owned(),
            output_path: Some(out.to_string_lossy().into_owned()),
            default_name: "smoke".into(),
            layout: "portrait_card".into(),
            card_bg: "night".into(),
            crop_x: 0.5,
            duration: 2.0,
            static_overlay_png: Some(minimal_rgba_png_b64(1080, 1920, 0, 0, 0, 0)),
            cues: vec![CueOverlay {
                start: 0.2,
                end: 1.5,
                y: 1320,
                png_base64: minimal_rgba_png_b64(1080, 160, 255, 220, 80, 180),
            }],
            cover_png: Some(minimal_rgba_png_b64(1080, 1920, 24, 24, 28, 255)),
            cover_mode: "flash".into(),
            embed_thumbnail: true,
            save_cover_sidecar: true,
            landscape_cover_png: Some(minimal_rgba_png_b64(1920, 1080, 24, 24, 28, 255)), keep_ranges: vec![], loudness: false, pan: vec![],
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let res = render_clip(&data, req_flash, cancel.clone(), |_, _, _| {}).await.unwrap();
        let size = std::fs::metadata(&res.video_path).unwrap().len();
        assert!(size > 10_000, "rendered flash mp4 too small: {size}");
        assert!(std::fs::metadata(res.cover_path.as_ref().unwrap()).unwrap().len() > 100);
        assert!(std::fs::metadata(res.landscape_cover_path.as_ref().unwrap()).unwrap().len() > 100);

        let (frames, best) = extract_candidate_frames(&ffmpeg, &src, 2.0).await;
        assert!(!frames.is_empty());
        assert!(best < frames.len());

        let out_hold = data.join("out-hold.mp4");
        let req_hold = RenderRequest { frame: None, output_dir: None, lead: 0.0, speed: 1.0,
            raw_path: src.to_string_lossy().into_owned(),
            output_path: Some(out_hold.to_string_lossy().into_owned()),
            default_name: "smoke-hold".into(),
            layout: "portrait_card".into(),
            card_bg: "paper".into(),
            crop_x: 0.5,
            duration: 2.0,
            static_overlay_png: None,
            cues: vec![],
            cover_png: Some(minimal_rgba_png_b64(1080, 1920, 245, 240, 232, 255)),
            cover_mode: "hold".into(),
            embed_thumbnail: true,
            save_cover_sidecar: false,
            landscape_cover_png: None, keep_ranges: vec![], loudness: false, pan: vec![],
        };
        let res_hold = render_clip(&data, req_hold, cancel.clone(), |_, _, _| {}).await.unwrap();
        assert!(std::fs::metadata(&res_hold.video_path).unwrap().len() > 10_000);

        // Multi-cue O(1) concat demuxer stream + downscaled blur smoke test (12 uniform cues)
        let out_concat = data.join("out-concat.mp4");
        let cue_png = minimal_rgba_png_b64(1080, 200, 255, 220, 80, 180);
        let many_cues: Vec<CueOverlay> = (0..12)
            .map(|i| CueOverlay {
                start: i as f64 * 0.15,
                end: i as f64 * 0.15 + 0.12,
                y: 1236,
                png_base64: cue_png.clone(),
            })
            .collect();
        let req_concat = RenderRequest {
            frame: Some(Frame { w: 1080, h: 1920, fit: "card".into(), vx: 0, vy: 556, vw: 1080, vh: 810, crop_box: true }),
            output_dir: None,
            lead: 0.0,
            speed: 1.0,
            raw_path: src.to_string_lossy().into_owned(),
            output_path: Some(out_concat.to_string_lossy().into_owned()),
            default_name: "smoke-concat".into(),
            layout: "frame".into(),
            card_bg: "blur".into(),
            crop_x: 0.5,
            duration: 2.0,
            static_overlay_png: Some(minimal_rgba_png_b64(1080, 1920, 0, 0, 0, 0)),
            cues: many_cues,
            cover_png: Some(minimal_rgba_png_b64(1080, 1920, 24, 24, 28, 255)),
            cover_mode: "flash".into(),
            embed_thumbnail: true,
            save_cover_sidecar: false,
            landscape_cover_png: None, keep_ranges: vec![], loudness: false, pan: vec![],
        };
        let res_concat = render_clip(&data, req_concat, cancel, |_, _, _| {}).await.unwrap();
        assert!(std::fs::metadata(&res_concat.video_path).unwrap().len() > 10_000);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn silences_parse_and_move_to_clip_time() {
        let err = "[silencedetect @ 0x1] silence_start: 1.2\n[silencedetect @ 0x1] silence_end: 2.0 | silence_duration: 0.8\n[silencedetect @ 0x1] silence_start: 9.5\n";
        let s = parse_silences(err, 10.0);
        assert_eq!(s, vec![(1.2, 2.0), (9.5, 10.0)]);
        // lead 1.0, clip 8.7 s long: the first pause moves to 0.2–1.0, the second is clamped to the clip's end.
        let c = to_clip_time(&s, 1.0, 8.7);
        assert!((c[0].0 - 0.2).abs() < 1e-9 && (c[0].1 - 1.0).abs() < 1e-9);
        assert!((c[1].0 - 8.5).abs() < 1e-9 && (c[1].1 - 8.7).abs() < 1e-9);
    }

    #[test]
    fn keep_graph_consumes_each_input_once() {
        let keep = normalize_keep(&[(1.5, 3.0), (0.0, 1.0), (2.9, 3.5)], 4.0);
        assert_eq!(keep, vec![(0.0, 1.0), (1.5, 3.5)]);
        assert!(normalize_keep(&[(0.0, 4.0)], 4.0).is_empty());
        let base = build_filter_graph("frame", "night", 0.5, Some(&Frame { w: 1080, h: 1920, fit: "card".into(), vx: 0, vy: 556, vw: 1080, vh: 810, crop_box: false }), false, &[], None, "hold", None, true, None);
        for speed in [1.0, 1.5] {
            let g = with_keep(&with_speed(&base, speed, true), &keep, true);
            assert_eq!(g.matches("[0:v]").count(), 1, "{g}");
            assert_eq!(g.matches("[0:a]").count(), 1, "{g}");
            assert_eq!(g.matches("[kv]").count(), 2, "{g}");
            assert_eq!(g.matches("[ka]").count(), 2, "{g}");
            assert_eq!(g.matches("[aout]").count(), 1, "{g}");
        }
        // No graph stage reads the sound: the cut sound still leaves through [aout].
        let g = with_keep(&build_filter_graph("frame", "night", 0.5, None, false, &[], None, "none", None, true, None), &keep, true);
        assert!(g.contains("[ka]anull[aout]"), "{g}");
    }

    #[test]
    fn track_smooths_and_maps_to_the_export_clock() {
        // Left for 6 s, one stray frame, then right: one move, eased around 6 s; the stray is ignored.
        let s: Vec<(f64, Option<f64>)> = [0.2, 0.2, 0.9, 0.2, 0.8, 0.8, 0.8, 0.8].iter().enumerate().map(|(k, &x)| (k as f64 * 2.0, Some(x))).collect();
        let t = smooth_track(&s, 16.0);
        assert_eq!(t.len(), 3, "{t:?}");
        assert!((t[0].1 - 0.2).abs() < 1e-9 && (t[2].1 - 0.8).abs() < 1e-9, "{t:?}");
        assert!(t[1].0 > 5.0 && t[2].0 < 7.0, "{t:?}");
        // Nobody moves: one point, no expression.
        assert_eq!(smooth_track(&[(0.0, Some(0.4)), (2.0, None), (4.0, Some(0.42))], 6.0), vec![(0.0, 0.4)]);
        assert!(pan_expr(&[(0.0, 0.4)]).is_none());
        assert!(smooth_track(&[(0.0, None)], 2.0).is_empty());
        // 2 s cut out before t, then 2× speed.
        assert!((to_output_time(5.0, &[(0.0, 1.0), (3.0, 8.0)], 2.0) - 1.5).abs() < 1e-9);
        let e = pan_expr(&t).unwrap();
        assert!(e.starts_with("clip(") && e.matches("if(").count() == 3, "{e}");
    }

    #[tokio::test]
    async fn real_ffmpeg_smart_framing_follows_the_speaker() {
        let data = std::env::temp_dir().join(format!("subcast-vclip-pan-{}", uuid::Uuid::new_v4()));
        let Some(ffmpeg) = find_ffmpeg(&data) else { return };
        std::fs::create_dir_all(&data).unwrap();
        let src = data.join("src.mp4");
        // A skin-coloured block on the left for 4 s, then on the right.
        let mk = std::process::Command::new(&ffmpeg)
            .args(["-y", "-f", "lavfi", "-i", "color=c=0x202020:s=640x360:d=12:r=30",
                "-vf", "drawbox=x=60:y=100:w=100:h=160:color=0xE0AC69:t=fill:enable='lt(t,4)',drawbox=x=480:y=100:w=100:h=160:color=0xE0AC69:t=fill:enable='gte(t,4)'",
                "-c:v", "libx264", "-preset", "ultrafast", "-pix_fmt", "yuv420p"])
            .arg(&src).output().unwrap();
        assert!(mk.status.success(), "{}", String::from_utf8_lossy(&mk.stderr));
        let track = track_subject(&ffmpeg, &src, 0.0, 12.0).await;
        assert!(track.len() >= 3 && track[0].1 < 0.3 && track.last().unwrap().1 > 0.7, "{track:?}");

        let out = data.join("out.mp4");
        let req = RenderRequest { frame: Some(Frame { w: 360, h: 640, fit: "fill".into(), vx: 0, vy: 0, vw: 360, vh: 640, crop_box: false }),
            output_dir: None, lead: 0.0, speed: 1.0, raw_path: src.to_string_lossy().into_owned(),
            output_path: Some(out.to_string_lossy().into_owned()), default_name: "pan".into(), layout: "frame".into(),
            card_bg: "night".into(), crop_x: 0.5, duration: 12.0, static_overlay_png: None, cues: vec![], cover_png: None,
            cover_mode: "none".into(), embed_thumbnail: false, save_cover_sidecar: false, landscape_cover_png: None,
            keep_ranges: vec![], loudness: false, pan: track.clone() };
        let res = render_clip(&data, req, Arc::new(AtomicBool::new(false)), |_, _, _| {}).await.unwrap();
        // The block sits near the middle of the 9:16 frame both before and after it moved.
        for at in ["1.0", "10.0"] {
            let f = std::process::Command::new(&ffmpeg).args(["-v", "error", "-ss", at, "-i", &res.video_path, "-frames:v", "1",
                "-vf", &format!("scale={TRACK_W}:{TRACK_H}"), "-f", "rawvideo", "-pix_fmt", "rgb24", "-"]).output().unwrap();
            let x = subject_x(&f.stdout, TRACK_W, TRACK_H).unwrap_or(-1.0);
            assert!((x - 0.5).abs() < 0.2, "at {at}s the speaker is at {x}");
        }
        let _ = std::fs::remove_dir_all(&data);
    }

    #[tokio::test]
    async fn real_ffmpeg_remove_pauses() {
        let data = std::env::temp_dir().join(format!("subcast-vclip-keep-{}", uuid::Uuid::new_v4()));
        let Some(ffmpeg) = find_ffmpeg(&data) else { return };
        std::fs::create_dir_all(&data).unwrap();
        let src = data.join("src.mp4");
        // 4 s of tone with a silent 1.2–2.4 s.
        let mk = std::process::Command::new(&ffmpeg)
            .args([
                "-y", "-f", "lavfi", "-i", "testsrc=size=640x360:rate=30:duration=4",
                "-f", "lavfi", "-i", "sine=frequency=440:duration=4",
                "-af", "volume=0.5,volume=enable='between(t,1.2,2.4)':volume=0",
                "-c:v", "libx264", "-preset", "ultrafast", "-c:a", "aac", "-shortest",
            ])
            .arg(&src)
            .output()
            .unwrap();
        assert!(mk.status.success(), "generate test source failed");
        let sil = detect_silences(&ffmpeg, &src, 0.5, 3.5).await;
        assert_eq!(sil.len(), 1, "{sil:?}");
        assert!((sil[0].0 - 0.7).abs() < 0.15 && (sil[0].1 - 1.9).abs() < 0.15, "{sil:?}");

        for (speed, mode) in [(1.0, "none"), (1.25, "hold")] {
            let out = data.join(format!("out-{mode}.mp4"));
            let req = RenderRequest { frame: Some(Frame { w: 1080, h: 1920, fit: "card".into(), vx: 0, vy: 556, vw: 1080, vh: 810, crop_box: false }),
                output_dir: None, lead: 0.0, speed,
                raw_path: src.to_string_lossy().into_owned(),
                output_path: Some(out.to_string_lossy().into_owned()),
                default_name: "keep".into(), layout: "frame".into(), card_bg: "night".into(), crop_x: 0.5, duration: 4.0,
                static_overlay_png: None, cues: vec![],
                cover_png: Some(minimal_rgba_png_b64(1080, 1920, 24, 24, 28, 255)), cover_mode: mode.into(),
                embed_thumbnail: false, save_cover_sidecar: false, landscape_cover_png: None,
                keep_ranges: vec![(0.0, 1.3), (2.3, 4.0)],
                // The second pass also evens the loudness: a quiet tone comes out near −14 LUFS.
                loudness: mode == "hold",
                pan: vec![],
            };
            let res = render_clip(&data, req, Arc::new(AtomicBool::new(false)), |_, _, _| {}).await.unwrap();
            let (d, _, _, audio) = probe_video_meta(&ffmpeg, Path::new(&res.video_path), 0.0).await;
            let want = 3.0 / speed + if mode == "hold" { 1.0 } else { 0.0 };
            assert!(audio && (d - want).abs() < 0.2, "{mode}: {d} vs {want}");
            let meter = std::process::Command::new(&ffmpeg).args(["-hide_banner", "-nostats", "-i", &res.video_path, "-af", "ebur128", "-f", "null", "-"]).output().unwrap();
            let log = String::from_utf8_lossy(&meter.stderr);
            let lufs = parse_lufs(&log).unwrap_or(f64::NAN);
            if mode == "hold" { assert!((lufs - TARGET_LUFS).abs() < 1.5, "loudness {lufs}"); }
            else { assert!(lufs < TARGET_LUFS - 3.0, "untouched tone should stay quiet: {lufs}"); }
        }
        let _ = std::fs::remove_dir_all(&data);
    }
}

#[cfg(test)]
mod render_live {
    use super::*;
    /// Montage from two stretches inside the cached clip (run with --ignored; no network: both come from the cache).
    #[tokio::test]
    #[ignore]
    async fn montage_from_cache() {
        let data = dirs::data_dir().unwrap().join("ai.qiaomu.subcast");
        let t = std::time::Instant::now();
        let p = prepare_montage(&data, "xgAJkGC89rM", &[(545.0, 560.0), (600.0, 615.0)], Arc::new(AtomicBool::new(false)), |s, p, m| eprintln!("{s} {p:.2} {m}")).await.unwrap();
        eprintln!("montage took {:?}: {}x{} {:.2}s burned={} frames={}", t.elapsed(), p.width, p.height, p.duration, p.has_burned_subtitles, p.frames.len());
        assert!((p.duration - 30.0).abs() < 0.3, "{}", p.duration);
        assert_eq!((p.width, p.height), (1920, 1080));
        // The second part starts at 15 s: its first frame is the source at 600 s.
        let ff = find_ffmpeg(&data).unwrap();
        let src = data.join("work/clips/xgAJkGC89rM/541390-646520.mp4");
        for (file, at, name) in [(p.path.clone(), 15.2, "m15"), (src.to_string_lossy().into_owned(), 600.0 - 541.39 + 0.2, "s600")] {
            std::process::Command::new(&ff).args(["-v", "error", "-y", "-ss", &format!("{at:.2}"), "-i", &file, "-frames:v", "1", "-vf", "scale=320:-1", &format!("/tmp/claude-501/{name}.png")]).status().unwrap();
        }
    }

    /// Real render of the cached 105 s clip (run with --ignored): 9:16 card cropped to 4:3, blurred background,
    /// a static layer and 20 subtitle bands. Prints the time and checks the output's size and length.
    #[tokio::test]
    #[ignore]
    async fn renders_card_crop() {
        let data = dirs::data_dir().unwrap().join("ai.qiaomu.subcast");
        let src = data.join("work/clips/xgAJkGC89rM/541390-646520.mp4");
        let ff = find_ffmpeg(&data).unwrap();
        let png = |w: u32, h: u32| -> String {
            let out = std::process::Command::new(&ff).args(["-v", "error", "-f", "lavfi", "-i", &format!("color=c=white@0.0:s={w}x{h},format=rgba,drawbox=x=100:y=20:w=600:h=60:color=white@0.9:t=fill"), "-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"]).output().unwrap();
            base64::engine::general_purpose::STANDARD.encode(out.stdout)
        };
        let band = { let out = std::process::Command::new(&ff).args(["-v", "error", "-f", "lavfi", "-i", "color=c=red:s=1080x120", "-frames:v", "1", "-f", "image2pipe", "-vcodec", "png", "-"]).output().unwrap(); base64::engine::general_purpose::STANDARD.encode(out.stdout) };
        let cues = (0..20).map(|k| CueOverlay { start: k as f64 * 5.0, end: k as f64 * 5.0 + 4.5, y: 1500, png_base64: band.clone() }).collect();
        let out = std::env::temp_dir().join("subcast-render-test.mp4");
        let _ = std::fs::remove_file(&out);
        let req = RenderRequest { lead: 0.0, speed: 1.0,
            output_dir: None,
            frame: Some(Frame { w: 1080, h: 1920, fit: "card".into(), vx: 0, vy: 556, vw: 1080, vh: 810, crop_box: true }),
            raw_path: src.to_string_lossy().into(), output_path: Some(out.to_string_lossy().into()), default_name: "t".into(),
            layout: "frame".into(), card_bg: "blur".into(), crop_x: 0.7, duration: 105.16,
            static_overlay_png: Some(png(1080, 1920)), cues, cover_png: None, cover_mode: "none".into(),
            embed_thumbnail: false, save_cover_sidecar: false, landscape_cover_png: None, keep_ranges: vec![], loudness: false, pan: vec![],
        };
        let t = std::time::Instant::now();
        render_clip(&data, req, Arc::new(AtomicBool::new(false)), |_, _, _| {}).await.unwrap();
        let (dur, w, h, audio) = probe_video_meta(&ff, &out, 0.0).await;
        eprintln!("render took {:?}: {w}x{h}, {dur:.2}s, audio={audio}", t.elapsed());
        assert_eq!((w, h), (1080, 1920));
        assert!((dur - 105.16).abs() < 0.3 && audio);
    }

    /// 1.5× export of the cached clip on a solid card (run with --ignored): shorter by a third, audio kept.
    #[tokio::test]
    #[ignore]
    async fn renders_faster_speed() {
        let data = dirs::data_dir().unwrap().join("ai.qiaomu.subcast");
        let src = data.join("work/clips/xgAJkGC89rM/541390-646520.mp4");
        let ff = find_ffmpeg(&data).unwrap();
        let out = std::env::temp_dir().join("subcast-render-speed.mp4");
        let _ = std::fs::remove_file(&out);
        let req = RenderRequest {
            lead: 0.0, speed: 1.5, output_dir: None,
            frame: Some(Frame { w: 1080, h: 1920, fit: "card".into(), vx: 0, vy: 556, vw: 1080, vh: 810, crop_box: true }),
            raw_path: src.to_string_lossy().into(), output_path: Some(out.to_string_lossy().into()), default_name: "t".into(),
            layout: "frame".into(), card_bg: "night".into(), crop_x: 0.5, duration: 105.16,
            static_overlay_png: None, cues: vec![], cover_png: None, cover_mode: "none".into(),
            embed_thumbnail: false, save_cover_sidecar: false, landscape_cover_png: None, keep_ranges: vec![], loudness: false, pan: vec![],
        };
        let t = std::time::Instant::now();
        render_clip(&data, req, Arc::new(AtomicBool::new(false)), |_, _, _| {}).await.unwrap();
        let (dur, w, h, audio) = probe_video_meta(&ff, &out, 0.0).await;
        eprintln!("speed render took {:?}: {w}x{h}, {dur:.2}s, audio={audio}", t.elapsed());
        assert!((dur - 105.16 / 1.5).abs() < 0.4 && audio, "{dur}");
    }
}

#[cfg(test)]
mod track_live {
    /// SUBCAST_TRACK_FILE: a fetched clip. Prints the speaker track smart framing would follow.
    #[tokio::test]
    #[ignore]
    async fn print_track() {
        let Ok(f) = std::env::var("SUBCAST_TRACK_FILE") else { return };
        let data = dirs::data_dir().unwrap().join("ai.qiaomu.subcast");
        let ff = super::find_ffmpeg(&data).unwrap();
        let p = std::path::Path::new(&f);
        let lead = super::read_lead(p);
        let (d, _, _, _) = super::probe_video_meta(&ff, p, 0.0).await;
        eprintln!("{:?}", super::track_subject(&ff, p, lead, d - lead).await);
    }
}
