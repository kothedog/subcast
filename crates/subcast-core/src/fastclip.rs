//! Fast download of one stretch of a YouTube video, without yt-dlp's per-section ffmpeg download.
//!
//! YouTube serves each quality as one fragmented MP4 whose index (`sidx`) says where every few seconds start.
//! We read that index, fetch only the bytes the stretch needs, and fetch them over several connections at once:
//! googlevideo slows a single connection to roughly playback speed after the first few megabytes, so parallel
//! ranges are 4-8x faster (a 30 s 1080p stretch: about a minute through yt-dlp, a few seconds here). The video
//! and audio addresses come from yt-dlp once and are reused until they expire.
use crate::{asr, net};
use anyhow::{anyhow, bail, Context, Result};
use futures_util::{stream, StreamExt};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Parallel connections per stream, and the size of each range request.
const CONNECTIONS: usize = 8;
const CHUNK: u64 = 1 << 20;

#[derive(Debug, Clone, PartialEq)]
pub struct Fragment { pub offset: u64, pub size: u64, pub start: f64, pub dur: f64 }

#[derive(Debug, Clone, PartialEq)]
pub struct Index { pub init_end: u64, pub fragments: Vec<Fragment> }

/// Reads ftyp/moov/sidx at the start of a fragmented MP4. `Err(n)`: the index needs the first `n` bytes.
pub fn parse_index(head: &[u8]) -> std::result::Result<Index, u64> {
    let mut pos = 0usize;
    let mut init_end = None;
    while pos + 8 <= head.len() {
        let mut size = u32::from_be_bytes(head[pos..pos + 4].try_into().unwrap()) as u64;
        let kind = &head[pos + 4..pos + 8];
        let mut hdr = 8usize;
        if size == 1 {
            if pos + 16 > head.len() { return Err((pos + 16) as u64); }
            size = u64::from_be_bytes(head[pos + 8..pos + 16].try_into().unwrap());
            hdr = 16;
        }
        if size < hdr as u64 { return Err(u64::MAX); }
        let end = pos as u64 + size;
        if kind == b"moov" { init_end = Some(end); }
        if kind == b"sidx" {
            if end > head.len() as u64 { return Err(end); }
            let b = &head[pos + hdr..end as usize];
            let version = b[0];
            let timescale = u32::from_be_bytes(b[8..12].try_into().unwrap()) as f64;
            let (ept, first_off, mut p) = if version == 0 {
                (u32::from_be_bytes(b[12..16].try_into().unwrap()) as u64, u32::from_be_bytes(b[16..20].try_into().unwrap()) as u64, 20usize)
            } else {
                (u64::from_be_bytes(b[12..20].try_into().unwrap()), u64::from_be_bytes(b[20..28].try_into().unwrap()), 28usize)
            };
            p += 2; // reserved
            let count = u16::from_be_bytes(b[p..p + 2].try_into().unwrap()) as usize;
            p += 2;
            let mut offset = end + first_off;
            let mut t = ept as f64 / timescale;
            let mut fragments = Vec::with_capacity(count);
            for _ in 0..count {
                if p + 12 > b.len() { break; }
                let size = (u32::from_be_bytes(b[p..p + 4].try_into().unwrap()) & 0x7fff_ffff) as u64;
                let dur = u32::from_be_bytes(b[p + 4..p + 8].try_into().unwrap()) as f64 / timescale;
                fragments.push(Fragment { offset, size, start: t, dur });
                offset += size;
                t += dur;
                p += 12;
            }
            let init_end = init_end.ok_or(u64::MAX)?;
            return Ok(Index { init_end, fragments });
        }
        pos = end as usize;
        if end > head.len() as u64 { return Err(end + 4096); }
    }
    Err(head.len() as u64 * 2)
}

/// The fragments covering `[start, end]`.
pub fn covering(fr: &[Fragment], start: f64, end: f64) -> &[Fragment] {
    let a = fr.iter().rposition(|f| f.start <= start + 1e-3).unwrap_or(0);
    let b = fr.iter().position(|f| f.start + f.dur >= end - 1e-3).unwrap_or(fr.len().saturating_sub(1));
    if fr.is_empty() || b < a { return &[]; }
    &fr[a..=b]
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Pair { video: String, audio: String, #[serde(default)] headers: Vec<(String, String)> }

/// The best H.264 video up to `max_h` and the best AAC audio, as direct addresses (cached until they expire).
async fn resolve(ytdlp: &Path, video_id: &str, max_h: u32) -> Result<Pair> {
    let key = format!("{video_id}|dash{max_h}");
    // One yt-dlp run per video at a time: a warm-up and a download asking together share its answer.
    let lock = {
        static LOCKS: once_cell::sync::Lazy<parking_lot::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = once_cell::sync::Lazy::new(Default::default);
        LOCKS.lock().entry(key.clone()).or_default().clone()
    };
    let _held = lock.lock().await;
    if let Some(p) = asr::streams::get(&key).and_then(|j| serde_json::from_str::<Pair>(&j).ok()) { return Ok(p); }
    let info: serde_json::Value = asr::with_bot_fallback(|extra| async move {
        let mut cmd = asr::ytdlp_command(ytdlp);
        cmd.args(["-j", "--no-playlist", "--no-warnings"]).args(&extra).arg(format!("https://www.youtube.com/watch?v={video_id}"));
        let out = tokio::time::timeout(std::time::Duration::from_secs(60), cmd.output()).await.map_err(|_| anyhow!("获取视频地址超时"))?.context("无法启动 yt-dlp")?;
        serde_json::from_slice(&out.stdout).map_err(|_| anyhow!("无法获取视频地址：{}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").chars().take(200).collect::<String>()))
    }).await?;
    let formats = info["formats"].as_array().cloned().unwrap_or_default();
    let https = |f: &&serde_json::Value| f["protocol"].as_str() == Some("https") && f["url"].is_string();
    let vf = formats.iter().filter(https)
        .filter(|f| f["vcodec"].as_str().is_some_and(|v| v.starts_with("avc1")) && f["acodec"].as_str() == Some("none"))
        .filter(|f| f["height"].as_u64().is_some_and(|h| h as u32 <= max_h))
        .max_by_key(|f| (f["height"].as_u64().unwrap_or(0), (f["tbr"].as_f64().unwrap_or(0.0) * 10.0) as u64))
        .ok_or_else(|| anyhow!("这个视频没有可分段下载的 H.264 画面"))?;
    let video = vf["url"].as_str().unwrap_or_default().to_string();
    // The address is bound to the client yt-dlp posed as: its headers go with every request.
    let headers: Vec<(String, String)> = vf["http_headers"].as_object().map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))).collect()).unwrap_or_default();
    let audio = formats.iter().filter(https)
        .filter(|f| f["acodec"].as_str().is_some_and(|a| a.starts_with("mp4a")) && f["vcodec"].as_str() == Some("none"))
        // The original language before bitrate: YouTube's AI dubs come at the same bitrate as the original.
        .max_by_key(|f| (asr::audio_rank(f), (f["abr"].as_f64().unwrap_or(0.0) * 10.0) as i64, !f["format_id"].as_str().unwrap_or("").contains("drc")))
        .and_then(|f| f["url"].as_str().map(String::from))
        .ok_or_else(|| anyhow!("这个视频没有可分段下载的音频"))?;
    let pair = Pair { video, audio, headers };
    // Cached as JSON; the stored text still carries the addresses' `expire=`, which sets how long it is kept.
    asr::streams::put(&key, &serde_json::to_string(&pair)?);
    Ok(pair)
}

async fn range(url: &str, headers: &[(String, String)], a: u64, b: u64, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let mut last = None;
    for attempt in 0..4u64 {
        if cancel.load(Ordering::Relaxed) { bail!("已取消"); }
        if attempt > 0 { tokio::time::sleep(std::time::Duration::from_millis(300 * attempt)).await; }
        let mut req = net::client().get(url).header("Range", format!("bytes={a}-{b}"));
        // yt-dlp's browser headers (Sec-Fetch-Mode: navigate, text/html Accept) get 403 on range requests; only
        // headers that describe the media request itself go along.
        for (k, v) in headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case("Origin") || k.eq_ignore_ascii_case("Referer")) { req = req.header(k.as_str(), v.as_str()); }
        match req.send().await {
            Ok(r) if r.status().is_success() => match r.bytes().await {
                Ok(bytes) if bytes.len() as u64 == b - a + 1 => return Ok(bytes.to_vec()),
                Ok(bytes) => last = Some(anyhow!("片段不完整（{} / {}）", bytes.len(), b - a + 1)),
                Err(e) => last = Some(anyhow!(net::describe(e))),
            },
            Ok(r) if r.status().as_u16() == 403 => bail!("视频地址已失效"),
            Ok(r) => last = Some(anyhow!("请求失败（{}）", r.status().as_u16())),
            Err(e) => last = Some(net::describe(e)),
        }
    }
    Err(last.unwrap_or_else(|| anyhow!("下载失败")))
}

/// Downloads the init segment and the fragments covering `[start, end]` into `out` (a playable fragmented MP4).
/// Returns the time (in the source) where `out` begins.
async fn fetch_stretch(url: &str, headers: &[(String, String)], start: f64, end: f64, out: &Path, cancel: &AtomicBool, got: &AtomicU64, total: &AtomicU64) -> Result<f64> {
    let mut want = 256 * 1024u64;
    let (head, index) = loop {
        let head = range(url, headers, 0, want - 1, cancel).await?;
        match parse_index(&head) {
            Ok(i) => break (head, i),
            Err(n) if n != u64::MAX && n > want && n < 16 << 20 => want = n,
            Err(_) => bail!("视频没有分段索引"),
        }
    };
    let frs = covering(&index.fragments, start, end);
    let (first, last) = (frs.first().ok_or_else(|| anyhow!("找不到这一段"))?, frs.last().unwrap());
    let (a, b) = (first.offset, last.offset + last.size - 1);
    total.fetch_add(b - a + 1, Ordering::Relaxed);
    let chunks: Vec<(u64, u64)> = (0..).map(|k| a + k * CHUNK).take_while(|s| *s <= b).map(|s| (s, (s + CHUNK - 1).min(b))).collect();
    let mut parts: Vec<(u64, Vec<u8>)> = stream::iter(chunks)
        .map(|(s, e)| async move {
            let bytes = range(url, headers, s, e, cancel).await?;
            got.fetch_add(bytes.len() as u64, Ordering::Relaxed);
            Ok::<_, anyhow::Error>((s, bytes))
        })
        .buffer_unordered(CONNECTIONS)
        .collect::<Vec<_>>().await
        .into_iter().collect::<Result<_>>()?;
    parts.sort_by_key(|p| p.0);
    let mut file = Vec::with_capacity((index.init_end + b - a + 1) as usize);
    file.extend_from_slice(&head[..index.init_end as usize]);
    for (_, p) in parts { file.extend_from_slice(&p); }
    tokio::fs::write(out, file).await?;
    Ok(first.start)
}

/// Resolves the addresses ahead of time (the slow part, ~30 s), so cutting a clip later only downloads.
pub async fn warm(data: &Path, video_id: &str) -> Result<()> {
    let bin = asr::ytdlp(data).await?;
    resolve(&bin, video_id, 1080).await.map(|_| ())
}

/// `[start, end]` of `video_id` at `dest`: the picture copied from the keyframe before `start` (see
/// `mux_copy`), with the lead recorded beside it. An address that answers 403 (YouTube sometimes hands out
/// ones that can't be fetched directly) is resolved again once before giving up.
pub async fn fetch_section(
    data: &Path,
    ytdlp: &Path,
    ffmpeg: &Path,
    video_id: &str,
    start: f64,
    end: f64,
    dest: &Path,
    cancel: Arc<AtomicBool>,
    mut on_progress: impl FnMut(f64, &str),
) -> Result<()> {
    match fetch_once(data, ytdlp, ffmpeg, video_id, start, end, dest, cancel.clone(), &mut on_progress).await {
        Err(e) if !cancel.load(Ordering::Relaxed) && e.to_string().contains("视频地址已失效") => {
            fetch_once(data, ytdlp, ffmpeg, video_id, start, end, dest, cancel, &mut on_progress).await
        }
        r => r,
    }
}

#[allow(clippy::too_many_arguments)]
async fn fetch_once(
    data: &Path,
    ytdlp: &Path,
    ffmpeg: &Path,
    video_id: &str,
    start: f64,
    end: f64,
    dest: &Path,
    cancel: Arc<AtomicBool>,
    on_progress: &mut impl FnMut(f64, &str),
) -> Result<()> {
    // Resolving can take ~30 s the first time: the bar keeps creeping (a slowing curve) instead of sitting at 0.
    let pair = {
        let fut = resolve(ytdlp, video_id, 1080);
        tokio::pin!(fut);
        let started = std::time::Instant::now();
        loop {
            tokio::select! {
                r = &mut fut => break r?,
                _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {
                    let t = started.elapsed().as_secs_f64();
                    on_progress(0.3 * (1.0 - (-t / 14.0).exp()), "第一次剪这个视频，正在解析视频地址…");
                }
            }
        }
    };
    // Stages: resolve 0–30%, download 30–88%, exact cut 90%.
    let base = 0.3;
    let work = data.join("work").join(format!("fast-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir_all(&work).await?;
    let res = async {
        let (vp, ap) = (work.join("v.mp4"), work.join("a.m4a"));
        let (got, total) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
        let done = Arc::new(AtomicBool::new(false));
        let fetch = async {
            let r = tokio::try_join!(
                fetch_stretch(&pair.video, &pair.headers, start, end, &vp, &cancel, &got, &total),
                fetch_stretch(&pair.audio, &pair.headers, start, end, &ap, &cancel, &got, &total),
            );
            done.store(true, Ordering::Relaxed);
            r
        };
        let report = async {
            while !done.load(Ordering::Relaxed) {
                let (g, t) = (got.load(Ordering::Relaxed), total.load(Ordering::Relaxed).max(1));
                on_progress(base + (g as f64 / t as f64).min(1.0) * (0.88 - base), "正在下载这一段画面…");
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        };
        let (fetched, ()) = tokio::join!(fetch, report);
        let (v0, a0) = fetched?;
        on_progress(0.92, "正在合成片段…");
        crate::videoclip::mux_copy(ffmpeg, &vp, v0, &ap, a0, start, end, dest, &cancel).await
    }.await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    if res.is_err() && !cancel.load(Ordering::Relaxed) {
        // A stale address (or any odd failure) shouldn't stick: the next try resolves afresh.
        asr::streams::forget(&format!("{video_id}|dash1080"));
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidx(timescale: u32, refs: &[(u32, u32)]) -> Vec<u8> {
        let mut body = vec![0u8, 0, 0, 0];
        body.extend_from_slice(&1u32.to_be_bytes());
        body.extend_from_slice(&timescale.to_be_bytes());
        body.extend_from_slice(&0u32.to_be_bytes()); // earliest presentation time
        body.extend_from_slice(&0u32.to_be_bytes()); // first offset
        body.extend_from_slice(&0u16.to_be_bytes());
        body.extend_from_slice(&(refs.len() as u16).to_be_bytes());
        for &(size, dur) in refs {
            body.extend_from_slice(&size.to_be_bytes());
            body.extend_from_slice(&dur.to_be_bytes());
            body.extend_from_slice(&0x9000_0000u32.to_be_bytes());
        }
        let mut b = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(b"sidx");
        b.extend(body);
        b
    }

    fn boxed(kind: &[u8; 4], len: usize) -> Vec<u8> {
        let mut b = ((len + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(kind);
        b.extend(vec![0u8; len]);
        b
    }

    #[test]
    fn reads_the_index_and_picks_fragments() {
        let mut head = boxed(b"ftyp", 16);
        head.extend(boxed(b"moov", 100));
        let init_end = head.len() as u64;
        let s = sidx(1000, &[(500, 5000), (600, 5000), (700, 5000), (800, 5000)]);
        let first = init_end + s.len() as u64;
        head.extend(s);
        let ix = parse_index(&head).unwrap();
        assert_eq!(ix.init_end, init_end);
        assert_eq!(ix.fragments.len(), 4);
        assert_eq!(ix.fragments[0].offset, first);
        assert_eq!(ix.fragments[2], Fragment { offset: first + 1100, size: 700, start: 10.0, dur: 5.0 });
        let c = covering(&ix.fragments, 7.0, 12.0);
        assert_eq!((c[0].start, c.len()), (5.0, 2));
        // Cut short before the sidx ends: asks for more.
        assert!(parse_index(&head[..head.len() - 10]).is_err());
    }

    /// Network: a real 30 s stretch (run with --ignored).
    #[tokio::test]
    #[ignore]
    async fn real_section() {
        let data = dirs::data_dir().unwrap().join("ai.qiaomu.subcast");
        let ytdlp = data.join("bin").join("yt-dlp");
        let ffmpeg = crate::videoclip::find_ffmpeg(&data).unwrap();
        let dest = std::env::temp_dir().join("subcast-fast-test.mp4");
        let t = std::time::Instant::now();
        fetch_section(&data, &ytdlp, &ffmpeg, "xgAJkGC89rM", 541.39, 571.39, &dest, Arc::new(AtomicBool::new(false)), |p, m| eprintln!("T+{:.1}s {p:.2} {m}", t.elapsed().as_secs_f64())).await.unwrap();
        eprintln!("took {:?}, {} bytes", t.elapsed(), std::fs::metadata(&dest).unwrap().len());
    }
}

