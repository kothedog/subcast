//! Bringing subscriptions in and out.
//!
//! YouTube, from the browser the listener is signed in with: the app's own yt-dlp copies that browser's YouTube
//! cookies into a file (it knows how to read Chrome, Edge, Brave, Firefox and Safari), the channels page
//! (youtube.com/feed/channels) is read with them, and the cookie file is deleted at once. Nothing leaves the
//! computer except the request to YouTube itself. Files (OPML, Google Takeout CSV, NewPipe and FreeTube exports)
//! are parsed in the front end; OPML export is written here.
use crate::net;
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Channel { pub id: String, pub title: String, pub thumbnail: String }

/// Netscape cookie file -> "name=value; …" for one site (and its subdomains).
fn cookie_header(file: &str, domain: &str) -> (String, Option<String>) {
    let mut pairs = Vec::new();
    let mut sapisid = None;
    for line in file.lines() {
        let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
        if line.starts_with('#') || line.trim().is_empty() { continue; }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 7 { continue; }
        let host = f[0].trim_start_matches('.');
        if host != domain && !domain.ends_with(&format!(".{host}")) && !host.ends_with(domain) { continue; }
        if f[5] == "SAPISID" || (f[5] == "__Secure-3PAPISID" && sapisid.is_none()) { sapisid = Some(f[6].to_string()); }
        pairs.push(format!("{}={}", f[5], f[6]));
    }
    (pairs.join("; "), sapisid)
}

/// The header YouTube's own web client sends with signed-in API calls.
fn sapisid_hash(sapisid: &str) -> String {
    use sha1::{Digest, Sha1};
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    let origin = "https://www.youtube.com";
    format!("SAPISIDHASH {ts}_{}", hex::encode(Sha1::digest(format!("{ts} {sapisid} {origin}").as_bytes())))
}

/// The JSON object assigned after `marker` in a page (`var ytInitialData = {…};`).
fn embedded_json(page: &str, marker: &str) -> Option<Value> {
    let start = page.find(marker)? + marker.len();
    let rest = page[start..].trim_start().trim_start_matches('=').trim_start();
    let mut de = serde_json::Deserializer::from_str(rest).into_iter::<Value>();
    de.next()?.ok()
}

/// Every channel the page lists, and the token for the next page if there is one.
pub fn channels_in(data: &Value) -> (Vec<Channel>, Option<String>) {
    let mut out = Vec::new();
    let mut next = None;
    fn walk(v: &Value, out: &mut Vec<Channel>, next: &mut Option<String>) {
        match v {
            Value::Object(m) => {
                if let Some(c) = m.get("channelRenderer") {
                    let id = c["channelId"].as_str().unwrap_or("").to_string();
                    let title = c["title"]["simpleText"].as_str().map(str::to_string)
                        .or_else(|| c["title"]["runs"][0]["text"].as_str().map(str::to_string)).unwrap_or_default();
                    let thumb = c["thumbnail"]["thumbnails"].as_array().and_then(|a| a.last()).and_then(|t| t["url"].as_str()).unwrap_or("");
                    let thumbnail = if thumb.starts_with("//") { format!("https:{thumb}") } else { thumb.to_string() };
                    if id.starts_with("UC") && !out.iter().any(|x: &Channel| x.id == id) { out.push(Channel { id, title, thumbnail }); }
                    return;
                }
                if let Some(tok) = m.get("continuationCommand").and_then(|c| c["token"].as_str()) { *next = Some(tok.to_string()); }
                for x in m.values() { walk(x, out, next); }
            }
            Value::Array(a) => for x in a { walk(x, out, next) },
            _ => {}
        }
    }
    walk(data, &mut out, &mut next);
    (out, next)
}

/// Text the interface looks for to offer the 「打开系统设置」 button (i18n: matched on the backend wording).
pub const NEEDS_ACCESS: &str = "需要你允许 SubCast 读取"; // i18n-ignore (the interface shows its own text)

fn browser_name(browser: &str) -> &'static str {
    match browser { "chrome" => "Chrome", "safari" => "Safari", "edge" => "Edge", "firefox" => "Firefox", "brave" => "Brave", "chromium" => "Chromium", "opera" => "Opera", "vivaldi" => "Vivaldi", _ => "浏览器" }
}

#[cfg(target_os = "macos")]
fn browser_dir(browser: &str) -> Option<std::path::PathBuf> {
    let home = dirs::home_dir()?;
    let sub = match browser {
        "chrome" => "Library/Application Support/Google/Chrome",
        "edge" => "Library/Application Support/Microsoft Edge",
        "brave" => "Library/Application Support/BraveSoftware/Brave-Browser",
        "chromium" => "Library/Application Support/Chromium",
        "vivaldi" => "Library/Application Support/Vivaldi",
        "opera" => "Library/Application Support/com.operasoftware.Opera",
        "firefox" => "Library/Application Support/Firefox/Profiles",
        "safari" => "Library/Containers/com.apple.Safari/Data/Library/Cookies",
        _ => return None,
    };
    Some(home.join(sub))
}

/// macOS (26 and later) guards other apps' data. yt-dlp is a child process: when it is blocked it gets no prompt
/// and only reports "could not find … cookies". So SubCast itself opens the browser's cookie store first: macOS
/// then asks the listener on SubCast's behalf, and a refusal becomes a clear message with a way to Settings.
pub fn ensure_browser_access(browser: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::io::ErrorKind;
        let name = browser_name(browser);
        let Some(dir) = browser_dir(browser) else { return Ok(()) };
        let denied = || anyhow!("{NEEDS_ACCESS} {name} 的数据：在 系统设置 › 隐私与安全性 › 文件和文件夹 里，打开 SubCast 下的「{name}」，然后重试"); // i18n-ignore (the interface shows its own text)
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == ErrorKind::NotFound => return Err(anyhow!("没有找到 {name} 的数据，这台电脑可能没有用过 {name}。换一个浏览器试试")), // i18n-ignore (the interface shows its own text)
            Err(e) if e.kind() == ErrorKind::PermissionDenied => return Err(denied()),
            Err(_) => return Ok(()),
        };
        // Open one cookie database (the file itself is what macOS guards in some versions).
        let mut cands: Vec<std::path::PathBuf> = vec![dir.join("Cookies.binarycookies")];
        for e in entries.flatten() {
            let p = e.path();
            cands.extend([p.join("Cookies"), p.join("Network/Cookies"), p.join("cookies.sqlite")]);
        }
        for c in cands.iter().filter(|c| c.exists()) {
            match std::fs::File::open(c) {
                Ok(_) => return Ok(()),
                Err(e) if e.kind() == ErrorKind::PermissionDenied => return Err(denied()),
                Err(_) => continue,
            }
        }
    }
    let _ = browser;
    Ok(())
}

/// Opens System Settings at Privacy & Security › Files and Folders (macOS).
pub fn open_privacy_settings() -> Result<()> {
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.preference.security?Privacy_FilesAndFolders").spawn()?;
    Ok(())
}

/// The listener's YouTube subscriptions, read with the cookies of `browser` (chrome, safari, firefox, edge, brave…).
pub async fn youtube_subscriptions(ytdlp: &Path, browser: &str) -> Result<Vec<Channel>> {
    { let b = browser.to_string(); tokio::task::spawn_blocking(move || ensure_browser_access(&b)).await??; }
    let dir = std::env::temp_dir().join(format!("subcast-cookies-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir)?;
    let jar = dir.join("cookies.txt");
    let result = async {
        // yt-dlp writes the browser's cookies to the jar on exit; the URL is a tiny page it handles quickly.
        let mut cmd = tokio::process::Command::new(ytdlp);
        cmd.args(["--cookies-from-browser", browser, "--cookies"]).arg(&jar)
            .args(["--skip-download", "--flat-playlist", "--no-warnings", "--playlist-items", "0", "https://www.youtube.com/feed/channels"])
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        let proxy = net::effective_proxy();
        if !proxy.is_empty() { cmd.args(["--proxy", &proxy]); }
        #[cfg(windows)]
        { cmd.creation_flags(0x08000000); }
        let out = tokio::time::timeout(std::time::Duration::from_secs(90), cmd.output()).await.map_err(|_| anyhow!("读取浏览器登录信息超时"))??;
        let file = std::fs::read_to_string(&jar).unwrap_or_default();
        if !file.contains("youtube.com") {
            let err = String::from_utf8_lossy(&out.stderr);
            let why = err.lines().rev().find(|l| l.contains("ERROR")).unwrap_or("").to_string();
            // Our own check passed, so "could not find" from yt-dlp means it was blocked (macOS) or the
            // browser keeps no cookies yet.
            if why.to_lowercase().contains("could not find") || why.to_lowercase().contains("not found") {
                return Err(if cfg!(target_os = "macos") { anyhow!("{NEEDS_ACCESS} {} 的数据：在 系统设置 › 隐私与安全性 › 文件和文件夹 里，打开 SubCast 下的「{}」，然后重试", browser_name(browser), browser_name(browser)) } else { anyhow!("没有找到这个浏览器的数据，换一个浏览器试试") }); // i18n-ignore (the interface shows its own text)
            }
            if why.contains("Operation not permitted") || why.to_lowercase().contains("permission") { return Err(anyhow!("没有权限读取这个浏览器的数据（Safari 需要在 系统设置 › 隐私与安全性 › 完全磁盘访问权限 里允许 SubCast）")); }
            return Err(anyhow!("这个浏览器里没有登录 YouTube{}", if why.is_empty() { String::new() } else { format!("：{}", why.chars().take(160).collect::<String>()) }));
        }
        let (cookie, sapisid) = cookie_header(&file, "www.youtube.com");
        let page = net::client().get("https://www.youtube.com/feed/channels").header("Cookie", &cookie).header("Accept-Language", "en-US,en;q=0.9")
            .send().await?.text().await?;
        let data = embedded_json(&page, "ytInitialData").ok_or_else(|| anyhow!("没有读到 YouTube 的订阅页"))?;
        let (mut all, mut next) = channels_in(&data);
        if all.is_empty() && next.is_none() { return Err(anyhow!("这个浏览器里没有登录 YouTube，或者还没有订阅任何频道")); }
        let version = embedded_json(&page, "ytcfg.set(").and_then(|c| c["INNERTUBE_CLIENT_VERSION"].as_str().map(str::to_string)).unwrap_or_else(|| "2.20251101.00.00".into());
        // Long lists come in pages.
        for _ in 0..40 {
            let Some(token) = next.take() else { break };
            let mut req = net::client().post("https://www.youtube.com/youtubei/v1/browse?prettyPrint=false").header("Cookie", &cookie)
                .header("X-Origin", "https://www.youtube.com").header("Origin", "https://www.youtube.com")
                .json(&json!({ "context": { "client": { "clientName": "WEB", "clientVersion": version, "hl": "en" } }, "continuation": token }));
            if let Some(s) = &sapisid { req = req.header("Authorization", sapisid_hash(s)); }
            let v: Value = req.send().await?.json().await?;
            let (more, n) = channels_in(&v);
            for c in more { if !all.iter().any(|x| x.id == c.id) { all.push(c); } }
            next = n;
        }
        Ok(all)
    }.await;
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn xml_escape(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;") }

/// An OPML file of the subscriptions (title, feed address), the format every podcast app imports.
pub fn opml(shows: &[(String, String)]) -> String {
    let mut out = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<opml version=\"2.0\">\n  <head><title>SubCast</title></head>\n  <body>\n");
    for (title, url) in shows {
        out.push_str(&format!("    <outline type=\"rss\" text=\"{0}\" title=\"{0}\" xmlUrl=\"{1}\"/>\n", xml_escape(title), xml_escape(url)));
    }
    out.push_str("  </body>\n</opml>\n");
    out
}

/// Parses an OPML XML file into `(title, xml_url)` pairs.
pub fn parse_opml(xml: &str) -> Result<Vec<(String, String)>> {
    let doc = roxmltree::Document::parse(xml)?;
    let mut out = Vec::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("outline")) {
        let Some(url) = node.attribute("xmlUrl").or_else(|| node.attribute("xmlurl")).map(str::trim).filter(|u| !u.is_empty()) else {
            continue;
        };
        let title = node
            .attribute("text")
            .or_else(|| node.attribute("title"))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(url)
            .to_string();
        out.push((title, url.to_string()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finds_channels_and_the_next_page() {
        let data = json!({ "contents": { "twoColumnBrowseResultsRenderer": { "tabs": [{ "tabRenderer": { "content": { "sectionListRenderer": { "contents": [
            { "itemSectionRenderer": { "contents": [{ "shelfRenderer": { "content": { "expandedShelfContentsRenderer": { "items": [
                { "channelRenderer": { "channelId": "UCabc", "title": { "simpleText": "Lex Fridman" }, "thumbnail": { "thumbnails": [{ "url": "//yt3.ggpht.com/a=s88" }, { "url": "//yt3.ggpht.com/a=s176" }] } } },
                { "channelRenderer": { "channelId": "UCdef", "title": { "runs": [{ "text": "词元之外" }] }, "thumbnail": { "thumbnails": [] } } }
            ] } } } }] } },
            { "continuationItemRenderer": { "continuationEndpoint": { "continuationCommand": { "token": "TOKEN1", "request": "CONTINUATION_REQUEST_TYPE_BROWSE" } } } }
        ] } } } }] } } });
        let (ch, next) = channels_in(&data);
        assert_eq!(ch.len(), 2);
        assert_eq!(ch[0], Channel { id: "UCabc".into(), title: "Lex Fridman".into(), thumbnail: "https://yt3.ggpht.com/a=s176".into() });
        assert_eq!(ch[1].title, "词元之外");
        assert_eq!(next.as_deref(), Some("TOKEN1"));
    }
    #[test]
    fn reads_the_page_data_and_cookies() {
        let page = r#"<script>var ytInitialData = {"a":{"channelRenderer":{"channelId":"UCx","title":{"simpleText":"X"}}}};</script><script>ytcfg.set({"INNERTUBE_CLIENT_VERSION":"2.2026"});</script>"#;
        assert_eq!(channels_in(&embedded_json(page, "ytInitialData").unwrap()).0[0].id, "UCx");
        assert_eq!(embedded_json(page, "ytcfg.set(").unwrap()["INNERTUBE_CLIENT_VERSION"], "2.2026");
        let jar = "# Netscape HTTP Cookie File\n.youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tsecret\n#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\tSID\tabc\n.google.com\tTRUE\t/\tTRUE\t0\tNID\tzzz\n";
        let (h, s) = cookie_header(jar, "www.youtube.com");
        assert_eq!(h, "SAPISID=secret; SID=abc");
        assert_eq!(s.as_deref(), Some("secret"));
        assert!(sapisid_hash("secret").starts_with("SAPISIDHASH "));
    }
    #[test]
    fn opml_escapes() {
        let o = opml(&[("A & B <1>".into(), "https://x.com/feed?a=1&b=2".into())]);
        assert!(o.contains("text=\"A &amp; B &lt;1&gt;\"") && o.contains("xmlUrl=\"https://x.com/feed?a=1&amp;b=2\""));
    }
}
