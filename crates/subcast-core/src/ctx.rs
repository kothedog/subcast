use crate::{asr, db, model::Segment, net, youtube};
use anyhow::{anyhow, Result};
use rusqlite::Connection;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use parking_lot::Mutex;
use std::sync::Arc;

pub const APP_IDENTIFIER: &str = "ai.qiaomu.subcast";

/// Resolves the shared application data directory (`SUBCAST_DATA_DIR`, else `<data_dir>/ai.qiaomu.subcast`).
pub fn default_data_dir() -> Result<PathBuf> {
    if let Ok(v) = std::env::var("SUBCAST_DATA_DIR") {
        let trimmed = v.trim();
        if !trimmed.is_empty() {
            return Ok(PathBuf::from(trimmed));
        }
    }
    Ok(dirs::data_dir()
        .ok_or_else(|| anyhow!("cannot determine application data directory"))? // i18n-ignore
        .join(APP_IDENTIFIER))
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum ProgressEvent {
    Asr {
        #[serde(rename = "episodeId")]
        episode_id: String,
        stage: String,
        progress: f64,
        message: Option<String>,
        segment: Option<Segment>,
    },
    LlmDelta {
        #[serde(rename = "requestId")]
        request_id: String,
        delta: String,
    },
    Download {
        #[serde(rename = "episodeId")]
        episode_id: String,
        progress: Option<f64>,
    },
    ModelDownload {
        id: String,
        received: u64,
        total: Option<u64>,
    },
    LocalTtsDownload {
        id: String,
        received: u64,
        total: Option<u64>,
    },
    Align {
        #[serde(rename = "episodeId")]
        episode_id: String,
        progress: Option<f64>,
    },
    VideoClip {
        #[serde(rename = "jobId")]
        job_id: String,
        stage: String,
        progress: f64,
        message: String,
    },
    YtStream {
        #[serde(rename = "videoId")]
        video_id: String,
        stage: String,
        progress: f64,
    },
    SharedUpload {
        action: String,
    },
    ShadowLevel {
        peak: f32,
    },
    Info {
        stage: String,
        message: String,
        progress: Option<f64>,
    },
}

pub trait ProgressSink: Send + Sync {
    fn emit(&self, ev: ProgressEvent);
}

#[derive(Default)]
pub struct NoopSink;

impl ProgressSink for NoopSink {
    fn emit(&self, _ev: ProgressEvent) {}
}

pub struct FnSink<F>(pub F);

impl<F> ProgressSink for FnSink<F>
where
    F: Fn(ProgressEvent) + Send + Sync,
{
    fn emit(&self, ev: ProgressEvent) {
        (self.0)(ev);
    }
}

pub struct CoreCtx {
    pub db: Mutex<Connection>,
    pub data: PathBuf,
    pub jobs: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

pub fn apply_remote_config(v: &Value) {
    youtube::set_clients(&v["youtubeClients"]);
    crate::curated_video::set_overrides(&v["curatedYoutube"]);
}

pub fn apply_runtime_settings(v: &Value) {
    let _ = net::set_proxy(v["proxy"].as_str().unwrap_or(""));
    *asr::YT_BROWSER.write() = v["ytCookiesBrowser"].as_str().unwrap_or("").to_string();
    let cf = v["ytCookiesFile"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    *asr::YT_COOKIES_FILE.write() = cf;
}

impl CoreCtx {
    /// Opens SubCast's SQLite library and applies stored proxy, YouTube cookie, and remote client settings.
    pub fn open(data_dir: Option<PathBuf>) -> Result<Self> {
        let data = match data_dir {
            Some(d) => d,
            None => default_data_dir()?,
        };
        std::fs::create_dir_all(&data)?;
        let conn = db::open(&data.join("subcast.db"))?;
        conn.execute(
            "INSERT OR IGNORE INTO shows(id,kind,title,feed_url,updated_at) VALUES('single','single','单独打开','',0)",
            [],
        )?;
        if let Some(s) = db::get_setting(&conn, "app")?.and_then(|r| serde_json::from_str::<Value>(&r).ok()) {
            apply_runtime_settings(&s);
        }
        if let Some(v) = db::get_setting(&conn, "remote-config")?.and_then(|r| serde_json::from_str::<Value>(&r).ok()) {
            apply_remote_config(&v);
        }
        // Clear stale 1:1 static-cover YouTube bindings on curated audio-only shows (both subscribed and previewed).
        let mut curated_feeds: Vec<(String, String)> = db::list_shows(&conn)
            .unwrap_or_default()
            .into_iter()
            .filter(|s| s.kind == "qiaomu")
            .map(|s| (s.id, s.feed_url))
            .collect();
        if let Ok(mut stmt) = conn.prepare("SELECT episode_id, data FROM blobs WHERE kind='show-json'") {
            if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                for row in rows.flatten() {
                    if let Ok(v) = serde_json::from_str::<Value>(&row.1) {
                        if v["kind"].as_str() == Some("qiaomu") {
                            if let Some(url) = v["feedUrl"].as_str().or_else(|| v["feed_url"].as_str()) {
                                curated_feeds.push((row.0, url.to_string()));
                            }
                        }
                    }
                }
            }
        }
        for (show_id, feed_url) in curated_feeds {
            if let Some(src) = crate::qiaomu::rss_source_id(&feed_url).or_else(|| crate::qiaomu::site_id(&feed_url)) {
                if crate::curated_video::is_audio_only(&src) {
                    let _ = db::clear_show_videos(&conn, &show_id);
                }
            }
        }
        Ok(Self {
            db: Mutex::new(conn),
            data,
            jobs: Mutex::new(HashMap::new()),
        })
    }

    pub fn job(&self, id: &str) -> Arc<AtomicBool> {
        let flag = Arc::new(AtomicBool::new(false));
        self.jobs.lock().insert(id.to_string(), flag.clone());
        flag
    }

    /// Ends a job: removes only its own entry, so a newer run of the same id stays cancellable.
    pub fn done_with(&self, id: &str, flag: &Arc<AtomicBool>) {
        let mut jobs = self.jobs.lock();
        if jobs.get(id).is_some_and(|f| Arc::ptr_eq(f, flag)) {
            jobs.remove(id);
        }
    }

    /// Whether a job with this id is currently running and not cancelled.
    pub fn running(&self, id: &str) -> bool {
        self.jobs
            .lock()
            .get(id)
            .is_some_and(|f| !f.load(Ordering::Relaxed))
    }

    pub fn cancel(&self, id: &str) {
        if let Some(f) = self.jobs.lock().get(id) {
            f.store(true, Ordering::Relaxed);
        }
    }

    pub fn cancel_all(&self) {
        for flag in self.jobs.lock().values() {
            flag.store(true, Ordering::Relaxed);
        }
    }
}
