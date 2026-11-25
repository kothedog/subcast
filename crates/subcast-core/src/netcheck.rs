//! Network diagnosis for "the video won't load" reports: what SubCast itself can reach through its proxy setting
//! (YouTube, YouTube's video servers, where yt-dlp comes from) and which yt-dlp it has. The interface adds what its
//! own web view can reach, which is what YouTube's embedded player uses.
use crate::{asr, net};
use serde::Serialize;
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// proxy | youtube | video | ytdlpSource | ytdlp
    pub id: &'static str,
    pub ok: bool,
    pub ms: Option<u64>,
    /// The proxy in use, the yt-dlp version, or why it failed.
    pub detail: String,
}

/// A proxy address without any user name or password in it, safe to show and to copy into a report.
pub fn redact_proxy(p: &str) -> String {
    match p.split_once("://") {
        Some((scheme, rest)) => match rest.rsplit_once('@') { Some((_, host)) => format!("{scheme}://***@{host}"), None => p.to_string() },
        None => match p.rsplit_once('@') { Some((_, host)) => format!("***@{host}"), None => p.to_string() },
    }
}

async fn reach(id: &'static str, url: &str) -> Check {
    let start = Instant::now();
    let res = tokio::time::timeout(Duration::from_secs(10), net::client().get(url).send()).await;
    let ms = Some(start.elapsed().as_millis() as u64);
    match res {
        // Any answer means the route works (some of these hosts answer 204, others redirect or 404).
        Ok(Ok(_)) => Check { id, ok: true, ms, detail: String::new() },
        Ok(Err(e)) => Check { id, ok: false, ms, detail: net::describe(e).to_string() },
        Err(_) => Check { id, ok: false, ms, detail: "10 秒内没有响应".into() }, // i18n-ignore (shown through tr())
    }
}

/// Runs every check at once; about 10 s at most.
pub async fn run(data: &Path) -> Vec<Check> {
    let own = net::proxy();
    let used = net::effective_proxy();
    let proxy = Check {
        id: "proxy",
        ok: true,
        ms: None,
        detail: if !own.trim().is_empty() { format!("SubCast: {}", redact_proxy(own.trim())) }
            else if !used.trim().is_empty() { format!("system: {}", redact_proxy(used.trim())) }
            else { "none".into() },
    };
    let ytdlp = async {
        match asr::find_ytdlp(data) {
            None => Check { id: "ytdlp", ok: false, ms: None, detail: "not installed".into() },
            Some(bin) => match asr::ytdlp_version(&bin).await {
                Some(v) => Check { id: "ytdlp", ok: true, ms: None, detail: v },
                None => Check { id: "ytdlp", ok: false, ms: None, detail: "doesn't run".into() },
            },
        }
    };
    let (youtube, video, source, ytdlp) = tokio::join!(
        reach("youtube", "https://www.youtube.com/generate_204"),
        reach("video", "https://redirector.googlevideo.com/generate_204"),
        reach("ytdlpSource", asr::YTDLP_MIRROR),
        ytdlp,
    );
    vec![proxy, youtube, video, source, ytdlp]
}

#[cfg(test)]
mod tests {
    use super::redact_proxy;
    #[test]
    fn proxy_credentials_are_hidden() {
        assert_eq!(redact_proxy("http://127.0.0.1:7890"), "http://127.0.0.1:7890");
        assert_eq!(redact_proxy("http://user:pass@proxy.example:8080"), "http://***@proxy.example:8080");
        assert_eq!(redact_proxy("socks5://a:b@c@host:1080"), "socks5://***@host:1080");
        assert_eq!(redact_proxy("user:pw@host:3128"), "***@host:3128");
    }
}
