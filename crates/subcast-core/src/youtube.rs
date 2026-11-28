//! YouTube captions without the viewer's cookies: ask InnerTube's player endpoint as a mobile client
//! (the same route as youtube-transcript-api and yt-dlp, ported from qiaomu-clipper), then fetch the chosen track.
use crate::model::Segment;
use crate::net;
use crate::transcript;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

/// Overrides from the remote config (`youtubeClients`), so a YouTube change can be answered without a release.
static REMOTE_CLIENTS: parking_lot::RwLock<Vec<(String, String, Option<u32>)>> = parking_lot::RwLock::new(Vec::new());

/// `[{"name": "ANDROID", "version": "20.10.38", "sdk": 34}, …]`; ignored when empty or malformed.
pub fn set_clients(v: &Value) {
    let list: Vec<(String, String, Option<u32>)> = v.as_array().into_iter().flatten().filter_map(|c| {
        Some((c["name"].as_str()?.to_string(), c["version"].as_str()?.to_string(), c["sdk"].as_u64().map(|n| n as u32)))
    }).collect();
    if !list.is_empty() { *REMOTE_CLIENTS.write() = list; }
}

fn clients() -> Vec<(String, String, Option<u32>)> {
    let remote = REMOTE_CLIENTS.read().clone();
    if !remote.is_empty() { return remote; }
    CLIENTS.iter().map(|(n, v, s)| (n.to_string(), v.to_string(), *s)).collect()
}

const CLIENTS: &[(&str, &str, Option<u32>)] = &[
    ("ANDROID", "20.10.38", Some(34)),
    ("IOS", "20.10.3", None),
    ("ANDROID_VR", "1.60.19", Some(32)),
];

#[derive(Clone, Debug)]
pub struct Track {
    pub base_url: String,
    pub language: String,
    pub auto: bool,
}

pub struct VideoInfo {
    pub title: String,
    pub author: String,
    pub duration: Option<f64>,
    pub description: String,
    pub tracks: Vec<Track>,
    /// The spoken language, when YouTube marks one audio track as the original (videos with dubbed audio).
    pub original_language: Option<String>,
    /// `UC…`, when the player response includes one. Used to subscribe from a video opened on its own.
    pub channel_id: Option<String>,
}

fn original_language(player: &Value) -> Option<String> {
    let mut found = std::collections::BTreeSet::new();
    for key in ["formats", "adaptiveFormats"] {
        for f in player["streamingData"][key].as_array().into_iter().flatten() {
            let a = &f["audioTrack"];
            if a["displayName"].as_str().is_some_and(|n| n.to_lowercase().contains("original")) {
                if let Some(id) = a["id"].as_str() { found.insert(base(id.split('.').next().unwrap_or(id))); }
            }
        }
    }
    (found.len() == 1).then(|| found.into_iter().next().unwrap())
}

fn base(lang: &str) -> String { lang.split(['-', '_']).next().unwrap_or(lang).to_lowercase() }

/// A YouTube channel id (`UC` plus 22 characters). Anything else is not a channel we can subscribe to.
pub fn channel_id_of(details: &Value) -> Option<String> {
    let id = details["channelId"].as_str()?;
    (id.starts_with("UC") && id.len() == 24 && id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')).then(|| id.to_string())
}

fn tracks_of(player: &Value) -> Vec<Track> {
    player["captions"]["playerCaptionsTracklistRenderer"]["captionTracks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let url = t["baseUrl"].as_str()?;
            // Token-gated tracks need the web player's proof; skip them.
            if url.contains("exp=xpe") { return None; }
            Some(Track { base_url: url.to_string(), language: t["languageCode"].as_str().unwrap_or("und").to_string(), auto: t["kind"].as_str() == Some("asr") })
        })
        .collect()
}

pub async fn video_info(video_id: &str) -> Result<VideoInfo> {
    let mut last_err = anyhow!("YouTube 没有返回视频信息");
    let mut fallback: Option<VideoInfo> = None;
    for (name, version, sdk) in clients() {
        let mut client = json!({ "clientName": name, "clientVersion": version, "hl": "en" });
        if let Some(sdk) = sdk { client["androidSdkVersion"] = json!(sdk); }
        let body = json!({ "context": { "client": client }, "videoId": video_id, "contentCheckOk": true, "racyCheckOk": true });
        let res = net::client().post("https://www.youtube.com/youtubei/v1/player?prettyPrint=false").json(&body).send().await;
        let player: Value = match res {
            Ok(r) if r.status().is_success() => match r.json().await { Ok(v) => v, Err(e) => { last_err = e.into(); continue; } },
            Ok(r) => { last_err = anyhow!("YouTube {}", r.status()); continue; }
            Err(e) => { last_err = anyhow!("连不上 YouTube（国内需要设置代理）：{e}"); continue; }
        };
        let d = &player["videoDetails"];
        let info = VideoInfo {
            title: d["title"].as_str().unwrap_or("").to_string(),
            author: d["author"].as_str().unwrap_or("").to_string(),
            duration: d["lengthSeconds"].as_str().and_then(|s| s.parse().ok()),
            description: d["shortDescription"].as_str().unwrap_or("").to_string(),
            tracks: tracks_of(&player),
            original_language: original_language(&player),
            channel_id: channel_id_of(d),
        };
        if !info.tracks.is_empty() { return Ok(info); }
        if fallback.is_none() && !info.title.is_empty() { fallback = Some(info); }
    }
    fallback.ok_or(last_err)
}

/// The track to show: a human-made one in the spoken language, else the auto track in that language.
/// Videos with dubbed audio carry one auto track per dub, so the spoken language comes from the audio track
/// YouTube marks "original"; with a single auto track, that track's language is the spoken one.
/// With no audio-track or single-auto-track evidence, the spoken language is guessed from `hint` (the title's
/// script, see `script_hint`): a Chinese show that also uploads English subtitles must not open in English.
pub fn choose<'a>(tracks: &'a [Track], original: Option<&str>, hint: Option<&str>) -> Option<&'a Track> {
    let autos: Vec<&Track> = tracks.iter().filter(|t| t.auto).collect();
    let spoken = original.map(base).or_else(|| (autos.len() == 1).then(|| base(&autos[0].language))).or_else(|| hint.map(base));
    if let Some(sp) = &spoken {
        if let Some(t) = tracks.iter().find(|t| !t.auto && &base(&t.language) == sp) { return Some(t); }
        if let Some(t) = autos.iter().find(|t| &base(&t.language) == sp) { return Some(t); }
    }
    tracks.iter().find(|t| !t.auto && base(&t.language) == "en")
        .or_else(|| autos.iter().copied().find(|t| base(&t.language) == "en"))
        .or_else(|| tracks.iter().find(|t| !t.auto))
        .or_else(|| tracks.first())
}

/// The language a title is most likely spoken in, from its script: kana means Japanese, hangul Korean, and
/// Han characters (without kana) Chinese. None for Latin and other scripts, which say little on their own.
pub fn script_hint(title: &str) -> Option<&'static str> {
    let has = |lo: char, hi: char| title.chars().any(|c| (lo..=hi).contains(&c));
    if has('\u{3040}', '\u{30ff}') { Some("ja") }
    else if has('\u{ac00}', '\u{d7af}') { Some("ko") }
    else if title.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count() >= 2 { Some("zh") }
    else { None }
}

/// The publisher's own Simplified Chinese subtitles, when the video is in another language and has them:
/// better than machine translation, so they become the Chinese line of the bilingual view.
pub fn official_chinese<'a>(tracks: &'a [Track], spoken: &str) -> Option<&'a Track> {
    if base(spoken) == "zh" { return None; }
    let zh: Vec<&Track> = tracks.iter().filter(|t| !t.auto && base(&t.language) == "zh").collect();
    zh.iter().find(|t| matches!(t.language.as_str(), "zh-Hans" | "zh-CN" | "zh-SG" | "zh")).or(zh.first()).copied()
}

/// Puts each line of another track under the original segment its midpoint falls in. None for segments
/// nothing lands in; the caller decides whether the coverage is good enough to use.
pub fn align(orig: &[Segment], other: &[Segment]) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = vec![None; orig.len()];
    for s in other {
        let mid = (s.start + s.end) / 2.0;
        let i = orig.partition_point(|o| o.start <= mid).saturating_sub(1);
        if orig.is_empty() || mid > orig[i].end + 2.0 { continue; }
        let text = s.text.trim();
        if text.is_empty() { continue; }
        match &mut out[i] { Some(prev) => prev.push_str(text), slot => *slot = Some(text.to_string()) }
    }
    out
}

pub async fn fetch_track(track: &Track) -> Result<Vec<Segment>> {
    let base = regex::Regex::new(r"&fmt=[^&]*").unwrap().replace_all(&track.base_url, "").to_string();
    for url in [format!("{base}&fmt=json3"), base.clone()] {
        if let Ok(r) = net::client().get(&url).send().await {
            if let Ok(body) = r.text().await {
                let segs = transcript::parse_youtube(&body);
                if !segs.is_empty() { return Ok(segs); }
            }
        }
    }
    Err(anyhow!("字幕下载失败"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn t(l: &str, auto: bool) -> Track { Track { base_url: String::new(), language: l.into(), auto } }
    #[test]
    fn channel_id_is_a_youtube_channel() {
        assert_eq!(channel_id_of(&json!({"channelId": "UCFDMMIHbtRdrVhHlGXVzApA"})).as_deref(), Some("UCFDMMIHbtRdrVhHlGXVzApA"));
        assert!(channel_id_of(&json!({"channelId": "not-a-channel"})).is_none());
        assert!(channel_id_of(&json!({})).is_none());
    }
    #[test]
    fn dubbed_video_picks_original() {
        let tracks = vec![t("ar", true), t("en", true), t("en-US", false), t("de", true)];
        assert_eq!(choose(&tracks, Some("en"), None).unwrap().language, "en-US");
        assert_eq!(choose(&tracks, None, None).unwrap().language, "en-US");
        let only_auto = vec![t("ar", true), t("en", true)];
        assert_eq!(choose(&only_auto, None, None).unwrap().language, "en");
        assert_eq!(choose(&[t("ja", true)], None, None).unwrap().language, "ja");
    }
    #[test]
    fn chinese_show_with_english_subtitles_opens_in_chinese() {
        // 词元之外 #010: uploaded zh-Hans and en, plus auto zh-Hans and en.
        let tracks = vec![t("en", false), t("zh-Hans", false), t("zh-Hans", true), t("en", true)];
        let hint = script_hint("#010 黄仁勋辞职，Fable 杀死比赛，语言即世界");
        assert_eq!(hint, Some("zh"));
        assert_eq!(choose(&tracks, None, hint).unwrap().language, "zh-Hans");
        assert!(official_chinese(&tracks, "zh-Hans").is_none());
        assert_eq!(script_hint("Recursive self-improvement has started"), None);
        assert_eq!(script_hint("ChatGPTの使い方"), Some("ja"));
    }
    #[test]
    fn english_video_takes_the_publishers_chinese() {
        let tracks = vec![t("en", false), t("zh-TW", false), t("zh-Hans", false), t("en", true)];
        assert_eq!(official_chinese(&tracks, "en").unwrap().language, "zh-Hans");
        let seg = |s: f64, e: f64, x: &str| Segment { start: s, end: e, text: x.into() };
        let orig = vec![seg(0.0, 4.0, "Hello there."), seg(4.0, 9.0, "How are you?"), seg(9.0, 12.0, "Fine.")];
        let zh = vec![seg(0.2, 2.0, "你好"), seg(2.0, 3.8, "呀。"), seg(4.5, 8.0, "你好吗？")];
        assert_eq!(align(&orig, &zh), vec![Some("你好呀。".into()), Some("你好吗？".into()), None]);
    }
}
