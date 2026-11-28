//! Headless application operations shared by the Tauri desktop shell and the `subcast` CLI.
use crate::ctx::{apply_remote_config, apply_runtime_settings, CoreCtx, ProgressEvent, ProgressSink};
use crate::{
    agents, align, asr, asr_context, clip, cloud, db, feeds, hlsproxy, import, llm, local_tts,
    model::*, net, offline, qiaomu, shared, speech, transcript, translate, videoclip, youtube,
};
use anyhow::{anyhow, Context, Result};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub const REMOTE_CONFIG_URL: &str = "https://subcast.qiaomu.ai/tools/config.json";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Added {
    pub show: Option<Show>,
    pub episode: Option<Episode>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub show: Show,
    pub episodes: Vec<Episode>,
    pub fetched_at: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Refreshed {
    pub shows: usize,
    pub new_episodes: usize,
    pub skipped: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShowHit {
    pub episode: Episode,
    /// title | notes | transcript
    pub field: &'static str,
    pub snippet: String,
    /// Where in the episode a subtitle hit is, in seconds.
    pub time: Option<f64>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackChoice {
    pub language: String,
    pub auto: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoStream {
    pub url: String,
    /// hls | mp4
    pub kind: &'static str,
    /// The video's length in seconds, when known (yt-dlp reports it).
    pub duration: Option<f64>,
}

/// One address lookup per video at a time: a second request (the player and its fallback can both ask) waits for
/// the first and then finds its address in the cache, instead of starting another yt-dlp.
fn stream_lock(video_id: &str) -> std::sync::Arc<tokio::sync::Mutex<()>> {
    static LOCKS: once_cell::sync::Lazy<parking_lot::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>> = once_cell::sync::Lazy::new(Default::default);
    let mut m = LOCKS.lock();
    m.retain(|_, l| std::sync::Arc::strong_count(l) > 1);
    m.entry(video_id.to_string()).or_default().clone()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Aligned {
    /// same: subtitles fit the file that plays | aligned: `map` converts | unknown: nothing to compare with | failed: files share too little
    pub status: &'static str,
    pub map: Option<align::TimeMap>,
    pub path: Option<String>,
    pub local_bytes: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Favorite {
    pub id: String,
    pub at: i64,
}

pub fn store(ctx: &CoreCtx, show: &Show, eps: &[Episode]) -> Result<()> {
    let mut conn = ctx.db.lock();
    db::upsert_show(&conn, show)?;
    db::upsert_episodes(&mut conn, eps)?;
    Ok(())
}

/// Fetches any subscription's latest state. Curated 精读 channels also cache their Chinese titles.
pub async fn fetch_any(ctx: &CoreCtx, feed_url: &str, kind: &str) -> Result<(Show, Vec<Episode>)> {
    if kind == "qiaomu" {
        let (source_id, ch) = if let Some(id) = qiaomu::rss_source_id(feed_url) {
            let ch = qiaomu::rss_channel(&id).await?;
            (id, ch)
        } else {
            let id = qiaomu::site_id(feed_url).ok_or_else(|| anyhow!("不是精读频道"))?;
            let ch = qiaomu::channel(&id).await?;
            (id, ch)
        };
        let conn = ctx.db.lock();
        if ch.videos_checked || crate::curated_video::is_audio_only(&source_id) {
            let _ = db::clear_show_videos(&conn, &ch.show.id);
        }
        if let Ok(tx) = conn.unchecked_transaction() {
            for (orig, zh) in &ch.zh_titles {
                let _ = db::tr_put(&tx, &short_hash(&format!("zh-CN|{orig}"), 24), zh);
            }
            let _ = tx.commit();
        }
        return Ok((ch.show, ch.episodes));
    }
    feeds::fetch_show(feed_url, kind).await
}

pub async fn youtube_channel_feed(ctx: &CoreCtx, video_id: &str) -> Result<String> {
    let ep_id = short_hash(&format!("single:{video_id}"), 20);
    if let Some(feed) = db::get_blob(&ctx.db.lock(), &ep_id, "youtube-channel-feed")? {
        return Ok(feed);
    }
    let info = tokio::time::timeout(std::time::Duration::from_secs(8), youtube::video_info(video_id))
        .await
        .map_err(|_| anyhow!("没有在这个页面找到 YouTube 频道"))??;
    let id = info.channel_id.ok_or_else(|| anyhow!("没有在这个页面找到 YouTube 频道"))?;
    let feed = feeds::channel_feed(&id);
    db::set_blob(&ctx.db.lock(), &ep_id, "youtube-channel-feed", &feed)?;
    Ok(feed)
}

pub async fn single_youtube(ctx: &CoreCtx, video_id: &str) -> Result<Episode> {
    let info = youtube::video_info(video_id).await?;
    let ep = Episode {
        id: short_hash(&format!("single:{video_id}"), 20),
        show_id: "single".into(),
        guid: video_id.into(),
        title: if info.title.is_empty() { video_id.to_string() } else { info.title },
        published: Some(now_ms()),
        duration: info.duration,
        video_id: Some(video_id.into()),
        link: Some(format!("https://www.youtube.com/watch?v={video_id}")),
        cover: Some(format!("https://i.ytimg.com/vi/{video_id}/hqdefault.jpg")),
        notes: Some(format!("{}\n\n{}", info.author, info.description)),
        ..Default::default()
    };
    let mut conn = ctx.db.lock();
    db::upsert_episodes(&mut conn, std::slice::from_ref(&ep))?;
    if let Some(id) = info.channel_id {
        let _ = db::set_blob(&conn, &ep.id, "youtube-channel-feed", &feeds::channel_feed(&id));
    }
    Ok(db::get_episode(&conn, &ep.id)?.unwrap_or(ep))
}

pub enum Fetched {
    Show(Show, Vec<Episode>),
    Single(Episode),
}

pub async fn fetch_input(ctx: &CoreCtx, input: &str) -> Result<Fetched> {
    let normalized = if input.trim().starts_with("http") {
        input.trim().to_string()
    } else {
        format!("https://{}", input.trim())
    };
    if let Some(id) = qiaomu::rss_source_id(&normalized) {
        let (show, eps) = fetch_any(ctx, &qiaomu::rss_feed_url(&id), "qiaomu").await?;
        return Ok(Fetched::Show(show, eps));
    }
    if let Some(id) = qiaomu::site_id(&normalized) {
        let (show, eps) = fetch_any(ctx, &qiaomu::feed_url(&id), "qiaomu").await?;
        return Ok(Fetched::Show(show, eps));
    }
    let (show, eps) = match feeds::resolve(input).await? {
        feeds::Source::YouTubeVideo(id) => return Ok(Fetched::Single(single_youtube(ctx, &id).await?)),
        feeds::Source::XiaoyuzhouEpisode(url) => {
            let mut ep = feeds::xiaoyuzhou_episode(&url).await?;
            ep.id = short_hash(&format!("single:{}", ep.guid), 20);
            let mut conn = ctx.db.lock();
            db::upsert_episodes(&mut conn, std::slice::from_ref(&ep))?;
            let ep = db::get_episode(&conn, &ep.id)?.unwrap_or(ep);
            return Ok(Fetched::Single(ep));
        }
        feeds::Source::Xiaoyuzhou(url) => feeds::xiaoyuzhou_show(&url).await?,
        feeds::Source::Feed(url) => match feeds::fetch_show(&url, "podcast").await {
            Ok(r) if !r.1.is_empty() => r,
            other if url.contains("playlist_id=UULF") => {
                let all = url.replace("playlist_id=UULF", "channel_id=UC");
                match feeds::fetch_show(&all, "podcast").await {
                    Ok(r) => r,
                    Err(err) => return Err(other.err().unwrap_or(err)),
                }
            }
            other => other?,
        },
    };
    if eps.is_empty() {
        return Err(anyhow!("这个订阅源里没有可播放的节目"));
    }
    Ok(Fetched::Show(show, eps))
}

pub async fn with_channel_avatar(mut show: Show, existing: Option<String>) -> Show {
    if show.kind != "youtube" {
        return show;
    }
    if existing.as_deref().is_some_and(|c| c.starts_with("data:image/")) {
        show.cover = existing;
        return show;
    }
    if let Ok(Some(avatar)) =
        tokio::time::timeout(std::time::Duration::from_secs(8), feeds::youtube_channel_avatar(&show.feed_url)).await
    {
        show.cover = Some(avatar);
    }
    show
}

pub async fn add_subscription(ctx: &CoreCtx, input: &str) -> Result<Added> {
    match fetch_input(ctx, input).await? {
        Fetched::Single(ep) => Ok(Added { show: None, episode: Some(ep) }),
        Fetched::Show(show, eps) => {
            let show = with_channel_avatar(show, None).await;
            store(ctx, &show, &eps)?;
            Ok(Added { show: Some(show), episode: None })
        }
    }
}

pub fn known_show_id(input: &str) -> Option<String> {
    let normalized = if input.trim().starts_with("http") {
        input.trim().to_string()
    } else {
        format!("https://{}", input.trim())
    };
    if let Some(id) = qiaomu::rss_source_id(&normalized) {
        return Some(short_hash(&qiaomu::rss_feed_url(&id), 16));
    }
    if let Some(id) = qiaomu::site_id(&normalized) {
        return Some(short_hash(&qiaomu::feed_url(&id), 16));
    }
    Some(short_hash(input.trim(), 16))
}

pub fn cached_show(ctx: &CoreCtx, input: &str) -> Result<Option<Preview>> {
    let Some(id) = known_show_id(input) else { return Ok(None) };
    let conn = ctx.db.lock();
    let Some(json) = db::get_blob(&conn, &id, "show-json")? else { return Ok(None) };
    let Ok(show) = serde_json::from_str::<Show>(&json) else { return Ok(None) };
    let fetched_at = db::get_blob(&conn, &id, "fetched-at")?.and_then(|t| t.parse().ok()).unwrap_or(0);
    let episodes = db::list_episodes(&conn, Some(&id), 300)?;
    if episodes.is_empty() {
        return Ok(None);
    }
    Ok(Some(Preview { show, episodes, fetched_at }))
}

pub async fn preview_show(ctx: &CoreCtx, input: &str) -> Result<Preview> {
    match fetch_input(ctx, input).await? {
        Fetched::Single(_) => Err(anyhow!("这是单集链接，不是频道")),
        Fetched::Show(show, eps) => keep_preview(ctx, show, eps),
    }
}

/// Keeps a fetched show's episodes (and the show's details, for its name) without following the show.
pub fn keep_preview(ctx: &CoreCtx, show: Show, eps: Vec<Episode>) -> Result<Preview> {
    let mut conn = ctx.db.lock();
    db::upsert_episodes(&mut conn, &eps)?;
    let now = now_ms();
    if let Ok(json) = serde_json::to_string(&show) {
        let _ = db::set_blob(&conn, &show.id, "show-json", &json);
    }
    let _ = db::set_blob(&conn, &show.id, "fetched-at", &now.to_string());
    let episodes = db::list_episodes(&conn, Some(&show.id), 300)?;
    Ok(Preview { show, episodes, fetched_at: now })
}

pub async fn refresh_show(ctx: &CoreCtx, id: &str) -> Result<Show> {
    let show = db::get_show(&ctx.db.lock(), id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    let existing = show.cover.clone();
    let (fresh, eps) = fetch_any(ctx, &show.feed_url, &show.kind).await?;
    let fresh = Show { id: show.id.clone(), kind: show.kind.clone(), ..fresh };
    let fresh = with_channel_avatar(fresh, existing).await;
    store(ctx, &fresh, &eps)?;
    Ok(fresh)
}

static REFRESHING: AtomicBool = AtomicBool::new(false);

pub async fn refresh_all(ctx: &CoreCtx) -> Result<Refreshed> {
    if REFRESHING.swap(true, Ordering::SeqCst) {
        return Ok(Refreshed { shows: 0, new_episodes: 0, skipped: true });
    }
    let r = async {
        let shows: Vec<Show> = db::list_shows(&ctx.db.lock())?
            .into_iter()
            .filter(|s| s.kind != "single")
            .collect();
        let (mut ok, mut fresh_eps) = (0, 0);
        let slots = tokio::sync::Semaphore::new(6);
        let results = futures_util::future::join_all(shows.iter().map(|s| {
            let slots = &slots;
            async move {
                let _slot = slots.acquire().await;
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    fetch_any(ctx, &s.feed_url, &s.kind),
                )
                .await;
                let result = match result {
                    Ok(Ok((fresh, eps))) => Ok(Ok((with_channel_avatar(fresh, s.cover.clone()).await, eps))),
                    other => other,
                };
                (s, result)
            }
        }))
        .await;
        for (s, r) in results {
            let Ok(Ok((fresh, eps))) = r else { continue };
            let known = {
                let conn = ctx.db.lock();
                let ids = db::list_show_episode_ids(&conn, &s.id).unwrap_or_default();
                eps.iter().filter(|x| ids.contains(&x.id)).count()
            };
            let fresh = Show { id: s.id.clone(), kind: s.kind.clone(), ..fresh };
            if store(ctx, &fresh, &eps).is_ok() {
                ok += 1;
                fresh_eps += eps.len() - known;
            }
        }
        Ok(Refreshed { shows: ok, new_episodes: fresh_eps, skipped: false })
    }
    .await;
    REFRESHING.store(false, Ordering::SeqCst);
    r
}

pub async fn search_show(ctx: &CoreCtx, show_id: &str, query: &str) -> Result<Vec<ShowHit>> {
    let path = ctx.data.join("subcast.db");
    let sid = show_id.to_string();
    let q = query.to_string();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        search_episodes(&conn, &sid, &q)
    })
    .await?
}

pub fn search_episodes(conn: &Connection, show_id: &str, query: &str) -> Result<Vec<ShowHit>> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Ok(vec![]);
    }
    let eps = db::list_episodes(conn, Some(show_id), 5000)?;
    let all_notes = db::show_notes(conn, show_id)?;
    static TAGS: once_cell::sync::Lazy<regex::Regex> =
        once_cell::sync::Lazy::new(|| regex::Regex::new(r"<[^>]+>").unwrap());
    let tags = &*TAGS;
    let has = |text: &str| {
        let l = text.to_lowercase();
        words.iter().all(|w| l.contains(w))
    };
    let mut hits = Vec::new();
    for ep in eps {
        let zh_title = db::tr_get(conn, &short_hash(&format!("zh-CN|{}", ep.title), 24)).ok().flatten();
        let title = match &zh_title {
            Some(z) => format!("{} {z}", ep.title),
            None => ep.title.clone(),
        };
        if has(&title) {
            hits.push(ShowHit { snippet: String::new(), field: "title", time: None, episode: ep });
            continue;
        }
        let notes = all_notes.get(&ep.id).map(|n| tags.replace_all(n, " ").to_string()).unwrap_or_default();
        if has(&notes) {
            hits.push(ShowHit { snippet: snippet(&notes, &words[0]), field: "notes", time: None, episode: ep });
            continue;
        }
        if !ep.has_transcript {
            continue;
        }
        let Some((raw_segs, raw_zh)) = db::raw_transcript_and_zh(conn, &ep.id).ok().flatten() else {
            continue;
        };
        if words.iter().all(|w| !w.contains('"') && !w.contains('\\')) {
            let segs_lower = raw_segs.to_lowercase();
            let zh_lower = raw_zh.as_deref().map(str::to_lowercase);
            if !words.iter().all(|w| segs_lower.contains(w) || zh_lower.as_deref().is_some_and(|z| z.contains(w))) {
                continue;
            }
        }
        let Ok(segs) = serde_json::from_str::<Vec<Segment>>(&raw_segs) else { continue };
        let zh: Vec<Option<String>> = raw_zh.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        let found = segs.iter().enumerate().find_map(|(i, s)| {
            let line = match zh.get(i).cloned().flatten() {
                Some(z) => format!("{} {z}", s.text),
                None => s.text.clone(),
            };
            has(&line).then(|| (s.start, line))
        });
        if let Some((start, line)) = found {
            hits.push(ShowHit { snippet: snippet(&line, &words[0]), field: "transcript", time: Some(start), episode: ep });
        }
    }
    hits.sort_by_key(|h| match h.field {
        "title" => 0,
        "notes" => 1,
        _ => 2,
    });
    Ok(hits)
}

fn snippet(text: &str, word: &str) -> String {
    let chars: Vec<char> = text.split_whitespace().collect::<Vec<_>>().join(" ").chars().collect();
    let lower: String = chars.iter().collect::<String>().to_lowercase();
    let at = lower.find(word).map(|b| lower[..b].chars().count()).unwrap_or(0);
    let from = at.saturating_sub(25);
    let to = (at + 45).min(chars.len());
    format!(
        "{}{}{}",
        if from > 0 { "…" } else { "" },
        chars[from..to].iter().collect::<String>(),
        if to < chars.len() { "…" } else { "" }
    )
}

pub async fn youtube_transcript(
    ctx: &CoreCtx,
    ep: &Episode,
    info: &youtube::VideoInfo,
    track: &youtube::Track,
) -> Result<Transcript> {
    let segs = transcript::resegment(transcript::clean(youtube::fetch_track(track).await?));
    let t = Transcript {
        episode_id: ep.id.clone(),
        source: if track.auto { "youtube-auto" } else { "youtube" }.into(),
        language: track.language.clone(),
        segments: segs,
    };
    if t.segments.is_empty() {
        return Ok(t);
    }
    db::save_transcript(&ctx.db.lock(), &t)?;
    if let Some(zh) = youtube::official_chinese(&info.tracks, &track.language) {
        if let Ok(lines) = youtube::fetch_track(zh).await {
            let items = youtube::align(&t.segments, &transcript::clean(lines));
            if items.iter().filter(|x| x.is_some()).count() * 10 >= items.len() * 6 {
                let conn = ctx.db.lock();
                db::set_blob(&conn, &ep.id, "zh", &serde_json::to_string(&items)?)?;
                db::set_blob(&conn, &ep.id, "zh-engine", "youtube")?;
            }
        }
    }
    Ok(t)
}

pub async fn youtube_tracks(ctx: &CoreCtx, episode_id: &str) -> Result<Vec<TrackChoice>> {
    let ep = db::get_episode(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    let Some(vid) = &ep.video_id else { return Ok(vec![]) };
    let info = youtube::video_info(vid).await?;
    let mut out: Vec<TrackChoice> = Vec::new();
    for t in info.tracks.iter().filter(|t| !t.auto).chain(info.tracks.iter().filter(|t| t.auto)) {
        if !out.iter().any(|c| c.language == t.language) {
            out.push(TrackChoice { language: t.language.clone(), auto: t.auto });
        }
    }
    Ok(out)
}

pub async fn use_youtube_track(ctx: &CoreCtx, episode_id: &str, language: &str, auto: bool) -> Result<Transcript> {
    let ep = db::get_episode(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    let vid = ep.video_id.clone().ok_or_else(|| anyhow!("不是视频节目"))?;
    let info = youtube::video_info(&vid).await?;
    let track = info
        .tracks
        .iter()
        .find(|t| t.language == language && t.auto == auto)
        .ok_or_else(|| anyhow!("没有这个语言的字幕"))?
        .clone();
    let t = youtube_transcript(ctx, &ep, &info, &track).await?;
    if t.segments.is_empty() {
        return Err(anyhow!("这条字幕是空的"));
    }
    db::set_blob(&ctx.db.lock(), episode_id, "track-picked", "1")?;
    Ok(t)
}

pub async fn fetch_official_transcript(ctx: &CoreCtx, episode_id: &str) -> Result<Option<Transcript>> {
    let ep = db::get_episode(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    let t = if let Some(vid) = &ep.video_id {
        let info = youtube::video_info(vid).await?;
        let hint = youtube::script_hint(&info.title).or_else(|| youtube::script_hint(&ep.title));
        match youtube::choose(&info.tracks, info.original_language.as_deref(), hint) {
            Some(track) => {
                let t = youtube_transcript(ctx, &ep, &info, track).await?;
                return Ok((!t.segments.is_empty()).then_some(t));
            }
            None => None,
        }
    } else if let Some(url) = ep.transcript_url.as_ref().filter(|_| ep.transcript_type.as_deref() != Some("podscribe")) {
        let body = net::get_text(url).await?;
        let kind = ep.transcript_type.clone().unwrap_or_default();
        let segs = if kind.contains("json") {
            transcript::parse_podcast_json(&body)
        } else {
            transcript::parse_cues(&body)
        };
        let segs = transcript::resegment(transcript::clean(segs));
        (!segs.is_empty()).then(|| Transcript {
            episode_id: ep.id.clone(),
            source: "rss".into(),
            language: "auto".into(),
            segments: segs,
        })
    } else {
        None
    };
    let t = match t {
        Some(t) => Some(t),
        None if ep.transcript_type.as_deref() == Some("podscribe") && ep.transcript_url.is_some() => {
            let segs = qiaomu::podscribe_transcript(ep.transcript_url.as_ref().unwrap()).await?;
            (!segs.is_empty()).then(|| Transcript {
                episode_id: ep.id.clone(),
                source: "podscribe".into(),
                language: "en".into(),
                segments: transcript::resegment(segs),
            })
        }
        None if ep.transcript_type.as_deref() == Some("qiaomu") => match &ep.link {
            Some(link) => {
                let d = qiaomu::detail(link).await?;
                d.transcript.map(|text| Transcript {
                    episode_id: ep.id.clone(),
                    source: "qiaomu".into(),
                    language: "auto".into(),
                    segments: qiaomu::transcript_segments(&text, ep.duration.unwrap_or(3600.0)),
                })
            }
            None => None,
        },
        None => None,
    };
    if let Some(t) = &t {
        if t.segments.is_empty() {
            return Ok(None);
        }
        db::save_transcript(&ctx.db.lock(), t)?;
    }
    Ok(t)
}

pub async fn qiaomu_detail(ctx: &CoreCtx, episode_id: &str) -> Result<qiaomu::Detail> {
    let ep = db::get_episode(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    let d = if matches!(ep.transcript_type.as_deref(), Some("qiaomu-rss") | Some("podscribe")) {
        qiaomu::Detail { audio: None, deep_read: qiaomu::rss_rewrite(&ep.guid).await?, transcript: None }
    } else {
        let link = ep.link.clone().ok_or_else(|| anyhow!("这集没有网页"))?;
        qiaomu::detail(&link).await?
    };
    let conn = ctx.db.lock();
    if let (Some(a), None) = (&d.audio, &ep.audio_url) {
        let _ = db::set_audio_url(&conn, &ep.id, a);
    }
    if let Some(h) = &d.deep_read {
        let _ = db::set_blob(&conn, &ep.id, "deep-read", h);
    }
    Ok(qiaomu::Detail { transcript: None, ..d })
}

pub async fn learn_clip(ctx: &CoreCtx, episode_id: &str, start: f64, end: f64) -> Result<String> {
    let data = &ctx.data;
    let done = clip::path_for(data, episode_id, start, end);
    if done.exists() {
        return Ok(done.to_string_lossy().into_owned());
    }
    let ep = db::get_episode(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    if let Some(saved) = offline::find(data, episode_id).or_else(|| offline::cached(data, episode_id)) {
        let map = db::get_blob(&ctx.db.lock(), episode_id, "tmap")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<align::TimeMap>(&s).ok())
            .filter(|m| m.play_bytes == saved.bytes);
        let (a, b) = map.map(|m| (m.to_play(start), m.to_play(end))).unwrap_or((start, end));
        return clip::make(data, clip::Source::File(saved.path.into()), episode_id, a, b)
            .await
            .map(|p| p.to_string_lossy().into_owned());
    }
    if let Some(url) = ep.audio_url.filter(|u| !u.is_empty()) {
        if Path::new(&url).is_file() {
            return clip::make(data, clip::Source::File(PathBuf::from(url)), episode_id, start, end)
                .await
                .map(|p| p.to_string_lossy().into_owned());
        }
        return clip::make(data, clip::Source::Url(url), episode_id, start, end)
            .await
            .map(|p| p.to_string_lossy().into_owned());
    }
    let vid = ep.video_id.ok_or_else(|| anyhow!("这一集没有音频"))?;
    asr::streams::init(data);
    for fresh in [false, true] {
        let v = vid.clone();
        let url = asr::with_ytdlp(data, |bin| {
            let v = v.clone();
            async move { asr::youtube_stream_url(&bin, &v, false, fresh).await }
        })
        .await?;
        match clip::make(data, clip::Source::Url(url), episode_id, start, end).await {
            Ok(p) => return Ok(p.to_string_lossy().into_owned()),
            Err(_) if !fresh => {}
            Err(err) => return Err(err),
        }
    }
    Err(anyhow!("没能截取原声"))
}

pub async fn video_clip_prepare(
    ctx: &CoreCtx,
    job_id: &str,
    video_id: &str,
    start: f64,
    end: f64,
    ranges: Option<Vec<(f64, f64)>>,
    sink: &dyn ProgressSink,
) -> Result<videoclip::PreparedClip> {
    let cancel = ctx.job(job_id);
    let jid = job_id.to_string();
    let report = |stage: &str, progress: f64, message: &str| {
        sink.emit(ProgressEvent::VideoClip {
            job_id: jid.clone(),
            stage: stage.to_string(),
            progress,
            message: message.to_string(),
        });
    };
    let res = match ranges.filter(|r| r.len() > 1) {
        Some(r) => videoclip::prepare_montage(&ctx.data, video_id, &r, cancel.clone(), report).await,
        None => videoclip::prepare_segment(&ctx.data, video_id, start, end, cancel.clone(), report).await,
    };
    ctx.done_with(job_id, &cancel);
    res
}

pub async fn video_clip_render(
    ctx: &CoreCtx,
    job_id: &str,
    req: videoclip::RenderRequest,
    sink: &dyn ProgressSink,
) -> Result<videoclip::RenderResult> {
    let cancel = ctx.job(job_id);
    let jid = job_id.to_string();
    let res = videoclip::render_clip(&ctx.data, req, cancel.clone(), |stage, progress, message| {
        sink.emit(ProgressEvent::VideoClip {
            job_id: jid.clone(),
            stage: stage.to_string(),
            progress,
            message: message.to_string(),
        });
    })
    .await;
    ctx.done_with(job_id, &cancel);
    res
}

pub async fn youtube_stream(
    ctx: &CoreCtx,
    video_id: &str,
    mode: Option<&str>,
    fresh: bool,
    sink: &dyn ProgressSink,
) -> Result<VideoStream> {
    asr::streams::init(&ctx.data);
    let mode = mode.unwrap_or(if cfg!(target_os = "macos") { "native-hls" } else { "proxy-hls" });
    let hls = mode != "mp4";
    let emit = |stage: &str, progress: f64| {
        sink.emit(ProgressEvent::YtStream {
            video_id: video_id.to_string(),
            stage: stage.to_string(),
            progress,
        });
    };
    asr::ytdlp_with(&ctx.data, |got, total| {
        emit("tool", total.map_or(0.0, |t| got as f64 / t.max(1) as f64));
    })
    .await?;
    emit("resolve", 0.0);
    let lock = stream_lock(video_id);
    // Bounded: the player gives up after 40 s and tries something else; past this the lookup (and its yt-dlp,
    // killed when dropped) stops too instead of holding the network for minutes.
    let url = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        let _one = lock.lock().await;
        asr::with_ytdlp(&ctx.data, |bin| {
            let v = video_id.to_string();
            async move { asr::youtube_stream_url(&bin, &v, hls, fresh).await }
        })
        .await
    })
    .await
    .map_err(|_| anyhow!("获取视频地址超时，请检查网络或代理"))??;
    let duration = asr::video_duration(video_id);
    if let Some(d) = duration { let _ = db::fill_video_duration(&ctx.db.lock(), video_id, d); }
    let is_hls = url.contains("m3u8") || url.contains("/manifest/hls");
    if is_hls && mode == "proxy-hls" {
        return Ok(VideoStream { url: hlsproxy::proxied(&url).await?, kind: "hls", duration });
    }
    if is_hls {
        return Ok(VideoStream { url: hlsproxy::master_only(&url).await?, kind: "hls", duration });
    }
    Ok(VideoStream { url, kind: "mp4", duration })
}

pub async fn prepare_youtube(ctx: &CoreCtx) -> Result<()> {
    let bin = asr::ytdlp(&ctx.data).await?;
    asr::warm_ytdlp(&bin).await;
    Ok(())
}

pub async fn download_episode(ctx: &CoreCtx, episode_id: &str, sink: &dyn ProgressSink) -> Result<offline::Saved> {
    if let Some(s) = offline::find(&ctx.data, episode_id) {
        return Ok(s);
    }
    let ep = db::get_episode(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    let job = format!("dl:{episode_id}");
    let cancel = ctx.job(&job);
    let result = offline::download(&ctx.data, &ep, cancel.clone(), |p| {
        sink.emit(ProgressEvent::Download {
            episode_id: episode_id.to_string(),
            progress: p,
        });
    })
    .await;
    ctx.done_with(&job, &cancel);
    result
}

pub async fn full_notes(ctx: &CoreCtx, episode_id: &str) -> Result<Option<String>> {
    if let Some(cached) = db::get_blob(&ctx.db.lock(), episode_id, "notes-full")? {
        return Ok(Some(cached).filter(|s| !s.is_empty()));
    }
    let ep = db::get_episode(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    let have = ep.notes.as_deref().map(|n| n.chars().count()).unwrap_or(0);
    let mut best: Option<String> = None;
    let mut reached = false;
    if let Some(v) = &ep.video_id {
        if let Ok(info) = youtube::video_info(v).await {
            reached = true;
            if !info.description.trim().is_empty() {
                best = Some(info.description);
            }
        }
    }
    if let Some(link) = ep.link.as_deref().filter(|l| l.contains("xiaoyuzhoufm.com/episode/")) {
        if let Ok(x) = feeds::xiaoyuzhou_episode(link).await {
            reached = true;
            if let Some(n) = x.notes {
                if best.as_ref().map_or(true, |b| n.chars().count() > b.chars().count()) {
                    best = Some(n);
                }
            }
        }
    }
    let out = best.filter(|b| b.chars().count() > have + 40);
    if out.is_none() && !reached {
        return Ok(None);
    }
    db::set_blob(&ctx.db.lock(), episode_id, "notes-full", out.as_deref().unwrap_or(""))?;
    Ok(out)
}

fn dir_bytes(p: &Path) -> u64 {
    std::fs::read_dir(p)
        .map(|rd| {
            rd.flatten()
                .map(|f| match f.metadata() {
                    Ok(m) if m.is_dir() => dir_bytes(&f.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

pub fn storage_info(ctx: &CoreCtx) -> Value {
    let d = &ctx.data;
    let library: u64 = ["subcast.db", "subcast.db-wal", "subcast.db-shm"]
        .iter()
        .map(|f| std::fs::metadata(d.join(f)).map(|m| m.len()).unwrap_or(0))
        .sum();
    json!({
        "downloads": dir_bytes(&offline::dir(d)),
        "models": dir_bytes(&asr::models_dir(d)),
        "library": library,
        "temp": dir_bytes(&d.join("work")),
        "audioCache": dir_bytes(&offline::cache_dir(d)),
        "ttsCache": dir_bytes(&speech::cache_dir(d)),
        "ttsModels": dir_bytes(&local_tts::models_dir(d)),
    })
}

pub fn clear_temp(ctx: &CoreCtx) -> Result<()> {
    let dir = ctx.data.join("work");
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    Ok(())
}

pub async fn take_shared(ctx: &CoreCtx, ep: &Episode) -> Option<Transcript> {
    let mut hit = shared::lookup(&shared::keys(ep)).await?;
    let translation = hit.translation.take();
    let audio_ref = hit.audio.take().map(|a| align::AudioRef {
        bytes: a.bytes,
        etag: a.etag,
        secs: a.secs,
        fp: None,
        shared_id: Some(hit.id.clone()),
        has_fp: a.fp,
    });
    let t = shared::into_transcript(&ep.id, hit);
    db::save_transcript(&ctx.db.lock(), &t).ok()?;
    if let Some(r) = audio_ref {
        let _ = db::set_blob(
            &ctx.db.lock(),
            &ep.id,
            "audio-ref",
            &serde_json::to_string(&r).unwrap_or_default(),
        );
    }
    if let Some(tr) = translation.filter(|tr| tr.items.len() == t.segments.len() && tr.lang.starts_with("zh")) {
        let conn = ctx.db.lock();
        let _ = db::set_blob(&conn, &ep.id, "zh", &serde_json::to_string(&tr.items).unwrap_or_default());
        let _ = db::set_blob(&conn, &ep.id, "zh-engine", "shared");
    }
    Some(t)
}

pub fn audio_ref(ctx: &CoreCtx, episode_id: &str) -> Option<align::AudioRef> {
    db::get_blob(&ctx.db.lock(), episode_id, "audio-ref")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<align::AudioRef>(&s).ok())
        .filter(|r| r.fp.is_some())
}

pub async fn align_audio(
    ctx: &CoreCtx,
    episode_id: &str,
    force: bool,
    model: Option<&str>,
    sink: &dyn ProgressSink,
) -> Result<Aligned> {
    let (ep, aref, saved) = {
        let conn = ctx.db.lock();
        let ep = db::get_episode(&conn, episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
        let aref = db::get_blob(&conn, episode_id, "audio-ref")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<align::AudioRef>(&s).ok());
        let saved = db::get_blob(&conn, episode_id, "tmap")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<align::TimeMap>(&s).ok());
        (ep, aref, saved)
    };
    let local = offline::find(&ctx.data, episode_id).or_else(|| offline::cached(&ctx.data, episode_id));
    let local_bytes = local.as_ref().map(|l| l.bytes).unwrap_or(0);
    let none = |status| Ok(Aligned { status, map: None, path: None, local_bytes });
    let (Some(url), None) = (ep.audio_url.clone(), &ep.video_id) else {
        return none("same");
    };
    if let Some(m) = saved {
        let fits = m.play_bytes == local_bytes || (m.method == "same" && local.is_none());
        if fits && m.method != "same" && !(force && m.method != "manual") {
            return Ok(Aligned { status: "aligned", map: Some(m), path: local.map(|l| l.path), local_bytes });
        }
        if fits && m.method == "same" && !force {
            return none("same");
        }
    }
    let has_fp = aref.as_ref().is_some_and(|a| a.fp.is_some() || a.has_fp);
    if !has_fp && !force {
        return none("unknown");
    }
    let job = format!("al:{episode_id}");
    if ctx.running(&job) {
        return none("unknown");
    }
    let cancel = ctx.job(&job);
    let result: Result<Aligned> = async {
        let save = |m: &align::TimeMap| {
            let _ = db::set_blob(&ctx.db.lock(), episode_id, "tmap", &serde_json::to_string(m).unwrap_or_default());
        };
        if let Some(a) = &aref {
            let play_bytes = match &local {
                Some(l) => Some(l.bytes),
                None => align::remote_size(&url).await.map(|x| x.0),
            };
            if play_bytes == Some(a.bytes) {
                save(&align::TimeMap::same(a.bytes));
                return none("same");
            }
        }
        let path = match &local {
            Some(l) => PathBuf::from(&l.path),
            None => {
                let dest = offline::cache_path(&ctx.data, episode_id, &url);
                let part = dest.with_extension("part");
                let _ = tokio::fs::create_dir_all(offline::cache_dir(&ctx.data)).await;
                asr::download(&url, &part, &cancel, |got, total| {
                    sink.emit(ProgressEvent::Align {
                        episode_id: episode_id.to_string(),
                        progress: total.map(|t| got as f64 / t.max(1) as f64),
                    });
                })
                .await?;
                PathBuf::from(offline::keep_cached(&ctx.data, episode_id, &part)?.path)
            }
        };
        sink.emit(ProgressEvent::Align { episode_id: episode_id.to_string(), progress: None });
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let p2 = path.clone();
        let (secs, play_fp) = tokio::task::spawn_blocking(move || align::fingerprint_file(&p2)).await??;
        let (pieces, method) = if let Some(mut a) = aref.clone().filter(|_| has_fp) {
            let ref_fp = match a.fp.as_deref().and_then(align::decode_fp) {
                Some(fp) => fp,
                None => {
                    let fp = shared::fingerprint(a.shared_id.as_deref().unwrap_or(""))
                        .await
                        .ok_or_else(|| anyhow!("没有拿到字幕对应音频的指纹"))?;
                    a.fp = Some(align::encode_fp(&fp));
                    let _ = db::set_blob(
                        &ctx.db.lock(),
                        episode_id,
                        "audio-ref",
                        &serde_json::to_string(&a).unwrap_or_default(),
                    );
                    fp
                }
            };
            (align::map_fingerprints(&ref_fp, &play_fp)?, "fingerprint")
        } else {
            let t = db::get_transcript(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("没有字幕"))?;
            let model = model.ok_or_else(|| anyhow!("先在「语音识别」里下载一个本机模型，才能自动比对"))?;
            let mut anchors = Vec::new();
            let mut starts = align::probe_starts(secs, 120.0);
            for _round in 0..2 {
                if starts.is_empty() {
                    break;
                }
                let heard = listen_probes(ctx, &path, &starts, model, &t.language, cancel.clone()).await?;
                anchors.extend(align::probe_anchors(&t.segments, &heard, &starts));
                starts = align::refine_starts(&anchors);
            }
            (align::map_anchors(&anchors), "probe")
        };
        let shared_secs: f64 = pieces.iter().map(|p| p.ref_end - p.ref_start).sum();
        if pieces.is_empty() || (method == "fingerprint" && shared_secs < secs / 3.0) {
            return Ok(Aligned {
                status: "failed",
                map: None,
                path: Some(path.to_string_lossy().into_owned()),
                local_bytes: bytes,
            });
        }
        let map = align::TimeMap { play_bytes: bytes, method: method.into(), pieces };
        save(&map);
        Ok(Aligned {
            status: "aligned",
            map: Some(map),
            path: Some(path.to_string_lossy().into_owned()),
            local_bytes: bytes,
        })
    }
    .await;
    ctx.done_with(&job, &cancel);
    result
}

async fn listen_probes(
    ctx: &CoreCtx,
    path: &Path,
    starts: &[f64],
    model: &str,
    language: &str,
    cancel: Arc<AtomicBool>,
) -> Result<Vec<Segment>> {
    let spec = asr::resolve(&ctx.data, model).ok_or_else(|| anyhow!("未知模型"))?;
    if !spec.path.is_file() {
        return Err(anyhow!("先在「语音识别」里下载一个本机模型，才能自动比对"));
    }
    let work = ctx.data.join("work").join(format!("align-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir_all(&work).await?;
    let wav = work.join("probes.wav");
    let (p, st, w) = (path.to_path_buf(), starts.to_vec(), wav.clone());
    let made = tokio::task::spawn_blocking(move || -> Result<()> {
        let ext = p.extension().and_then(|x| x.to_str()).map(str::to_string);
        let slot = (align::SLOT_SECS * 16000.0) as usize;
        let mut all: Vec<f32> = Vec::with_capacity(slot * st.len());
        for s in st {
            let mut v = clip::cut(Box::new(std::fs::File::open(&p)?), ext.as_deref(), s, s + align::PROBE_SECS)
                .unwrap_or_default();
            v.resize(slot, 0.0);
            all.extend(v);
        }
        asr::write_wav(&w, &all)
    })
    .await?;
    let heard = match made {
        Err(err) => Err(err),
        Ok(()) if spec.engine == "qwen3" => match asr::find_crispasr(&ctx.data) {
            Some(bin) => asr::run_crispasr(&bin, &spec.path, &wav, language, &asr::AsrContext::default(), cancel, |_| {}).await,
            None => Err(anyhow!("Qwen3-ASR 识别程序还没准备好，先转写一集就会自动安装")),
        },
        Ok(()) => asr::run_worker(&wav, &spec.path, language, "", cancel, |_| {}).await.map(|r| r.segments),
    };
    let _ = tokio::fs::remove_dir_all(&work).await;
    heard
}

pub async fn shared_transcript(ctx: &CoreCtx, episode_id: &str) -> Result<Option<Transcript>> {
    let ep = db::get_episode(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    Ok(take_shared(ctx, &ep).await)
}

pub async fn shared_exists(ctx: &CoreCtx, episode_ids: &[String]) -> Result<Vec<String>> {
    let eps: Vec<Episode> = {
        let conn = ctx.db.lock();
        episode_ids.iter().take(200).filter_map(|id| db::get_episode(&conn, id).ok().flatten()).collect()
    };
    let lists: Vec<Vec<String>> = eps.iter().map(shared::keys).collect();
    let hits = shared::existing(&lists).await;
    Ok(eps.into_iter().zip(hits).filter(|(_, h)| *h).map(|(ep, _)| ep.id).collect())
}

fn emit_asr(sink: &dyn ProgressSink, id: &str, stage: &str, progress: f64, message: Option<&str>, segment: Option<Segment>) {
    sink.emit(ProgressEvent::Asr {
        episode_id: id.to_string(),
        stage: stage.to_string(),
        progress,
        message: message.map(str::to_string),
        segment,
    });
}

async fn run_asr_pipeline(
    ctx: &CoreCtx,
    ep: &Episode,
    audio: &Path,
    work: &Path,
    model: &str,
    language: &str,
    cloud: Option<&cloud::CloudAsr>,
    cancel: &Arc<AtomicBool>,
    sink: &dyn ProgressSink,
) -> Result<(Transcript, &'static str, String)> {
    let spec = asr::resolve(&ctx.data, model).ok_or_else(|| anyhow!("未知模型"))?;
    let model_path = spec.path.clone();
    if cloud.is_none() && !model_path.is_file() {
        return Err(anyhow!("请先下载识别模型"));
    }
    emit_asr(sink, &ep.id, "transcribe", 0.0, None, None);
    let use_notes = {
        let conn = ctx.db.lock();
        db::get_setting(&conn, "app")
            .ok()
            .flatten()
            .and_then(|r| serde_json::from_str::<Value>(&r).ok())
            .and_then(|s| s["useShowNotes"].as_bool())
            .unwrap_or(true)
    };
    let meta = if use_notes {
        asr_context::Meta {
            title: ep.title.clone(),
            show: ep.show_title.clone().unwrap_or_default(),
            description: ep.notes.clone().unwrap_or_default(),
            chapters: vec![],
        }
    } else {
        asr_context::Meta::default()
    };
    let hotwords: Vec<String> = if use_notes {
        db::get_blob(&ctx.db.lock(), &ep.id, "ctx")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .map(|v| v["terms"].as_array().into_iter().flatten().filter_map(|t| t["src"].as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    } else {
        vec![]
    };
    let lang_for = |text: &str| {
        if language == "auto" {
            asr_context::script_of(&format!("{} {}", ep.title, text))
        } else {
            language
        }
    };
    let fit = |text: String| {
        if asr_context::fits_language(&text, lang_for(&text))
            && (language != "auto" || asr_context::script_of(&ep.title) == asr_context::script_of(&text))
        {
            text
        } else {
            String::new()
        }
    };
    let asr_ctx = asr::AsrContext {
        prompt: fit(asr_context::prompt(&meta, asr_context::BUDGET_PROMPT)),
        background: fit(asr_context::background(&meta, asr_context::BUDGET_QWEN)),
        doubao: {
            let b = fit(asr_context::background(&meta, asr_context::BUDGET_DOUBAO));
            if b.is_empty() && hotwords.is_empty() {
                String::new()
            } else {
                asr_context::doubao(&asr_context::Meta { description: b, ..Default::default() }, &hotwords)
            }
        },
        hotwords,
    };

    let out = if let Some(cfg) = cloud {
        emit_asr(sink, &ep.id, "transcribe", 0.01, Some("正在解码音频"), None);
        let src = audio.to_path_buf();
        let samples = tokio::task::spawn_blocking(move || asr::decode_16k_mono(&src)).await??;
        let segments = cloud::transcribe(cfg, samples, language, &asr_ctx, cancel.clone(), |p| {
            emit_asr(sink, &ep.id, "transcribe", p, Some("云端识别中"), None);
        })
        .await?;
        let sample: String = segments.iter().take(5).map(|s| s.text.as_str()).collect();
        let lang = if language != "auto" {
            language.to_string()
        } else if sample.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
            "zh".into()
        } else {
            "auto".into()
        };
        asr::WorkerResult { language: lang, segments }
    } else if spec.engine == "qwen3" {
        let bin = match asr::find_crispasr(&ctx.data) {
            Some(p) => p,
            None => {
                emit_asr(sink, &ep.id, "download", 0.0, Some("正在准备 Qwen3-ASR 识别程序"), None);
                asr::install_crispasr(&ctx.data, cancel, |got, total| {
                    emit_asr(
                        sink,
                        &ep.id,
                        "download",
                        total.map(|t| got as f64 / t as f64).unwrap_or(0.0),
                        Some("正在准备 Qwen3-ASR 识别程序"),
                        None,
                    );
                })
                .await?
            }
        };
        emit_asr(sink, &ep.id, "transcribe", 0.01, Some("正在解码音频"), None);
        let (src, wav) = (audio.to_path_buf(), work.join("audio16k.wav"));
        let wav2 = wav.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            asr::write_wav(&wav2, &asr::decode_16k_mono(&src)?)
        })
        .await??;
        let segments = asr::run_crispasr(&bin, &model_path, &wav, language, &asr_ctx, cancel.clone(), |p| {
            emit_asr(sink, &ep.id, "transcribe", p, None, None);
        })
        .await?;
        let sample: String = segments.iter().take(5).map(|s| s.text.as_str()).collect();
        let lang = if language != "auto" {
            language.to_string()
        } else if sample.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)) {
            "zh".into()
        } else {
            "auto".into()
        };
        asr::WorkerResult { language: lang, segments }
    } else {
        asr::run_worker(audio, &model_path, language, &asr_ctx.prompt, cancel.clone(), |ev| match ev {
            asr::WorkerEvent::Progress(p) => emit_asr(sink, &ep.id, "transcribe", p, None, None),
            asr::WorkerEvent::Segment(s) => emit_asr(sink, &ep.id, "segment", 0.0, None, Some(s)),
        })
        .await?
    };

    let echo_of = format!("{} {} {}", asr_ctx.background, asr_ctx.prompt, asr_ctx.doubao);
    let heard: Vec<Segment> = out.segments.into_iter().filter(|s| !asr_context::is_echo(&s.text, &echo_of)).collect();
    let segments = transcript::resegment(transcript::clean(heard));
    if segments.is_empty() {
        return Err(anyhow!("没有识别出文字（可能是纯音乐或静音）"));
    }
    let source_kind = if cloud.is_some() { "cloud" } else { "whisper" };
    let engine_label = if cloud.is_some() { "cloud" } else if spec.engine == "qwen3" { "qwen3" } else { "whisper" };
    let model_name = cloud.as_ref().map(|c| c.model.clone()).unwrap_or_else(|| spec.id.to_string());
    let t = Transcript {
        episode_id: ep.id.clone(),
        source: source_kind.into(),
        language: out.language,
        segments,
    };
    db::save_transcript(&ctx.db.lock(), &t)?;
    let made_by = json!({ "source": engine_label, "model": model_name });
    let _ = db::set_blob(&ctx.db.lock(), &ep.id, "asr-meta", &made_by.to_string());
    Ok((t, engine_label, model_name))
}

pub async fn transcribe(
    ctx: &CoreCtx,
    episode_id: &str,
    model: &str,
    language: &str,
    cloud: Option<cloud::CloudAsr>,
    wait_share: bool,
    skip_shared: bool,
    sink: Arc<dyn ProgressSink>,
) -> Result<Transcript> {
    let ep = db::get_episode(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    let share_keys = shared::keys(&ep);
    if !skip_shared && (cloud.is_none() || !share_keys.is_empty()) {
        if let Some(t) = take_shared(ctx, &ep).await {
            emit_asr(sink.as_ref(), episode_id, "done", 1.0, None, None);
            return Ok(t);
        }
    }
    let spec = asr::resolve(&ctx.data, model).ok_or_else(|| anyhow!("未知模型"))?;
    if cloud.is_none() && !spec.path.is_file() {
        return Err(anyhow!("请先下载识别模型"));
    }
    if ctx.running(episode_id) {
        return Err(anyhow!("这一集正在转写"));
    }
    let cancel = ctx.job(episode_id);
    let work = ctx.data.join("work").join(format!("{episode_id}-{}", uuid::Uuid::new_v4().simple()));
    let result: Result<(Transcript, &'static str, String)> = async {
        let _ = tokio::fs::create_dir_all(&work).await;
        emit_asr(sink.as_ref(), episode_id, "download", 0.0, None, None);
        let audio = if let Some(saved) = offline::find(&ctx.data, episode_id) {
            PathBuf::from(saved.path)
        } else if let Some(vid) = &ep.video_id {
            if asr::find_ytdlp(&ctx.data).is_none() {
                emit_asr(sink.as_ref(), episode_id, "download", 0.0, Some("正在准备 YouTube 下载工具"), None);
            }
            asr::with_ytdlp(&ctx.data, |bin| {
                let (v, w, c) = (vid.clone(), work.clone(), cancel.clone());
                async move { asr::youtube_audio(&bin, &v, &w, c).await }
            })
            .await?
        } else {
            let url = ep.audio_url.clone().ok_or_else(|| anyhow!("这集没有音频地址"))?;
            if Path::new(&url).is_file() {
                PathBuf::from(url)
            } else {
                let dest = work.join(format!("audio.{}", offline::audio_ext(&url)));
                asr::download(&url, &dest, &cancel, |got, total| {
                    let p = total.map(|t| got as f64 / t as f64).unwrap_or(0.0);
                    emit_asr(sink.as_ref(), episode_id, "download", p, None, None);
                })
                .await?;
                dest
            }
        };
        let (t, engine_label, model_name) =
            run_asr_pipeline(ctx, &ep, &audio, &work, model, language, cloud.as_ref(), &cancel, sink.as_ref()).await?;
        if ep.video_id.is_none() {
            let src = audio.clone();
            if let Ok(Ok((secs, fp))) = tokio::task::spawn_blocking(move || align::fingerprint_file(&src)).await {
                let bytes = std::fs::metadata(&audio).map(|m| m.len()).unwrap_or(0);
                let r = align::AudioRef {
                    bytes,
                    etag: String::new(),
                    secs,
                    fp: Some(align::encode_fp(&fp)),
                    shared_id: None,
                    has_fp: true,
                };
                let _ = db::set_blob(&ctx.db.lock(), &ep.id, "audio-ref", &serde_json::to_string(&r).unwrap_or_default());
                let _ = db::set_blob(&ctx.db.lock(), &ep.id, "tmap", &serde_json::to_string(&align::TimeMap::same(bytes)).unwrap_or_default());
            }
            if audio.starts_with(&work) {
                let _ = offline::keep_cached(&ctx.data, &ep.id, &audio);
            }
        }
        Ok((t, engine_label, model_name))
    }
    .await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    ctx.done_with(episode_id, &cancel);

    match result {
        Ok((t, engine_label, model_name)) => {
            if !share_keys.is_empty() {
                let (share_on, install) = {
                    let conn = ctx.db.lock();
                    let on = db::get_setting(&conn, "app")
                        .ok()
                        .flatten()
                        .and_then(|r| serde_json::from_str::<Value>(&r).ok())
                        .and_then(|s| s["shareTranscripts"].as_bool())
                        .unwrap_or(true);
                    (on, install_id(&conn))
                };
                if share_on {
                    let last_end = t.segments.last().map(|s| s.end).unwrap_or(0.0);
                    let translation = {
                        let conn = ctx.db.lock();
                        let items: Vec<Option<String>> = t
                            .segments
                            .iter()
                            .map(|s| db::tr_get(&conn, &short_hash(&format!("zh-CN|{}", s.text), 24)).ok().flatten())
                            .collect();
                        let filled = items.iter().filter(|i| i.is_some()).count();
                        (filled * 5 >= items.len() * 3).then(|| ("zh-CN".to_string(), items))
                    };
                    let meta = shared::UploadMeta {
                        title: ep.title.clone(),
                        show: ep.show_title.clone().unwrap_or_default(),
                        duration: ep.duration.unwrap_or(0.0).max(last_end).max(1.0),
                        source: engine_label.into(),
                        model: model_name,
                        language: t.language.clone(),
                        install,
                        translation,
                        audio: audio_ref(ctx, &ep.id),
                    };
                    let segs = t.segments.clone();
                    if wait_share {
                        if let Ok(Ok(action)) =
                            tokio::time::timeout(std::time::Duration::from_secs(6), shared::upload(&share_keys, meta, &segs)).await
                        {
                            sink.emit(ProgressEvent::SharedUpload { action });
                        }
                    } else {
                        let sink2 = sink.clone();
                        tokio::spawn(async move {
                            if let Ok(action) = shared::upload(&share_keys, meta, &segs).await {
                                sink2.emit(ProgressEvent::SharedUpload { action });
                            }
                        });
                    }
                }
            }
            emit_asr(sink.as_ref(), episode_id, "done", 1.0, None, None);
            Ok(t)
        }
        Err(err) => {
            emit_asr(sink.as_ref(), episode_id, "error", 0.0, Some(&err.to_string()), None);
            Err(err)
        }
    }
}

/// Imports and transcribes a local audio/video file as a standalone `single` episode so translate/guide/ask/export all work on it.
pub async fn transcribe_local_file(
    ctx: &CoreCtx,
    file_path: &Path,
    model: &str,
    language: &str,
    cloud: Option<cloud::CloudAsr>,
    sink: &dyn ProgressSink,
) -> Result<(Episode, Transcript)> {
    let canonical = std::fs::canonicalize(file_path).with_context(|| format!("找不到本地文件: {}", file_path.display()))?; // i18n-ignore (CLI local file)
    if !canonical.is_file() {
        return Err(anyhow!("找不到本地文件")); // i18n-ignore
    }
    let path_str = canonical.to_string_lossy().into_owned();
    let title = canonical
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("Local Audio")
        .to_string();
    let ep_id = short_hash(&format!("local:{path_str}"), 20);
    let mut ep = Episode {
        id: ep_id.clone(),
        show_id: "single".into(),
        guid: format!("local:{path_str}"),
        title,
        published: Some(now_ms()),
        audio_url: Some(path_str),
        ..Default::default()
    };
    {
        let mut conn = ctx.db.lock();
        db::upsert_episodes(&mut conn, std::slice::from_ref(&ep))?;
    }
    let spec = asr::resolve(&ctx.data, model).ok_or_else(|| anyhow!("未知模型"))?;
    if cloud.is_none() && !spec.path.is_file() {
        return Err(anyhow!("请先下载识别模型"));
    }
    if ctx.running(&ep_id) {
        return Err(anyhow!("这一集正在转写"));
    }
    let cancel = ctx.job(&ep_id);
    let work = ctx.data.join("work").join(format!("{ep_id}-{}", uuid::Uuid::new_v4().simple()));
    let _ = tokio::fs::create_dir_all(&work).await;
    let res = run_asr_pipeline(ctx, &ep, &canonical, &work, model, language, cloud.as_ref(), &cancel, sink).await;
    let _ = tokio::fs::remove_dir_all(&work).await;
    ctx.done_with(&ep_id, &cancel);
    match res {
        Ok((t, _, _)) => {
            if let Some(last) = t.segments.last() {
                ep.duration = Some(last.end);
                let mut conn = ctx.db.lock();
                let _ = db::upsert_episodes(&mut conn, std::slice::from_ref(&ep));
            }
            emit_asr(sink, &ep_id, "done", 1.0, None, None);
            Ok((db::get_episode(&ctx.db.lock(), &ep_id)?.unwrap_or(ep), t))
        }
        Err(err) => {
            emit_asr(sink, &ep_id, "error", 0.0, Some(&err.to_string()), None);
            Err(err)
        }
    }
}

pub async fn share_translation(ctx: &CoreCtx, episode_id: &str, engine: &str, model: &str) -> Result<String> {
    let (ep, t, items, made_by, share_on, install) = {
        let conn = ctx.db.lock();
        let ep = db::get_episode(&conn, episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
        let t = db::get_transcript(&conn, episode_id)?.ok_or_else(|| anyhow!("没有字幕"))?;
        let items: Vec<Option<String>> = db::get_blob(&conn, episode_id, "zh")?
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let made_by = db::get_blob(&conn, episode_id, "asr-meta")
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok());
        let on = db::get_setting(&conn, "app")
            .ok()
            .flatten()
            .and_then(|r| serde_json::from_str::<Value>(&r).ok())
            .and_then(|s| s["shareTranscripts"].as_bool())
            .unwrap_or(true);
        (ep, t, items, made_by, on, install_id(&conn))
    };
    if !share_on || !matches!(engine, "llm" | "google") {
        return Ok("skipped".into());
    }
    let (source, asr_model) = match (t.source.as_str(), &made_by) {
        ("whisper" | "cloud", Some(m)) => (
            m["source"].as_str().unwrap_or("whisper").to_string(),
            m["model"].as_str().unwrap_or("").to_string(),
        ),
        (s, _) if s.starts_with("shared:") => (s.trim_start_matches("shared:").to_string(), String::new()),
        _ => return Ok("skipped".into()),
    };
    if !matches!(source.as_str(), "whisper" | "qwen3" | "cloud") || items.len() != t.segments.len() {
        return Ok("skipped".into());
    }
    let filled = items.iter().filter(|i| i.as_ref().is_some_and(|s| !s.is_empty())).count();
    if filled * 10 < items.len() * 9 {
        return Ok("skipped".into());
    }
    let keys = shared::keys(&ep);
    if keys.is_empty() {
        return Ok("skipped".into());
    }
    let meta = shared::UploadMeta {
        title: ep.title.clone(),
        show: ep.show_title.clone().unwrap_or_default(),
        duration: ep.duration.unwrap_or(0.0).max(t.segments.last().map(|s| s.end).unwrap_or(0.0)).max(1.0),
        source,
        model: asr_model,
        language: t.language.clone(),
        install,
        translation: Some(("zh-CN".to_string(), items)),
        audio: audio_ref(ctx, episode_id),
    };
    shared::upload_with(&keys, meta, &t.segments, engine, model).await
}

pub async fn shared_guide(ctx: &CoreCtx, episode_id: &str) -> Result<Option<String>> {
    let ep = db::get_episode(&ctx.db.lock(), episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
    let Some(text) = shared::lookup_guide(&shared::keys(&ep)).await else { return Ok(None) };
    let conn = ctx.db.lock();
    let _ = db::set_blob(&conn, episode_id, "summary", &text);
    let _ = db::set_blob(&conn, episode_id, "summary-from", "shared");
    Ok(Some(text))
}

pub async fn share_guide(ctx: &CoreCtx, episode_id: &str, text: &str, model: &str) -> Result<String> {
    let (ep, on, install) = {
        let conn = ctx.db.lock();
        let ep = db::get_episode(&conn, episode_id)?.ok_or_else(|| anyhow!("节目不存在"))?;
        let on = db::get_setting(&conn, "app")
            .ok()
            .flatten()
            .and_then(|r| serde_json::from_str::<Value>(&r).ok())
            .and_then(|s| s["shareTranscripts"].as_bool())
            .unwrap_or(true);
        let _ = db::set_blob(&conn, episode_id, "summary-from", "");
        (ep, on, install_id(&conn))
    };
    let keys = shared::keys(&ep);
    if !on || keys.is_empty() {
        return Ok("skipped".into());
    }
    shared::upload_guide(&keys, &ep.title, ep.show_title.as_deref().unwrap_or(""), model, text, install).await
}

pub fn install_id(conn: &Connection) -> String {
    if let Ok(Some(id)) = db::get_setting(conn, "install") {
        return id;
    }
    let id = uuid::Uuid::new_v4().to_string();
    let _ = db::set_setting(conn, "install", &id);
    id
}

pub async fn download_model(ctx: &CoreCtx, id: &str, mirror_first: bool, sink: &dyn ProgressSink) -> Result<()> {
    let job = format!("model:{id}");
    let cancel = ctx.job(&job);
    let r = asr::download_model(&ctx.data, id, &cancel, mirror_first, |got, total| {
        sink.emit(ProgressEvent::ModelDownload {
            id: id.to_string(),
            received: got,
            total,
        });
    })
    .await;
    ctx.done_with(&job, &cancel);
    r
}

pub fn delete_model(ctx: &CoreCtx, id: &str) -> Result<()> {
    let r = asr::resolve(&ctx.data, id).ok_or_else(|| anyhow!("未知模型"))?;
    let _ = std::fs::remove_file(&r.path);
    Ok(())
}

pub async fn local_tts_install(ctx: &CoreCtx, id: &str, mirror_first: bool, sink: &dyn ProgressSink) -> Result<()> {
    let job = format!("local-tts:{id}");
    let cancel = ctx.job(&job);
    let r = local_tts::install(&ctx.data, id, cancel.clone(), mirror_first, |got, total| {
        sink.emit(ProgressEvent::LocalTtsDownload {
            id: id.to_string(),
            received: got,
            total,
        });
    })
    .await;
    ctx.done_with(&job, &cancel);
    r
}

pub async fn llm_chat(
    ctx: &CoreCtx,
    request_id: &str,
    provider: &llm::Provider,
    system: &str,
    messages: &[llm::Message],
    max_tokens: Option<u32>,
    json_mode: Option<bool>,
    quick: Option<bool>,
    sink: &dyn ProgressSink,
) -> Result<String> {
    let cancel = ctx.job(request_id);
    let rid = request_id.to_string();
    let r = if provider.protocol == "cli" {
        let kind = agents::Kind::parse(provider.id.trim_start_matches("cli-")).ok_or_else(|| anyhow!("未知的本机 AI"))?;
        let path = agents::path_of(kind)
            .ok_or_else(|| anyhow!("没有找到 {}，请在设置里点「重新检测」", agents::display(kind)))?;
        let user = flatten(messages);
        agents::complete(
            kind,
            &path,
            agents::Request {
                system,
                user: &user,
                model: Some(provider.model.as_str()).filter(|m| !m.is_empty() && *m != "default"),
                json: json_mode.unwrap_or(false),
            },
            cancel.clone(),
            |d| {
                sink.emit(ProgressEvent::LlmDelta {
                    request_id: rid.clone(),
                    delta: d.to_string(),
                });
            },
        )
        .await
    } else {
        llm::chat(
            provider,
            system,
            messages,
            max_tokens.unwrap_or(8000),
            json_mode.unwrap_or(false),
            quick.unwrap_or(false),
            &cancel,
            |d| {
                sink.emit(ProgressEvent::LlmDelta {
                    request_id: rid.clone(),
                    delta: d.to_string(),
                });
            },
        )
        .await
    };
    ctx.done_with(request_id, &cancel);
    r
}

fn flatten(messages: &[llm::Message]) -> String {
    if messages.len() == 1 {
        return messages[0].content.clone();
    }
    let mut out = String::from("以下是到目前为止的对话，请回答最后一个问题。\n\n"); // i18n-ignore (sent to the model)
    for m in messages {
        out.push_str(if m.role == "assistant" { "【助手】\n" } else { "【用户】\n" }); // i18n-ignore
        out.push_str(&m.content);
        out.push_str("\n\n");
    }
    out
}

pub async fn translate_texts(ctx: &CoreCtx, texts: &[String], target: Option<&str>) -> Result<Vec<String>> {
    let tl = target.unwrap_or("zh-CN");
    let keys: Vec<String> = texts.iter().map(|t| short_hash(&format!("{tl}|{t}"), 24)).collect();
    let mut out: Vec<Option<String>> = {
        let conn = ctx.db.lock();
        keys.iter().map(|k| db::tr_get(&conn, k).ok().flatten()).collect()
    };
    let missing: Vec<usize> = (0..texts.len()).filter(|&i| out[i].is_none() && !texts[i].trim().is_empty()).collect();
    if !missing.is_empty() {
        let lines: Vec<String> = missing.iter().map(|&i| texts[i].clone()).collect();
        let done = translate::translate_lines(&lines, tl).await?;
        let conn = ctx.db.lock();
        let tx = conn.unchecked_transaction()?;
        for (j, &i) in missing.iter().enumerate() {
            if let Some(t) = done.get(j) {
                let _ = db::tr_put(&tx, &keys[i], t);
                out[i] = Some(t.clone());
            }
        }
        tx.commit()?;
    }
    Ok(out.into_iter().map(Option::unwrap_or_default).collect())
}

pub fn export_opml_xml(ctx: &CoreCtx) -> Result<String> {
    let shows = db::list_shows(&ctx.db.lock())?;
    let list: Vec<(String, String)> = shows
        .into_iter()
        .filter(|s| s.feed_url.starts_with("http"))
        .map(|s| (s.title, s.feed_url))
        .collect();
    Ok(import::opml(&list))
}

pub fn export_opml(ctx: &CoreCtx, output_path: Option<&Path>) -> Result<String> {
    let xml = export_opml_xml(ctx)?;
    let path = match output_path {
        Some(p) => PathBuf::from(p),
        None => {
            let dir = dirs::download_dir().or_else(dirs::home_dir).ok_or_else(|| anyhow!("找不到下载文件夹"))?;
            dir.join(format!("SubCast-{}.opml", chrono::Local::now().format("%Y%m%d")))
        }
    };
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, xml)?;
    Ok(path.to_string_lossy().into())
}

pub fn get_settings(ctx: &CoreCtx) -> Result<Value> {
    let raw = db::get_setting(&ctx.db.lock(), "app")?;
    Ok(raw.and_then(|r| serde_json::from_str(&r).ok()).unwrap_or(Value::Null))
}

pub fn set_settings(ctx: &CoreCtx, settings: &Value) -> Result<()> {
    apply_runtime_settings(settings);
    db::set_setting(&ctx.db.lock(), "app", &settings.to_string())
}

pub async fn refresh_config_with<F>(ctx: &CoreCtx, pubkey: &str, verify: F) -> Result<Value>
where
    F: FnOnce(&str, &[u8], &str) -> Result<()>,
{
    let body = net::client().get(REMOTE_CONFIG_URL).send().await?.error_for_status()?.bytes().await?;
    let sig = net::get_text(&format!("{REMOTE_CONFIG_URL}.sig")).await?;
    verify(pubkey, &body, &sig)?;
    let v: Value = serde_json::from_slice(&body)?;
    apply_remote_config(&v);
    db::set_setting(&ctx.db.lock(), "remote-config", &v.to_string())?;
    Ok(v)
}

pub fn favorites_list(ctx: &CoreCtx) -> Result<Vec<Favorite>> {
    let raw = db::get_blob(&ctx.db.lock(), "_app", "favorites")?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

pub fn favorites_add(ctx: &CoreCtx, episode_id: &str) -> Result<Vec<Favorite>> {
    let mut conn = ctx.db.lock();
    let mut out = Vec::new();
    db::update_blob(&mut conn, "_app", "favorites", |cur| {
        let mut favs: Vec<Favorite> = cur.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        if !favs.iter().any(|f| f.id == episode_id) {
            favs.insert(0, Favorite { id: episode_id.to_string(), at: now_ms() });
        }
        out = favs.clone();
        Ok(serde_json::to_string(&favs)?)
    })?;
    Ok(out)
}

pub fn favorites_remove(ctx: &CoreCtx, episode_id: &str) -> Result<Vec<Favorite>> {
    let mut conn = ctx.db.lock();
    let mut out = Vec::new();
    db::update_blob(&mut conn, "_app", "favorites", |cur| {
        let mut favs: Vec<Favorite> = cur.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        favs.retain(|f| f.id != episode_id);
        out = favs.clone();
        Ok(serde_json::to_string(&favs)?)
    })?;
    Ok(out)
}

#[derive(Clone, Debug)]
pub struct ClipVideoOptions {
    pub episode_id: String,
    pub job_id: String,
    pub ranges: Vec<(f64, f64)>,
    pub preset: Option<String>,
    pub aspect: Option<String>,
    pub fit: Option<String>,
    pub card_crop: Option<String>,
    pub card_bg: Option<String>,
    pub subs_content: crate::subexport::SubContent,
    pub subs_below: Option<bool>,
    pub speed: Option<f64>,
    pub remove_pauses: Option<bool>,
    pub loudness: Option<bool>,
    pub cover_mode: Option<String>,
    pub cover_template: Option<String>,
    pub hook_title: Option<String>,
    pub sub_title: Option<String>,
    pub accent: Option<String>,
    pub crop_x: Option<f64>,
    pub embed_thumbnail: bool,
    pub save_cover_sidecar: bool,
    pub save_landscape_cover: bool,
    pub output_path: Option<String>,
    pub output_dir: Option<String>,
    pub show_brand: bool,
}

impl Default for ClipVideoOptions {
    fn default() -> Self {
        Self {
            episode_id: String::new(),
            job_id: "cli-clip".into(),
            ranges: Vec::new(),
            preset: Some("vertical".into()),
            aspect: None,
            fit: None,
            card_crop: None,
            card_bg: None,
            subs_content: crate::subexport::SubContent::Bilingual,
            subs_below: None,
            speed: None,
            remove_pauses: None,
            loudness: None,
            cover_mode: None,
            cover_template: None,
            hook_title: None,
            sub_title: None,
            accent: None,
            crop_x: None,
            embed_thumbnail: true,
            save_cover_sidecar: false,
            save_landscape_cover: false,
            output_path: None,
            output_dir: None,
            show_brand: true,
        }
    }
}

pub async fn clip_video_full(
    ctx: &CoreCtx,
    opts: ClipVideoOptions,
    sink: &dyn ProgressSink,
) -> Result<videoclip::RenderResult> {
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

    if opts.ranges.is_empty() {
        anyhow::bail!("请至少指定一个时间范围"); // i18n-ignore
    }
    let ep = db::get_episode(&ctx.db.lock(), &opts.episode_id)?
        .ok_or_else(|| anyhow!("节目不存在"))?;
    let show = db::get_show(&ctx.db.lock(), &ep.show_id)?;
    let video_id = ep
        .video_id
        .clone()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| anyhow!("这一集没有 YouTube 视频，无法剪辑视频"))?; // i18n-ignore

    let (start, end) = (opts.ranges[0].0, opts.ranges[0].1);
    let prepared = video_clip_prepare(
        ctx,
        &opts.job_id,
        &video_id,
        start,
        end,
        (opts.ranges.len() > 1).then(|| opts.ranges.clone()),
        sink,
    )
    .await?;

    let preset = opts
        .preset
        .as_deref()
        .and_then(crate::highlights::preset_by_id);
    let aspect = opts
        .aspect
        .as_deref()
        .and_then(crate::highlights::ClipAspect::parse)
        .or_else(|| preset.map(|p| p.aspect))
        .unwrap_or(crate::highlights::ClipAspect::V9x16);
    let fit = opts
        .fit
        .as_deref()
        .and_then(crate::highlights::ClipFit::parse)
        .or_else(|| preset.map(|p| p.fit))
        .unwrap_or(crate::highlights::ClipFit::Card);
    let card_crop = opts
        .card_crop
        .as_deref()
        .and_then(crate::highlights::CardCrop::parse)
        .or_else(|| preset.map(|p| p.card_crop))
        .unwrap_or(crate::highlights::CardCrop::C4x3);

    let subs_below = opts.subs_below.unwrap_or(fit == crate::highlights::ClipFit::Card);
    let card_bg = opts.card_bg.as_deref().unwrap_or("dark");
    let accent = opts.accent.as_deref().unwrap_or("#ffd23f");
    let speed = opts.speed.unwrap_or(1.0).clamp(0.5, 2.0);
    let remove_pauses = opts
        .remove_pauses
        .or_else(|| preset.map(|p| p.remove_pauses))
        .unwrap_or(false);
    let loudness = opts.loudness.unwrap_or(true);
    let cover_mode = opts
        .cover_mode
        .as_deref()
        .or_else(|| preset.map(|p| p.cover_mode))
        .unwrap_or("flash");

    let geom = crate::highlights::clip_geometry(
        aspect,
        fit,
        prepared.width,
        prepared.height,
        card_crop,
        subs_below,
    );

    // Standalone episodes sit in the internal "single" show, whose stored name (单独打开) is not a show name;
    // a previewed show has no row but its name comes with the episode.
    let show_name = match show.as_ref() {
        Some(s) if s.kind == "single" => String::new(),
        Some(s) => s.title.clone(),
        None => ep.show_title.clone().unwrap_or_default(),
    };
    let clip_title = opts
        .hook_title
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(&ep.title)
        .to_string();

    let (transcript_segs, zh) = {
        // The lock is let go first: load_episode_zh takes it again (holding it here hung every CLI clip).
        let tr = db::get_transcript(&ctx.db.lock(), &ep.id)?
            .map(|t| t.segments)
            .unwrap_or_default();
        (tr, crate::ai::load_episode_zh(ctx, &ep.id))
    };

    let max_cjk = if geom.vertical { 16 } else { 24 };
    let mut raw_cues: Vec<crate::subexport::Cue> = Vec::new();
    if !transcript_segs.is_empty() {
        let mut offset = 0.0;
        for &(rs, re) in &opts.ranges {
            let (si, ei) = crate::highlights::range_from_clock(&transcript_segs, rs, Some(re));
            let mut part = crate::highlights::build_clip_cues(
                &transcript_segs,
                zh.as_deref(),
                si,
                ei,
                rs,
                re,
                opts.subs_content,
                max_cjk,
            );
            for c in &mut part {
                c.start += offset;
                c.end += offset;
            }
            raw_cues.extend(part);
            offset += (re - rs).max(0.0);
        }
    }

    let (keep_ranges, cuts) = if remove_pauses && !prepared.silences.is_empty() {
        let pc = crate::highlights::pause_cuts(&prepared.silences, prepared.duration);
        (pc.keep, pc.cuts)
    } else {
        (Vec::new(), Vec::new())
    };

    for c in &mut raw_cues {
        if !cuts.is_empty() {
            c.start = crate::highlights::to_kept_time(c.start, &cuts);
            c.end = crate::highlights::to_kept_time(c.end, &cuts);
        }
        c.start /= speed;
        c.end = (c.end / speed).max(c.start + 0.08);
    }

    let overlay_bytes = crate::render::render_static_overlay_png(&crate::render::StaticOverlayOpts {
        geom,
        show_name: show_name.clone(),
        title: clip_title.clone(),
        card_bg: card_bg.to_string(),
        accent: accent.to_string(),
        show_brand: opts.show_brand,
        brand_text: "SubCast".into(),
    })?;
    let cue_overlays = crate::render::render_cue_overlays(
        &geom,
        &raw_cues,
        subs_below,
        card_bg,
        accent,
    )?;

    let backdrop = prepared.frames.get(prepared.best_frame).cloned();
    let cover_png = if cover_mode != "none" || opts.embed_thumbnail || opts.save_cover_sidecar {
        let bytes = crate::render::render_cover_png(&crate::render::CoverOpts {
            template: opts
                .cover_template
                .clone()
                .unwrap_or_else(|| "impact".into()),
            width: geom.w,
            height: geom.h,
            hook_title: clip_title.clone(),
            sub_title: opts.sub_title.clone().unwrap_or_default(),
            kicker: show_name.clone(),
            show_name: show_name.clone(),
            brand: "SubCast".into(),
            duration_sec: prepared.duration / speed,
            accent: accent.to_string(),
            backdrop_path: backdrop.clone(),
            crop_bottom: if prepared.has_burned_subtitles { 0.14 } else { 0.0 },
            ..Default::default()
        })?;
        Some(B64.encode(bytes))
    } else {
        None
    };

    let landscape_cover_png = if opts.save_landscape_cover {
        let bytes = crate::render::render_cover_png(&crate::render::CoverOpts {
            template: opts
                .cover_template
                .clone()
                .unwrap_or_else(|| "impact".into()),
            width: 1920,
            height: 1080,
            hook_title: clip_title.clone(),
            sub_title: opts.sub_title.clone().unwrap_or_default(),
            kicker: show_name.clone(),
            show_name: show_name.clone(),
            brand: "SubCast".into(),
            duration_sec: prepared.duration / speed,
            accent: accent.to_string(),
            backdrop_path: backdrop,
            crop_bottom: if prepared.has_burned_subtitles { 0.14 } else { 0.0 },
            ..Default::default()
        })?;
        Some(B64.encode(bytes))
    } else {
        None
    };

    let crop_x = opts
        .crop_x
        .or_else(|| prepared.subject_track.first().map(|p| p.1))
        .unwrap_or(0.5);
    let pan = if opts.crop_x.is_some() {
        Vec::new()
    } else {
        prepared.subject_track
    };

    let layout = if geom.fit == crate::highlights::ClipFit::Fill {
        if geom.vertical { "portrait_fill" } else { "landscape" }
    } else {
        "portrait_card"
    };

    let frame = videoclip::Frame {
        w: geom.w,
        h: geom.h,
        fit: match geom.fit {
            crate::highlights::ClipFit::Card => "card".into(),
            crate::highlights::ClipFit::Fill => "fill".into(),
        },
        vx: geom.video.x,
        vy: geom.video.y,
        vw: geom.video.w,
        vh: geom.video.h,
        crop_box: geom.crop_box,
    };

    let default_name = crate::subexport::safe_filename(&clip_title, "mp4");
    let req = videoclip::RenderRequest {
        frame: Some(frame),
        lead: prepared.lead,
        speed,
        output_dir: opts.output_dir,
        raw_path: prepared.path,
        output_path: opts.output_path,
        default_name,
        layout: layout.into(),
        card_bg: card_bg.to_string(),
        crop_x,
        duration: prepared.duration,
        static_overlay_png: Some(B64.encode(overlay_bytes)),
        cues: cue_overlays,
        cover_png,
        cover_mode: cover_mode.to_string(),
        embed_thumbnail: opts.embed_thumbnail,
        save_cover_sidecar: opts.save_cover_sidecar,
        landscape_cover_png,
        keep_ranges,
        loudness,
        pan,
    };

    video_clip_render(ctx, &opts.job_id, req, sink).await
}
