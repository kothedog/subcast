//! Episodes saved for listening offline: one audio file per episode, named by episode id, in `downloads/`.
//! `audio-cache/` holds the same kind of files kept on their own: the audio an episode was transcribed from, or
//! fetched to line its subtitles up, so it plays from the very file its subtitle times fit. It is trimmed to
//! `CACHE_LIMIT`, oldest played first, and never mixed with what the listener chose to download.
use crate::{asr, model::Episode};
use anyhow::{anyhow, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Saved {
    pub episode_id: String,
    pub path: String,
    pub bytes: u64,
    pub saved_at: i64,
}

pub fn dir(data: &Path) -> PathBuf { data.join("downloads") }
pub fn cache_dir(data: &Path) -> PathBuf { data.join("audio-cache") }
pub const CACHE_LIMIT: u64 = 2 * 1024 * 1024 * 1024;

/// The file extension an audio address points at (mp3 when it does not say).
pub fn audio_ext(url: &str) -> String {
    url.split('?').next().and_then(|u| u.rsplit('/').next()).and_then(|f| f.rsplit_once('.')).map(|(_, x)| x)
        .filter(|x| (2..=4).contains(&x.len()) && x.chars().all(|c| c.is_ascii_alphanumeric())).unwrap_or("mp3").to_lowercase()
}

fn saved_file(p: &Path) -> Option<Saved> {
    let id = p.file_stem()?.to_str()?;
    let ext = p.extension()?.to_str()?;
    if ext == "part" || id.starts_with('.') { return None; }
    let meta = std::fs::metadata(p).ok().filter(|m| m.is_file())?;
    let saved_at = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as i64).unwrap_or(0);
    Some(Saved { episode_id: id.to_string(), path: p.to_string_lossy().into_owned(), bytes: meta.len(), saved_at })
}

pub fn list(data: &Path) -> Vec<Saved> { list_in(&dir(data)) }

fn list_in(d: &Path) -> Vec<Saved> {
    let Ok(rd) = std::fs::read_dir(d) else { return vec![] };
    let mut out: Vec<Saved> = rd.flatten().filter_map(|e| saved_file(&e.path())).collect();
    out.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
    out
}

pub fn find(data: &Path, episode_id: &str) -> Option<Saved> { list(data).into_iter().find(|s| s.episode_id == episode_id) }

/// The cached audio of an episode. Looking it up counts as playing it, for trimming.
pub fn cached(data: &Path, episode_id: &str) -> Option<Saved> {
    let s = list_in(&cache_dir(data)).into_iter().find(|s| s.episode_id == episode_id)?;
    if let Ok(f) = std::fs::File::options().append(true).open(&s.path) { let _ = f.set_modified(std::time::SystemTime::now()); }
    Some(s)
}

pub fn list_cached(data: &Path) -> Vec<Saved> { list_in(&cache_dir(data)) }

/// Where an episode's audio would be cached (the file need not exist).
pub fn cache_path(data: &Path, episode_id: &str, url: &str) -> PathBuf { cache_dir(data).join(format!("{episode_id}.{}", audio_ext(url))) }

/// Moves a finished audio file into the cache under this episode, then trims the cache.
pub fn keep_cached(data: &Path, episode_id: &str, file: &Path) -> Result<Saved> {
    let d = cache_dir(data);
    std::fs::create_dir_all(&d)?;
    let ext = file.extension().and_then(|x| x.to_str()).unwrap_or("mp3");
    let dest = d.join(format!("{episode_id}.{ext}"));
    if std::fs::rename(file, &dest).is_err() { std::fs::copy(file, &dest)?; let _ = std::fs::remove_file(file); }
    trim_cache(data, CACHE_LIMIT, episode_id);
    saved_file(&dest).ok_or_else(|| anyhow!("缓存音频失败"))
}

/// Deletes the least recently played cached files until the cache fits `limit` (`keep` stays regardless).
pub fn trim_cache(data: &Path, limit: u64, keep: &str) {
    let mut all = list_in(&cache_dir(data));
    let mut total: u64 = all.iter().map(|s| s.bytes).sum();
    all.sort_by_key(|s| s.saved_at);
    for s in all {
        if total <= limit { break; }
        if s.episode_id == keep { continue; }
        if std::fs::remove_file(&s.path).is_ok() { total -= s.bytes; }
    }
}

pub fn clear_cache(data: &Path) -> Result<()> {
    let d = cache_dir(data);
    if d.is_dir() { std::fs::remove_dir_all(&d)?; }
    Ok(())
}

pub fn remove(data: &Path, episode_id: &str) -> Result<()> {
    if let Some(s) = find(data, episode_id) { std::fs::remove_file(s.path)?; }
    Ok(())
}

pub fn clear(data: &Path) -> Result<()> {
    let d = dir(data);
    if d.is_dir() { std::fs::remove_dir_all(&d)?; }
    Ok(())
}

/// Saves the episode. Podcasts download their audio file; YouTube videos save the video (720p with the original
/// sound) through yt-dlp. Progress 0–1, `None` while it's starting.
pub async fn download(data: &Path, ep: &Episode, cancel: Arc<AtomicBool>, mut progress: impl FnMut(Option<f64>)) -> Result<Saved> {
    let d = dir(data);
    tokio::fs::create_dir_all(&d).await?;
    let dest = if let Some(url) = &ep.audio_url {
        let dest = d.join(format!("{}.{}", ep.id, audio_ext(url)));
        asr::download(url, &dest, &cancel, |got, total| progress(total.map(|t| got as f64 / t.max(1) as f64))).await?;
        dest
    } else if let Some(vid) = &ep.video_id {
        // A video episode keeps its picture too, so it plays from disk instead of loading YouTube each time.
        progress(None);
        let tmp = d.join(format!(".tmp-{}", ep.id));
        let ffmpeg = crate::videoclip::ensure_ffmpeg(data, &cancel, |_, _| {}).await.ok();
        let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<f64>();
        let job = asr::with_ytdlp(data, |bin| {
            let (v, t, c, f, tx) = (vid.clone(), tmp.clone(), cancel.clone(), ffmpeg.clone(), ptx.clone());
            async move { asr::youtube_video(&bin, f.as_deref(), &v, &t, c, move |p| { let _ = tx.send(p); }).await }
        });
        tokio::pin!(job);
        let got = loop {
            tokio::select! {
                r = &mut job => break r,
                Some(p) = prx.recv() => progress(Some(p)),
            }
        };
        let result = match got {
            Ok(file) => {
                let ext = file.extension().and_then(|x| x.to_str()).unwrap_or("mp4").to_string();
                let dest = d.join(format!("{}.{ext}", ep.id));
                tokio::fs::rename(&file, &dest).await.map(|_| dest).map_err(anyhow::Error::from)
            }
            Err(err) => Err(err),
        };
        let _ = tokio::fs::remove_dir_all(&tmp).await;
        result?
    } else {
        return Err(anyhow!("这集没有音频地址"));
    };
    progress(Some(1.0));
    saved_file(&dest).ok_or_else(|| anyhow!("下载失败"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn extensions() {
        assert_eq!(audio_ext("https://a.com/x/ep.MP3?t=1"), "mp3");
        assert_eq!(audio_ext("https://a.com/x/ep.m4a"), "m4a");
        assert_eq!(audio_ext("https://a.com/x/redirect"), "mp3");
        assert_eq!(audio_ext("https://a.com/v1.2/stream"), "mp3");
    }
    #[test]
    fn lists_only_finished_files() {
        let data = std::env::temp_dir().join(format!("subcast-offline-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir(&data)).unwrap();
        std::fs::write(dir(&data).join("a.mp3"), b"123").unwrap();
        std::fs::write(dir(&data).join("b.part"), b"1").unwrap();
        std::fs::create_dir_all(dir(&data).join(".tmp-c")).unwrap();
        let l = list(&data);
        assert_eq!(l.len(), 1);
        assert_eq!((l[0].episode_id.as_str(), l[0].bytes), ("a", 3));
        remove(&data, "a").unwrap();
        assert!(find(&data, "a").is_none());
        clear(&data).unwrap();
        let _ = std::fs::remove_dir_all(&data);
    }
    #[test]
    fn cache_keeps_recent_files_under_the_limit() {
        let data = std::env::temp_dir().join(format!("subcast-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&data).unwrap();
        for (i, id) in ["old", "mid", "new"].iter().enumerate() {
            let f = data.join(format!("{id}.mp3"));
            std::fs::write(&f, vec![0u8; 100]).unwrap();
            keep_cached(&data, id, &f).unwrap();
            let t = std::time::SystemTime::now() - std::time::Duration::from_secs(100 - i as u64 * 10);
            std::fs::File::options().append(true).open(cache_dir(&data).join(format!("{id}.mp3"))).unwrap().set_modified(t).unwrap();
        }
        assert!(cached(&data, "old").is_some()); // played now: the newest
        trim_cache(&data, 200, "new");
        let left: Vec<String> = list_cached(&data).into_iter().map(|s| s.episode_id).collect();
        assert!(left.contains(&"old".to_string()) && left.contains(&"new".to_string()) && left.len() == 2, "{left:?}");
        assert!(list(&data).is_empty(), "the cache is not a download");
        clear_cache(&data).unwrap();
        let _ = std::fs::remove_dir_all(&data);
    }
}
