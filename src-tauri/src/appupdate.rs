//! App updates, downloaded the fast way. The manifest comes from the updater plugin as before (signed packages,
//! same public key), but the package itself is fetched with our own client (it follows the proxy setting) from
//! whichever source answers faster right now: our server or the GitHub release, which carries the same files.
//! A quick 256 KB probe of each picks the order; if one fails mid-way the other is tried. The bytes are
//! verified against the update's signature before they are kept, and installed only when the listener asks.
use crate::net;
use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde::Serialize;
use parking_lot::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::{Update, UpdaterExt};

const GITHUB: &str = "https://github.com/joeseesun/subcast/releases/download";
const PROBE: usize = 256 * 1024;

/// The update found by the last check, and its verified package once downloaded.
static PENDING: Mutex<Option<(Update, Option<Vec<u8>>)>> = Mutex::new(None);

#[derive(Serialize)]
pub struct Found { pub version: String, pub notes: Option<String>, pub ready: bool }

pub async fn check(app: &AppHandle) -> Result<Option<Found>> {
    let Some(u) = app.updater()?.check().await? else { *PENDING.lock() = None; return Ok(None) };
    let mut p = PENDING.lock();
    // Already downloaded this version: keep the bytes.
    let ready = matches!(&*p, Some((old, Some(_))) if old.version == u.version);
    if !ready { *p = Some((u.clone(), None)); }
    Ok(Some(Found { version: u.version.clone(), notes: u.body.clone(), ready }))
}

/// Our server first in the list; the GitHub release has the same file under the version's tag.
fn sources(u: &Update) -> Vec<(&'static str, String)> {
    let site = u.download_url.to_string();
    let file = u.download_url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("").to_string();
    let mut out = vec![("site", site)];
    if !file.is_empty() { out.push(("github", format!("{GITHUB}/v{}/{}", u.version, file))); }
    out
}

/// Seconds to fetch the first 256 KB (or the whole file if smaller), None if the source doesn't answer.
async fn probe(url: &str) -> Option<f64> {
    let t = Instant::now();
    let res = net::client().get(url).header("Range", format!("bytes=0-{}", PROBE - 1)).timeout(Duration::from_secs(8)).send().await.ok()?;
    if !res.status().is_success() { return None; }
    let mut got = 0usize;
    let mut stream = res.bytes_stream();
    let deadline = Duration::from_secs(8);
    while let Ok(Some(chunk)) = tokio::time::timeout(deadline.saturating_sub(t.elapsed()), stream.next()).await {
        got += chunk.ok()?.len();
        if got >= PROBE { break; }
    }
    (got > 0).then(|| t.elapsed().as_secs_f64() * (PROBE as f64 / got as f64))
}

async fn fetch(app: &AppHandle, url: &str, source: &str) -> Result<Vec<u8>> {
    let res = net::client().get(url).timeout(Duration::from_secs(1200)).send().await?;
    if !res.status().is_success() { return Err(anyhow!("HTTP {}", res.status())); }
    let total = res.content_length();
    let mut bytes = Vec::with_capacity(total.unwrap_or(0) as usize);
    let mut stream = res.bytes_stream();
    let mut last = Instant::now();
    while let Some(chunk) = stream.next().await {
        bytes.extend_from_slice(&chunk?);
        if last.elapsed() > Duration::from_millis(150) {
            last = Instant::now();
            let _ = app.emit("app-update", serde_json::json!({ "got": bytes.len(), "total": total, "source": source }));
        }
    }
    if total.is_some_and(|t| t as usize != bytes.len()) { return Err(anyhow!("下载不完整")); }
    Ok(bytes)
}

/// Downloads and verifies the pending update from the faster source (the other one if that fails).
pub async fn download(app: &AppHandle) -> Result<()> {
    let u = match &*PENDING.lock() {
        Some((_, Some(_))) => return Ok(()),
        Some((u, None)) => u.clone(),
        None => return Err(anyhow!("没有可下载的新版本")),
    };
    let list = sources(&u);
    let speeds = futures_util::future::join_all(list.iter().map(|(_, url)| probe(url))).await;
    let mut order: Vec<_> = list.into_iter().zip(speeds).collect();
    // Fastest first; a source that didn't answer goes last but is still tried.
    order.sort_by(|a, b| a.1.unwrap_or(f64::MAX).total_cmp(&b.1.unwrap_or(f64::MAX)));
    let mut err = anyhow!("下载失败");
    for ((source, url), _) in order {
        match fetch(app, &url, source).await {
            Ok(bytes) => {
                u.verify(&bytes).map_err(|e| anyhow!("新版本的签名校验没有通过：{e}"))?;
                let mut p = PENDING.lock();
                if let Some((cur, slot)) = p.as_mut() { if cur.version == u.version { *slot = Some(bytes); } }
                return Ok(());
            }
            Err(e) => err = e,
        }
    }
    Err(err)
}

/// Installs the downloaded update and restarts the app.
pub fn install(app: &AppHandle) -> Result<()> {
    let taken = PENDING.lock().take();
    let (u, bytes) = match taken {
        Some((u, Some(b))) => (u, b),
        other => { *PENDING.lock() = other; return Err(anyhow!("新版本还没有下载好")) }
    };
    u.install(&bytes)?;
    app.restart();
}

#[cfg(test)]
mod live {
    /// `cargo test --lib appupdate -- --ignored --nocapture`: how fast each source answers from here.
    #[tokio::test]
    #[ignore]
    async fn probe_both_sources() {
        for url in ["https://subcast.qiaomu.ai/download/0.2.7/SubCast_0.2.7_aarch64.app.tar.gz",
                    "https://github.com/joeseesun/subcast/releases/download/v0.2.7/SubCast_0.2.7_aarch64.app.tar.gz"] {
            println!("{:?}  {url}", super::probe(url).await);
        }
    }
}
