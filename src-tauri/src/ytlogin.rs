//! Signing in to YouTube inside SubCast, for the videos YouTube only plays after its "confirm you're not a bot"
//! check. Borrowing a browser's sign-in (yt-dlp --cookies-from-browser) fails where the browser encrypts its
//! cookies for itself (Chrome 127+ on Windows: "Failed to decrypt with DPAPI"), so SubCast can keep its own:
//! the listener signs in once in a SubCast window, the sign-in stays in SubCast's own web view storage, and each
//! yt-dlp job that needs it gets a fresh copy as a cookie file that is deleted when the job ends.
use std::path::PathBuf;
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

static APP: OnceLock<AppHandle> = OnceLock::new();
const LABEL: &str = "youtube-sign-in";
const SIGN_IN: &str = "https://accounts.google.com/ServiceLogin?service=youtube&continue=https%3A%2F%2Fwww.youtube.com%2F";
/// Google turns away sign-ins from pages that look like an embedded web view, so the window says which browser
/// engine it really is.
#[cfg(target_os = "macos")]
const AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Safari/605.1.15";
#[cfg(not(target_os = "macos"))]
const AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/138.0.0.0 Safari/537.36 Edg/138.0.0.0";

pub fn init(app: &AppHandle) { let _ = APP.set(app.clone()); }

fn ours(domain: &str) -> bool {
    let d = domain.trim_start_matches('.');
    d == "youtube.com" || d.ends_with(".youtube.com") || d == "google.com" || d.ends_with(".google.com")
}

/// The YouTube and Google cookies in SubCast's web view storage (shared by all its windows). Blocks until the
/// web view answers: call off the main thread and off the async workers (spawn_blocking).
fn cookies() -> Vec<tauri::webview::Cookie<'static>> {
    let Some(app) = APP.get() else { return vec![] };
    let Some(w) = app.get_webview_window(LABEL).or_else(|| app.get_webview_window("main")) else { return vec![] };
    w.cookies().unwrap_or_default().into_iter().filter(|c| c.domain().is_some_and(ours)).collect()
}

fn signed_in_with(cs: &[tauri::webview::Cookie<'static>]) -> bool {
    cs.iter().any(|c| c.name() == "LOGIN_INFO" && c.domain().is_some_and(|d| d.trim_start_matches('.').ends_with("youtube.com")))
}

/// Netscape cookie file, the format yt-dlp reads with --cookies.
fn jar_text(cs: &[tauri::webview::Cookie<'static>]) -> String {
    let mut out = String::from("# Netscape HTTP Cookie File\n");
    for c in cs {
        let Some(domain) = c.domain() else { continue };
        let dotted = if domain.starts_with('.') { domain.to_string() } else { format!(".{domain}") };
        let expires = c.expires_datetime().map(|t| t.unix_timestamp().max(0)).unwrap_or(0);
        let clean = |s: &str| s.replace(['\t', '\n', '\r'], "");
        out.push_str(&format!("{}{}\tTRUE\t{}\t{}\t{}\t{}\t{}\n",
            if c.http_only().unwrap_or(false) { "#HttpOnly_" } else { "" }, dotted, c.path().unwrap_or("/"),
            if c.secure().unwrap_or(false) { "TRUE" } else { "FALSE" }, expires, clean(c.name()), clean(c.value())));
    }
    out
}

/// Whether the listener has signed in to YouTube in SubCast.
pub async fn signed_in() -> bool {
    tokio::task::spawn_blocking(|| signed_in_with(&cookies())).await.unwrap_or(false)
}

/// A cookie file with the current sign-in for one yt-dlp job, or None when not signed in. Delete it afterwards.
pub async fn cookie_file() -> Option<PathBuf> {
    let cs = tokio::task::spawn_blocking(cookies).await.ok()?;
    if !signed_in_with(&cs) { return None; }
    let path = std::env::temp_dir().join(format!("subcast-yt-{}.txt", uuid::Uuid::new_v4()));
    write_private(&path, &jar_text(&cs)).ok()?;
    Some(path)
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
        f.write_all(text.as_bytes())
    }
    #[cfg(not(unix))]
    std::fs::write(path, text)
}

/// Opens the sign-in window. Call from an async command: building a window inside a synchronous one deadlocks on
/// Windows. When the listener is back on youtube.com signed in, the window closes itself and
/// the interface hears `youtube-signed-in`.
pub fn open(app: &AppHandle) -> Result<()> {
    if let Some(w) = app.get_webview_window(LABEL) { let _ = w.set_focus(); return Ok(()); }
    let handle = app.clone();
    let mut builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::External(SIGN_IN.parse()?))
        .title("YouTube")
        .inner_size(480.0, 680.0)
        .user_agent(AGENT);
    // Same proxy as the main window: WebView2 won't open a second window with other browser arguments.
    if let Some(p) = crate::webview_proxy() { builder = builder.proxy_url(p); }
    builder
        .on_page_load(move |w, p| {
            if !matches!(p.event(), tauri::webview::PageLoadEvent::Finished) { return; }
            if !p.url().host_str().is_some_and(|h| h.ends_with("youtube.com")) { return; }
            let handle = handle.clone();
            // Reading cookies waits on the web view: not from this (main-thread) callback.
            std::thread::spawn(move || {
                if signed_in_with(&cookies()) {
                    let _ = handle.emit("youtube-signed-in", true);
                    let _ = w.close();
                }
            });
        })
        .build()
        .map_err(|e| anyhow!("没能打开登录窗口：{e}"))?; // i18n-ignore (the interface shows its own text)
    // Windows and Linux put the app's menu bar (File, Edit, Window, Help) on every window; a sign-in page has no use for it.
    #[cfg(not(target_os = "macos"))]
    { if let Some(w) = app.get_webview_window(LABEL) { let _ = w.remove_menu(); } }
    Ok(())
}

/// Signs out of YouTube in SubCast: its YouTube and Google cookies are deleted.
pub async fn sign_out() -> Result<()> {
    tokio::task::spawn_blocking(|| {
        let Some(app) = APP.get() else { return };
        let Some(w) = app.get_webview_window("main") else { return };
        for c in cookies() { let _ = w.delete_cookie(c); }
    }).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::webview::Cookie;
    #[test]
    fn writes_a_netscape_jar_and_sees_the_sign_in() {
        let mut a = Cookie::new("LOGIN_INFO", "abc");
        a.set_domain(".youtube.com"); a.set_path("/"); a.set_secure(true); a.set_http_only(true);
        let mut b = Cookie::new("PREF", "f6=40000000");
        b.set_domain("www.youtube.com");
        let cs = vec![a, b];
        assert!(signed_in_with(&cs));
        let jar = jar_text(&cs);
        assert!(jar.starts_with("# Netscape HTTP Cookie File\n"));
        assert!(jar.contains("#HttpOnly_.youtube.com\tTRUE\t/\tTRUE\t0\tLOGIN_INFO\tabc\n"));
        assert!(jar.contains(".www.youtube.com\tTRUE\t/\tFALSE\t0\tPREF\tf6=40000000\n"));
        assert!(!signed_in_with(&cs[1..]));
        assert!(ours(".google.com") && ours("accounts.google.com") && !ours("notyoutube.com"));
    }
}
