//! Shared subtitles (subcast.qiaomu.ai): before transcribing we ask whether someone else already
//! shared this episode's subtitles, which skips the whole download + ASR pass; after transcribing
//! we upload ours so later listeners skip it too. Reading is always on; uploading follows the
//! "shareTranscripts" setting (default on). Uploads are anonymous: a random install id, no account.
use crate::model::{Episode, Segment, Transcript};
use crate::net;
use serde::{Deserialize, Serialize};
use std::time::Duration;

const API: &str = "https://subcast.qiaomu.ai/api/v1/transcripts";

fn sha1_hex(s: &str) -> String {
    use sha1::Digest;
    hex::encode(sha1::Sha1::digest(s.as_bytes()))
}

/// 1–4 keys that identify the same episode across installs (any one match is enough server-side).
pub fn keys(ep: &Episode) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if let Some(v) = &ep.video_id { out.push(format!("yt:{v}")); }
    if matches!(ep.transcript_type.as_deref(), Some("qiaomu") | Some("qiaomu-rss") | Some("podscribe")) && !ep.guid.is_empty() {
        out.push(format!("qm:{}", ep.guid));
    }
    if !ep.guid.is_empty() { out.push(format!("guid:{}", sha1_hex(&ep.guid))); }
    if let Some(a) = &ep.audio_url { out.push(format!("audio:{}", sha1_hex(a))); }
    out.truncate(4);
    out
}

#[derive(Deserialize)]
pub struct SharedTranslation {
    pub lang: String,
    pub items: Vec<Option<String>>,
}

/// The audio file a shared version was made from (servers from 0.3.0 on).
#[derive(Deserialize, Default)]
pub struct SharedAudio {
    pub bytes: u64,
    #[serde(default)]
    pub etag: String,
    pub secs: f64,
    #[serde(default)]
    pub fp: bool,
}

#[derive(Deserialize)]
pub struct Shared {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub audio: Option<SharedAudio>,
    pub source: String,
    pub language: String,
    pub segments: Vec<Segment>,
    pub translation: Option<SharedTranslation>,
}

/// The best shared version for these keys, if the server has one. Network problems are silent:
/// they just mean "transcribe it yourself".
pub async fn lookup(keys: &[String]) -> Option<Shared> {
    if keys.is_empty() { return None; }
    let qs = keys.iter().map(|k| format!("key={}", urlencoding::encode(k))).collect::<Vec<_>>().join("&");
    let resp = net::client().get(format!("{API}?{qs}")).timeout(Duration::from_secs(8)).send().await.ok()?;
    if !resp.status().is_success() { return None; }
    let hit: Shared = resp.json().await.ok()?;
    (!hit.segments.is_empty()).then_some(hit)
}

/// Which of these key lists (one per episode) the shared library has subtitles for, by index. One
/// request for a whole list of episodes; an older server or a network problem just means "none known".
pub async fn existing(lists: &[Vec<String>]) -> Vec<bool> {
    let mut out = vec![false; lists.len()];
    let all: Vec<&String> = lists.iter().flatten().collect();
    for chunk in all.chunks(400) {
        let body = serde_json::json!({ "keys": chunk });
        let Ok(resp) = net::client().post(format!("{API}/exists")).json(&body).timeout(Duration::from_secs(8)).send().await else { break };
        if !resp.status().is_success() { break; }
        #[derive(Deserialize)]
        struct Found { found: Vec<String> }
        let Ok(f) = resp.json::<Found>().await else { break };
        // The server normalizes keys (lowercased hashes); ours are already in that form.
        for (i, keys) in lists.iter().enumerate() { if keys.iter().any(|k| f.found.contains(k)) { out[i] = true; } }
    }
    out
}

const GUIDES: &str = "https://subcast.qiaomu.ai/api/v1/guides";

/// Someone's 导读 for this episode (Markdown), if shared.
pub async fn lookup_guide(keys: &[String]) -> Option<String> {
    if keys.is_empty() { return None; }
    let qs = keys.iter().map(|k| format!("key={}", urlencoding::encode(k))).collect::<Vec<_>>().join("&");
    let resp = net::client().get(format!("{GUIDES}?{qs}")).timeout(Duration::from_secs(8)).send().await.ok()?;
    if !resp.status().is_success() { return None; }
    #[derive(Deserialize)]
    struct Hit { text: String }
    let hit: Hit = resp.json().await.ok()?;
    (!hit.text.trim().is_empty()).then_some(hit.text)
}

/// Shares a 导读 we made (anonymous, like transcripts). Returns the server's action.
pub async fn upload_guide(keys: &[String], title: &str, show: &str, model: &str, text: &str, install: String) -> anyhow::Result<String> {
    let body = serde_json::json!({ "keys": keys, "title": title, "show": show, "lang": "zh", "model": model, "text": text,
        "client": { "app": "subcast", "version": env!("CARGO_PKG_VERSION"), "install": install } });
    let resp = net::client().post(GUIDES).json(&body).timeout(Duration::from_secs(20)).send().await?;
    if !resp.status().is_success() { anyhow::bail!("shared guides: HTTP {}", resp.status()); }
    let v: serde_json::Value = resp.json().await?;
    Ok(v["action"].as_str().unwrap_or("").to_string())
}

#[derive(Serialize)]
struct UploadTranslation {
    lang: String,
    engine: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    model: String,
    items: Vec<Option<String>>,
}

#[derive(Serialize)]
struct ClientInfo {
    app: &'static str,
    version: &'static str,
    install: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UploadAudio<'a> {
    bytes: u64,
    etag: &'a str,
    secs: f64,
    fp_algo: &'static str,
    fp: &'a str,
}

#[derive(Serialize)]
struct Upload<'a> {
    keys: &'a [String],
    title: &'a str,
    show: &'a str,
    duration: f64,
    source: &'a str,
    model: &'a str,
    language: &'a str,
    segments: &'a [Segment],
    translation: Option<UploadTranslation>,
    client: ClientInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    audio: Option<UploadAudio<'a>>,
}

#[derive(Deserialize)]
struct UploadReply {
    action: Option<String>,
}

/// What the upload is for: the engine that produced the subtitles, for quality scoring.
pub struct UploadMeta {
    pub title: String,
    pub show: String,
    pub duration: f64,
    pub source: String,
    pub model: String,
    pub language: String,
    pub install: String,
    /// Cached Chinese translations, one per segment, when at least 60% of the lines have one.
    pub translation: Option<(String, Vec<Option<String>>)>,
    /// The audio the segments were timed against, with its fingerprint.
    pub audio: Option<crate::align::AudioRef>,
}

/// Upload the transcript we just made. Returns the server's action ("created", "confirmed", …).
pub async fn upload(keys: &[String], meta: UploadMeta, segments: &[Segment]) -> anyhow::Result<String> {
    upload_with(keys, meta, segments, "google", "").await
}

/// Like `upload`, naming the translation's engine (llm | google) and model.
pub async fn upload_with(keys: &[String], meta: UploadMeta, segments: &[Segment], engine: &str, tr_model: &str) -> anyhow::Result<String> {
    let body = Upload {
        keys,
        title: &meta.title,
        show: &meta.show,
        duration: meta.duration,
        source: &meta.source,
        model: &meta.model,
        language: &meta.language,
        segments,
        translation: meta.translation.map(|(lang, items)| UploadTranslation { lang, engine: engine.into(), model: tr_model.into(), items }),
        client: ClientInfo { app: "subcast", version: env!("CARGO_PKG_VERSION"), install: meta.install },
        audio: meta.audio.as_ref().and_then(|a| Some(UploadAudio { bytes: a.bytes, etag: &a.etag, secs: a.secs, fp_algo: crate::align::FP_ALGO, fp: a.fp.as_deref()? })),
    };
    let resp = net::client().post(API).json(&body).timeout(Duration::from_secs(30)).send().await?;
    if !resp.status().is_success() { anyhow::bail!("shared transcripts: HTTP {}", resp.status()); }
    let reply: UploadReply = resp.json().await?;
    Ok(reply.action.unwrap_or_default())
}

/// The fingerprint of the audio a shared version was made from.
pub async fn fingerprint(id: &str) -> Option<Vec<u32>> {
    if id.is_empty() { return None; }
    let resp = net::client().get(format!("{API}/{}/fingerprint", urlencoding::encode(id))).timeout(Duration::from_secs(20)).send().await.ok()?;
    if !resp.status().is_success() { return None; }
    #[derive(Deserialize)]
    struct Fp { algo: String, fp: String }
    let f: Fp = resp.json().await.ok()?;
    (f.algo == crate::align::FP_ALGO).then(|| crate::align::decode_fp(&f.fp)).flatten()
}

/// The transcript we hand back when the shared library has this episode. Translation items, when
/// present, are dropped into the same cache translate_texts uses, so the subtitle tab shows them.
pub fn into_transcript(episode_id: &str, hit: Shared) -> Transcript {
    Transcript { episode_id: episode_id.into(), source: format!("shared:{}", hit.source), language: hit.language, segments: hit.segments }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_keys() {
        let ep = Episode {
            guid: "abc123".into(),
            audio_url: Some("https://media.example.com/ep1.mp3".into()),
            video_id: Some("tBMlXQIrYK4".into()),
            transcript_type: Some("qiaomu-rss".into()),
            ..Default::default()
        };
        let ks = keys(&ep);
        assert_eq!(ks.len(), 4, "{ks:?}");
        assert_eq!(ks[0], "yt:tBMlXQIrYK4");
        assert_eq!(ks[1], "qm:abc123");
        assert!(ks[2].starts_with("guid:") && ks[2].len() == 5 + 40);
        assert!(ks[3].starts_with("audio:") && ks[3].len() == 6 + 40);
        // A plain podcast episode: guid + audio only.
        let ep2 = Episode { guid: "g".into(), audio_url: Some("a".into()), ..Default::default() };
        assert_eq!(keys(&ep2).len(), 2);
        // No identifiers at all: no keys, no lookup.
        assert!(keys(&Episode::default()).is_empty());
    }
}
