use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Show {
    pub id: String,
    /// youtube | podcast | xiaoyuzhou | single
    pub kind: String,
    pub title: String,
    pub author: Option<String>,
    pub cover: Option<String>,
    pub feed_url: String,
    pub link: Option<String>,
    pub description: Option<String>,
    pub updated_at: i64,
    #[serde(default)]
    pub unplayed: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(rename_all = "camelCase")]
pub struct Episode {
    pub id: String,
    pub show_id: String,
    pub guid: String,
    pub title: String,
    pub published: Option<i64>,
    pub duration: Option<f64>,
    pub audio_url: Option<String>,
    pub video_id: Option<String>,
    pub link: Option<String>,
    pub cover: Option<String>,
    pub notes: Option<String>,
    pub transcript_url: Option<String>,
    pub transcript_type: Option<String>,
    #[serde(default)]
    pub progress: f64,
    #[serde(default)]
    pub has_transcript: bool,
    #[serde(default)]
    pub show_title: Option<String>,
    /// Has a 乔木精读 / 乔木改写 article.
    #[serde(default)]
    pub deep: bool,
    /// User/ended override for watched state: "done" | "unseen" | None (auto from progress).
    #[serde(default)]
    pub watched_override: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    pub episode_id: String,
    /// youtube | youtube-auto | rss | whisper | cloud
    pub source: String,
    pub language: String,
    pub segments: Vec<Segment>,
}

pub fn short_hash(text: &str, len: usize) -> String {
    use sha1::{Digest, Sha1};
    let digest = Sha1::digest(text.as_bytes());
    hex::encode(digest)[..len].to_string()
}

pub fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
