mod appupdate;
#[cfg_attr(debug_assertions, allow(dead_code))] // interface updates are release-only
mod ota;
mod ytlogin;
mod clipath;

pub use subcast_core::net;
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use subcast_core::{
    agents, align, anki, asr, cloud, db, fastclip, feeds, import, llm, local_tts, offline, ops,
    qiaomu, shadow, speech, videoclip, CoreCtx, Episode, ProgressEvent, ProgressSink, Show,
    Transcript,
};
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg_attr(debug_assertions, allow(dead_code))]
const LOCAL_PORT: u16 = 14327;
#[cfg_attr(debug_assertions, allow(dead_code))]
const UI_PORTS: u16 = 20;

pub struct AppState {
    ctx: Arc<CoreCtx>,
}

type Res<T> = Result<T, String>;
fn e<E: std::fmt::Display>(err: E) -> String {
    err.to_string()
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct AsrEvent {
    episode_id: String,
    stage: String,
    progress: f64,
    message: Option<String>,
    segment: Option<subcast_core::Segment>,
}

struct TauriSink(AppHandle);

impl ProgressSink for TauriSink {
    fn emit(&self, ev: ProgressEvent) {
        match ev {
            ProgressEvent::Asr { episode_id, stage, progress, message, segment } => {
                let _ = self.0.emit("asr", AsrEvent { episode_id, stage, progress, message, segment });
            }
            ProgressEvent::LlmDelta { request_id, delta } => {
                let _ = self.0.emit("llm", json!({ "requestId": request_id, "delta": delta }));
            }
            ProgressEvent::Download { episode_id, progress } => {
                let _ = self.0.emit("offline", json!({ "episodeId": episode_id, "progress": progress }));
            }
            ProgressEvent::ModelDownload { id, received, total } => {
                let _ = self.0.emit("model-download", json!({ "id": id, "received": received, "total": total }));
            }
            ProgressEvent::LocalTtsDownload { id, received, total } => {
                let _ = self.0.emit("local-tts-download", json!({ "id": id, "received": received, "total": total }));
            }
            ProgressEvent::Align { episode_id, progress } => {
                let _ = self.0.emit("align", json!({ "episodeId": episode_id, "progress": progress }));
            }
            ProgressEvent::VideoClip { job_id, stage, progress, message } => {
                let _ = self.0.emit(
                    "video-clip-progress",
                    json!({ "jobId": job_id, "stage": stage, "progress": progress, "message": message }),
                );
            }
            ProgressEvent::YtStream { video_id, stage, progress } => {
                let _ = self.0.emit(
                    "yt-stream",
                    json!({ "videoId": video_id, "stage": stage, "progress": progress }),
                );
            }
            ProgressEvent::SharedUpload { action } => {
                let _ = self.0.emit("shared-upload", action);
            }
            ProgressEvent::ShadowLevel { peak } => {
                let _ = self.0.emit("shadow-level", peak);
            }
            ProgressEvent::Info { .. } => {}
        }
    }
}

// ---- subscriptions ---------------------------------------------------------------------------------

#[tauri::command]
async fn youtube_channel_feed(state: State<'_, AppState>, video_id: String) -> Res<String> {
    ops::youtube_channel_feed(&state.ctx, &video_id).await.map_err(e)
}

#[tauri::command]
async fn add_subscription(state: State<'_, AppState>, input: String) -> Res<ops::Added> {
    ops::add_subscription(&state.ctx, &input).await.map_err(e)
}

#[tauri::command(async)]
fn cached_show(state: State<'_, AppState>, input: String) -> Res<Option<ops::Preview>> {
    ops::cached_show(&state.ctx, &input).map_err(e)
}

#[tauri::command]
async fn preview_show(state: State<'_, AppState>, input: String) -> Res<ops::Preview> {
    ops::preview_show(&state.ctx, &input).await.map_err(e)
}

#[tauri::command]
async fn refresh_show(state: State<'_, AppState>, id: String) -> Res<Show> {
    ops::refresh_show(&state.ctx, &id).await.map_err(e)
}

#[tauri::command]
async fn refresh_all(state: State<'_, AppState>) -> Res<ops::Refreshed> {
    ops::refresh_all(&state.ctx).await.map_err(e)
}

#[tauri::command(async)]
fn list_shows(state: State<'_, AppState>) -> Res<Vec<Show>> {
    db::list_shows(&state.ctx.db.lock().unwrap()).map_err(e)
}

#[tauri::command(async)]
fn list_episodes(state: State<'_, AppState>, show_id: Option<String>, limit: Option<i64>) -> Res<Vec<Episode>> {
    db::list_episodes(&state.ctx.db.lock().unwrap(), show_id.as_deref(), limit.unwrap_or(300)).map_err(e)
}

#[tauri::command]
async fn search_show(state: State<'_, AppState>, show_id: String, query: String) -> Res<Vec<ops::ShowHit>> {
    ops::search_show(&state.ctx, &show_id, &query).await.map_err(e)
}

#[tauri::command(async)]
fn recent_episodes(state: State<'_, AppState>) -> Res<Vec<Episode>> {
    db::recent_episodes(&state.ctx.db.lock().unwrap(), 60).map_err(e)
}

#[tauri::command(async)]
fn get_episode(state: State<'_, AppState>, id: String) -> Res<Option<Episode>> {
    db::get_episode(&state.ctx.db.lock().unwrap(), &id).map_err(e)
}

#[tauri::command(async)]
fn remove_show(state: State<'_, AppState>, id: String, keep_episode_id: Option<String>) -> Res<()> {
    db::delete_show(&state.ctx.db.lock().unwrap(), &id, keep_episode_id.as_deref()).map_err(e)
}

#[tauri::command(async)]
fn set_progress(state: State<'_, AppState>, id: String, seconds: f64) -> Res<()> {
    db::set_progress(&state.ctx.db.lock().unwrap(), &id, seconds).map_err(e)
}

#[tauri::command]
fn set_watched(state: State<'_, AppState>, id: String, watched: Option<String>) -> Res<()> {
    db::set_watched(&state.ctx.db.lock().unwrap(), &id, watched.as_deref()).map_err(e)
}

#[tauri::command]
fn mark_show_watched(state: State<'_, AppState>, show_id: String) -> Res<usize> {
    db::mark_show_watched(&state.ctx.db.lock().unwrap(), &show_id).map_err(e)
}

#[tauri::command]
async fn search_podcasts(term: String, country: Option<String>) -> Res<Vec<feeds::SearchResult>> {
    feeds::search_podcasts(&term, country.as_deref().unwrap_or("US")).await.map_err(e)
}

// ---- transcripts & captions ------------------------------------------------------------------------

#[tauri::command(async)]
fn load_transcript(state: State<'_, AppState>, episode_id: String) -> Res<Option<Transcript>> {
    db::get_transcript(&state.ctx.db.lock().unwrap(), &episode_id).map_err(e)
}

#[tauri::command]
async fn youtube_tracks(state: State<'_, AppState>, episode_id: String) -> Res<Vec<ops::TrackChoice>> {
    ops::youtube_tracks(&state.ctx, &episode_id).await.map_err(e)
}

#[tauri::command]
async fn use_youtube_track(state: State<'_, AppState>, episode_id: String, language: String, auto: bool) -> Res<Transcript> {
    ops::use_youtube_track(&state.ctx, &episode_id, &language, auto).await.map_err(e)
}

#[tauri::command]
async fn fetch_official_transcript(state: State<'_, AppState>, episode_id: String) -> Res<Option<Transcript>> {
    ops::fetch_official_transcript(&state.ctx, &episode_id).await.map_err(e)
}

#[tauri::command]
async fn qiaomu_detail(state: State<'_, AppState>, episode_id: String) -> Res<qiaomu::Detail> {
    ops::qiaomu_detail(&state.ctx, &episode_id).await.map_err(e)
}

// ---- media, clips & export -------------------------------------------------------------------------

#[tauri::command]
fn save_image(name: String, base64: String) -> Res<String> {
    use base64::Engine;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(base64.trim_start_matches("data:image/png;base64,"))
        .map_err(e)?;
    let dir = dirs::download_dir().or_else(dirs::desktop_dir).ok_or("找不到下载文件夹")?;
    let safe: String = name.chars().filter(|c| !"/\\:*?\"<>|".contains(*c)).take(60).collect();
    let mut path = dir.join(format!("{safe}.png"));
    let mut i = 2;
    while path.exists() {
        path = dir.join(format!("{safe} {i}.png"));
        i += 1;
    }
    std::fs::write(&path, bytes).map_err(e)?;
    Ok(path.to_string_lossy().into_owned())
}

#[tauri::command]
async fn learn_clip(state: State<'_, AppState>, episode_id: String, start: f64, end: f64) -> Res<String> {
    ops::learn_clip(&state.ctx, &episode_id, start, end).await.map_err(e)
}

#[tauri::command]
async fn export_anki(path: String, deck: String, model: String, notes: Vec<anki::Note>) -> Res<usize> {
    tokio::task::spawn_blocking(move || anki::write(std::path::Path::new(&path), &deck, &model, &notes))
        .await
        .map_err(e)?
        .map_err(e)
}

#[tauri::command]
fn save_text(path: String, text: String) -> Res<()> {
    std::fs::write(path, text).map_err(e)
}

#[tauri::command(async)]
fn video_clip_env(state: State<'_, AppState>) -> videoclip::FfmpegStatus {
    videoclip::env_status(&state.ctx.data)
}

#[tauri::command]
fn video_clip_warm(state: State<'_, AppState>, video_id: String, ranges: Option<Vec<(f64, f64)>>) {
    let data = state.ctx.data.clone();
    tauri::async_runtime::spawn(async move {
        let d2 = data.clone();
        let never = Arc::new(AtomicBool::new(false));
        let (warmed, ff) = tokio::join!(fastclip::warm(&data, &video_id), videoclip::ensure_ffmpeg(&d2, &never, |_, _| {}));
        if warmed.is_ok() && ff.is_ok() {
            for (a, b) in ranges.unwrap_or_default().into_iter().take(3) {
                let _ = videoclip::prepare_segment(&data, &video_id, a, b, never.clone(), |_, _, _| {}).await;
            }
        }
    });
}

#[tauri::command]
async fn video_clip_prepare(
    app: AppHandle,
    state: State<'_, AppState>,
    job_id: String,
    video_id: String,
    start: f64,
    end: f64,
    ranges: Option<Vec<(f64, f64)>>,
) -> Res<videoclip::PreparedClip> {
    let sink = TauriSink(app);
    ops::video_clip_prepare(&state.ctx, &job_id, &video_id, start, end, ranges, &sink)
        .await
        .map_err(e)
}

#[tauri::command]
async fn video_clip_render(
    app: AppHandle,
    state: State<'_, AppState>,
    job_id: String,
    req: videoclip::RenderRequest,
) -> Res<videoclip::RenderResult> {
    let sink = TauriSink(app);
    ops::video_clip_render(&state.ctx, &job_id, req, &sink).await.map_err(e)
}

#[tauri::command]
fn reveal_file(app: AppHandle, path: String) -> Res<()> {
    use tauri_plugin_opener::OpenerExt;
    if !std::path::Path::new(&path).exists() {
        return Err("找不到导出的文件".into());
    }
    app.opener().reveal_item_in_dir(&path).map_err(e)
}

#[tauri::command]
async fn list_models(provider: llm::Provider) -> Res<Vec<String>> {
    cloud::list_models(&provider).await.map_err(e)
}

#[tauri::command]
async fn tts_speak(text: String, voice: String, resource: Option<String>) -> Res<String> {
    use base64::Engine;
    let wav = cloud::tts(&voice, resource.as_deref().unwrap_or(""), &text).await.map_err(e)?;
    Ok(base64::engine::general_purpose::STANDARD.encode(wav))
}

#[tauri::command]
async fn tts_synthesize(state: State<'_, AppState>, req: speech::TtsRequest) -> Res<speech::TtsResult> {
    speech::synthesize(&state.ctx.data, req).await.map_err(e)
}

#[tauri::command(async)]
fn tts_system_voices() -> Vec<speech::SystemVoice> {
    speech::list_system_voices()
}

#[tauri::command(async)]
fn local_tts_status(state: State<'_, AppState>) -> local_tts::Status {
    local_tts::status(&state.ctx.data)
}

#[tauri::command]
async fn local_tts_install(app: AppHandle, state: State<'_, AppState>, id: String, mirror_first: bool) -> Res<()> {
    let sink = TauriSink(app);
    ops::local_tts_install(&state.ctx, &id, mirror_first, &sink).await.map_err(e)
}

#[tauri::command(async)]
fn local_tts_delete(state: State<'_, AppState>, id: String) -> Res<()> {
    local_tts::delete(&state.ctx.data, &id).map_err(e)
}

#[tauri::command(async)]
fn clear_tts_cache(state: State<'_, AppState>) -> Res<()> {
    speech::clear_cache(&state.ctx.data).map_err(e)
}

// ---- localhost UI server & hot updates -------------------------------------------------------------

fn bind_ui_port(preferred: u16) -> Option<(u16, Vec<std::net::TcpListener>)> {
    use std::net::TcpListener;
    for port in preferred..preferred.saturating_add(UI_PORTS) {
        let Ok(v4) = TcpListener::bind(("127.0.0.1", port)) else { continue };
        match TcpListener::bind(("::1", port)) {
            Ok(v6) => return Some((port, vec![v4, v6])),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(_) => return Some((port, vec![v4])),
        }
    }
    None
}

/// The proxy set in SubCast, for the windows' web views too. Without it YouTube's embedded player (and the
/// sign-in page) only follow the system proxy, so a proxy set in SubCast alone left the player unable to load.
/// Every window gets the same one (WebView2 refuses a second window with other browser arguments). Taken at
/// launch: a changed proxy reaches the web views after a restart.
pub(crate) fn webview_proxy() -> Option<tauri::Url> {
    WEBVIEW_PROXY.get_or_init(|| parse_webview_proxy(&subcast_core::net::proxy()).ok()).clone()
}

/// What the web views got at launch (they keep it until the app restarts).
static WEBVIEW_PROXY: std::sync::OnceLock<Option<tauri::Url>> = std::sync::OnceLock::new();

/// A proxy setting as the web views take it, or why they can't: "none", "unsupported-proxy" (only http:// and
/// socks5:// with a port work there) or "unsupported-os" (macOS supports it from 14 on; older versions would crash).
fn parse_webview_proxy(raw: &str) -> Result<tauri::Url, &'static str> {
    let raw = raw.trim();
    if raw.is_empty() { return Err("none"); }
    let url: tauri::Url = (if raw.contains("://") { raw.to_string() } else { format!("http://{raw}") }).parse().map_err(|_| "unsupported-proxy")?;
    if !matches!(url.scheme(), "http" | "socks5") || url.host().is_none() || url.port().is_none() { return Err("unsupported-proxy"); }
    #[cfg(target_os = "macos")]
    if macos_major() < 14 { return Err("unsupported-os"); }
    Ok(url)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkReport {
    checks: Vec<subcast_core::netcheck::Check>,
    /// Which proxy the web views (YouTube's player) use: "subcast" (the one set in SubCast), "system" (none set),
    /// "restart" (SubCast's changed since launch), "unsupported-os" or "unsupported-proxy".
    webview_proxy: &'static str,
}

#[tauri::command]
async fn network_check(state: State<'_, AppState>) -> Res<NetworkReport> {
    let checks = subcast_core::netcheck::run(&state.ctx.data).await;
    let applied = webview_proxy();
    let webview_proxy = match parse_webview_proxy(&subcast_core::net::proxy()) {
        Ok(now) if applied.as_ref() == Some(&now) => "subcast",
        Ok(_) => "restart",
        Err("none") if applied.is_some() => "restart",
        Err("none") => "system",
        Err(why) => why,
    };
    Ok(NetworkReport { checks, webview_proxy })
}

#[cfg(target_os = "macos")]
fn macos_major() -> u32 {
    std::process::Command::new("sw_vers").arg("-productVersion").output().ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().split('.').next()?.parse().ok())
        .unwrap_or(0)
}

/// Serves the interface on http://localhost (YouTube's embedded player refuses tauri://). A local debug build
/// (`scripts/dev-app.sh`) does it too, so the embed can be tried before a release; it starts 10 ports up, to leave
/// the installed app its usual port (and with it its page storage), and never uses interface updates.
fn serve_ui(app: &AppHandle, active: ota::Active) -> u16 {
    let first = if cfg!(debug_assertions) { LOCAL_PORT + 10 } else { LOCAL_PORT };
    let Some((port, listeners)) = bind_ui_port(first) else {
        eprintln!("SubCast: no free port for the interface near {LOCAL_PORT}");
        return LOCAL_PORT;
    };
    if port != LOCAL_PORT {
        eprintln!("SubCast: port {LOCAL_PORT} is taken by another program; using {port}");
    }
    for listener in listeners {
        let Ok(server) = tiny_http::Server::from_listener(listener, None) else { continue };
        let (resolver, active) = (app.asset_resolver(), active.clone());
        std::thread::spawn(move || {
            for req in server.incoming_requests() {
                let path = req.url().split(['?', '#']).next().unwrap_or("/").to_string();
                let found = match ota::read(&active, &path) {
                    Some(file) => file.map(|(bytes, mime)| (bytes, mime.to_string(), None)),
                    None => resolver.get(path).map(|a| (a.bytes, a.mime_type, a.csp_header)),
                };
                let Some((bytes, mime, csp)) = found else {
                    let _ = req.respond(tiny_http::Response::empty(404));
                    continue;
                };
                let mut resp = tiny_http::Response::from_data(bytes);
                for (k, v) in [
                    ("Content-Type", Some(mime)),
                    ("Cache-Control", Some("no-cache".to_string())),
                    ("Content-Security-Policy", csp),
                ] {
                    if let Some(h) = v.and_then(|v| tiny_http::Header::from_bytes(k.as_bytes(), v.as_bytes()).ok()) {
                        resp.add_header(h);
                    }
                }
                let _ = req.respond(resp);
            }
        });
    }
    port
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UiInfo {
    shell: &'static str,
    build: Option<u64>,
}

#[tauri::command]
fn ui_ready(state: State<'_, AppState>) -> UiInfo {
    UiInfo { shell: ota::SHELL, build: if cfg!(debug_assertions) { None } else { ota::ready(&state.ctx.data) } }
}

#[tauri::command]
async fn refresh_config(app: AppHandle, state: State<'_, AppState>) -> Res<()> {
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u["pubkey"].as_str())
        .ok_or("缺少更新公钥")?
        .to_string();
    ops::refresh_config_with(&state.ctx, &pubkey, |pk, body, sig| ota::verify(pk, body, sig))
        .await
        .map(|_| ())
        .map_err(e)
}

#[tauri::command]
async fn ui_check(app: AppHandle, state: State<'_, AppState>) -> Res<String> {
    let pubkey = app
        .config()
        .plugins
        .0
        .get("updater")
        .and_then(|u| u["pubkey"].as_str())
        .ok_or("缺少更新公钥")?
        .to_string();
    Ok(match ota::check(&state.ctx.data, &pubkey).await.map_err(e)? {
        ota::Outcome::None => "none".into(),
        ota::Outcome::Staged(b) => format!("staged:{b}"),
        ota::Outcome::RolledBack => "rollback".into(),
    })
}

#[tauri::command]
async fn youtube_stream(
    app: AppHandle,
    state: State<'_, AppState>,
    video_id: String,
    mode: Option<String>,
    fresh: Option<bool>,
) -> Res<ops::VideoStream> {
    let sink = TauriSink(app);
    ops::youtube_stream(&state.ctx, &video_id, mode.as_deref(), fresh.unwrap_or(false), &sink)
        .await
        .map_err(e)
}

// Async on purpose: a window built inside a synchronous command deadlocks on Windows (WebView2 needs the main
// thread the command is blocking), leaving a black, frozen sign-in window.
#[tauri::command]
async fn youtube_sign_in(app: AppHandle) -> Res<()> {
    ytlogin::open(&app).map_err(e)
}

#[tauri::command]
async fn youtube_signed_in() -> Res<bool> {
    Ok(ytlogin::signed_in().await)
}

#[tauri::command]
async fn youtube_sign_out() -> Res<()> {
    ytlogin::sign_out().await.map_err(e)
}

#[tauri::command]
async fn prepare_youtube(state: State<'_, AppState>) -> Res<()> {
    ops::prepare_youtube(&state.ctx).await.map_err(e)
}

// ---- offline & storage -----------------------------------------------------------------------------

#[tauri::command]
async fn download_episode(app: AppHandle, state: State<'_, AppState>, episode_id: String) -> Res<offline::Saved> {
    let sink = TauriSink(app);
    ops::download_episode(&state.ctx, &episode_id, &sink).await.map_err(e)
}

#[tauri::command(async)]
fn list_downloads(state: State<'_, AppState>) -> Vec<offline::Saved> {
    offline::list(&state.ctx.data)
}

#[tauri::command(async)]
fn delete_download(state: State<'_, AppState>, episode_id: String) -> Res<()> {
    offline::remove(&state.ctx.data, &episode_id).map_err(e)
}

#[tauri::command(async)]
fn clear_downloads(state: State<'_, AppState>) -> Res<()> {
    offline::clear(&state.ctx.data).map_err(e)
}

#[tauri::command]
async fn full_notes(state: State<'_, AppState>, episode_id: String) -> Res<Option<String>> {
    ops::full_notes(&state.ctx, &episode_id).await.map_err(e)
}

#[tauri::command(async)]
fn storage_info(state: State<'_, AppState>) -> Value {
    ops::storage_info(&state.ctx)
}

#[tauri::command]
fn open_data_folder(app: AppHandle, state: State<'_, AppState>, kind: String) -> Res<()> {
    use tauri_plugin_opener::OpenerExt;
    let dir = match kind.as_str() {
        "downloads" => offline::dir(&state.ctx.data),
        "models" => asr::models_dir(&state.ctx.data),
        "tts-cache" => speech::cache_dir(&state.ctx.data),
        "tts-models" => local_tts::models_dir(&state.ctx.data),
        _ => state.ctx.data.clone(),
    };
    std::fs::create_dir_all(&dir).map_err(e)?;
    app.opener().open_path(dir.to_string_lossy(), None::<&str>).map_err(e)
}

#[tauri::command]
fn reveal_download(app: AppHandle, state: State<'_, AppState>, episode_id: String) -> Res<()> {
    use tauri_plugin_opener::OpenerExt;
    let saved = offline::find(&state.ctx.data, &episode_id).ok_or("没有找到下载的文件")?;
    app.opener().reveal_item_in_dir(&saved.path).map_err(e)
}

#[tauri::command(async)]
fn clear_temp(state: State<'_, AppState>) -> Res<()> {
    ops::clear_temp(&state.ctx).map_err(e)
}

// ---- alignment, shadowing, shared & asr ------------------------------------------------------------

#[tauri::command]
async fn align_audio(
    app: AppHandle,
    state: State<'_, AppState>,
    episode_id: String,
    force: bool,
    model: Option<String>,
) -> Res<ops::Aligned> {
    let sink = TauriSink(app);
    ops::align_audio(&state.ctx, &episode_id, force, model.as_deref(), &sink)
        .await
        .map_err(e)
}

#[tauri::command]
fn shadow_start(app: AppHandle) -> Res<()> {
    shadow::start(move |peak| {
        let _ = app.emit("shadow-level", peak);
    })
    .map_err(e)
}

#[tauri::command]
fn shadow_stop() {
    shadow::stop();
}

#[tauri::command]
fn shadow_clear() {
    shadow::clear();
}

#[tauri::command]
fn shadow_ready(state: State<'_, AppState>, model: String) -> bool {
    shadow::ready(&state.ctx.data, &model)
}

#[tauri::command]
async fn shadow_hear(state: State<'_, AppState>, model: String, language: String) -> Res<String> {
    shadow::hear(&state.ctx.data, &model, &language).await.map_err(e)
}

#[tauri::command]
fn open_mic_settings() {
    shadow::open_mic_settings();
}

#[tauri::command]
fn save_time_map(state: State<'_, AppState>, episode_id: String, map: align::TimeMap) -> Res<()> {
    db::set_blob(&state.ctx.db.lock().unwrap(), &episode_id, "tmap", &serde_json::to_string(&map).map_err(e)?)
        .map_err(e)
}

/// A loopback address for an episode's audio, for when the web view can't load the original (see hlsproxy).
#[tauri::command]
async fn audio_stream(url: String) -> Res<String> {
    subcast_core::hlsproxy::audio(&url).await.map_err(e)
}

#[tauri::command]
fn list_audio_cache(state: State<'_, AppState>) -> Vec<offline::Saved> {
    offline::list_cached(&state.ctx.data)
}

#[tauri::command]
fn clear_audio_cache(state: State<'_, AppState>) -> Res<()> {
    offline::clear_cache(&state.ctx.data).map_err(e)
}

#[tauri::command]
async fn shared_transcript(state: State<'_, AppState>, episode_id: String) -> Res<Option<Transcript>> {
    ops::shared_transcript(&state.ctx, &episode_id).await.map_err(e)
}

#[tauri::command]
async fn shared_exists(state: State<'_, AppState>, episode_ids: Vec<String>) -> Res<Vec<String>> {
    ops::shared_exists(&state.ctx, &episode_ids).await.map_err(e)
}

#[tauri::command]
async fn transcribe(
    app: AppHandle,
    state: State<'_, AppState>,
    episode_id: String,
    model: String,
    language: String,
    cloud: Option<cloud::CloudAsr>,
) -> Res<Transcript> {
    let sink: Arc<dyn ProgressSink> = Arc::new(TauriSink(app));
    ops::transcribe(&state.ctx, &episode_id, &model, &language, cloud, false, false, sink)
        .await
        .map_err(e)
}

#[tauri::command]
async fn share_translation(state: State<'_, AppState>, episode_id: String, engine: String, model: String) -> Res<String> {
    ops::share_translation(&state.ctx, &episode_id, &engine, &model).await.map_err(e)
}

#[tauri::command]
async fn shared_guide(state: State<'_, AppState>, episode_id: String) -> Res<Option<String>> {
    ops::shared_guide(&state.ctx, &episode_id).await.map_err(e)
}

#[tauri::command]
async fn share_guide(state: State<'_, AppState>, episode_id: String, text: String, model: String) -> Res<String> {
    ops::share_guide(&state.ctx, &episode_id, &text, &model).await.map_err(e)
}

#[tauri::command]
fn cancel_job(state: State<'_, AppState>, id: String) {
    state.ctx.cancel(&id);
}

#[tauri::command]
async fn asr_models(state: State<'_, AppState>) -> Res<Vec<asr::ModelInfo>> {
    let data = state.ctx.data.clone();
    tokio::task::spawn_blocking(move || asr::list_models(&data)).await.map_err(e)
}

#[tauri::command]
async fn download_model(app: AppHandle, state: State<'_, AppState>, id: String, mirror_first: bool) -> Res<()> {
    let sink = TauriSink(app);
    ops::download_model(&state.ctx, &id, mirror_first, &sink).await.map_err(e)
}

#[tauri::command(async)]
fn log_listen(state: State<'_, AppState>, episode_id: String, show_id: String, seconds: f64, translated: bool) -> Res<()> {
    db::log_listen(&state.ctx.db.lock().unwrap(), &episode_id, &show_id, seconds, translated).map_err(e)
}

#[tauri::command(async)]
fn listening_stats(state: State<'_, AppState>, since: String) -> Res<Vec<Value>> {
    db::listens_since(&state.ctx.db.lock().unwrap(), &since).map_err(e)
}

#[tauri::command]
fn asr_elsewhere() -> Vec<asr::Elsewhere> {
    asr::elsewhere()
}

#[tauri::command]
fn rescan_models() {
    asr::rescan();
}

#[tauri::command]
fn delete_model(state: State<'_, AppState>, id: String) -> Res<()> {
    ops::delete_model(&state.ctx, &id).map_err(e)
}

// ---- stored data, llm, agents & settings -----------------------------------------------------------

#[tauri::command(async)]
fn get_blob(state: State<'_, AppState>, episode_id: String, kind: String) -> Res<Option<String>> {
    db::get_blob(&state.ctx.db.lock().unwrap(), &episode_id, &kind).map_err(e)
}

#[tauri::command(async)]
fn set_blob(state: State<'_, AppState>, episode_id: String, kind: String, data: String) -> Res<()> {
    db::set_blob(&state.ctx.db.lock().unwrap(), &episode_id, &kind, &data).map_err(e)
}

#[tauri::command(async)]
fn get_settings(state: State<'_, AppState>) -> Res<Value> {
    ops::get_settings(&state.ctx).map_err(e)
}

#[tauri::command(async)]
fn set_settings(state: State<'_, AppState>, settings: Value) -> Res<()> {
    ops::set_settings(&state.ctx, &settings).map_err(e)
}

#[tauri::command]
fn set_api_key(provider: String, key: String) -> Res<()> {
    llm::set_key(&provider, key.trim()).map_err(e)
}

#[tauri::command(async)]
fn has_api_key(provider: String) -> bool {
    llm::get_key(&provider).is_some_and(|k| !k.is_empty())
}

#[tauri::command]
async fn llm_chat(
    app: AppHandle,
    state: State<'_, AppState>,
    request_id: String,
    provider: llm::Provider,
    system: String,
    messages: Vec<llm::Message>,
    max_tokens: Option<u32>,
    json_mode: Option<bool>,
    quick: Option<bool>,
) -> Res<String> {
    let sink = TauriSink(app);
    ops::llm_chat(&state.ctx, &request_id, &provider, &system, &messages, max_tokens, json_mode, quick, &sink)
        .await
        .map_err(e)
}

#[tauri::command]
async fn youtube_subscriptions(state: State<'_, AppState>, browser: String) -> Res<Vec<import::Channel>> {
    if !matches!(browser.as_str(), "chrome" | "safari" | "firefox" | "edge" | "brave" | "chromium" | "opera" | "vivaldi") {
        return Err("不支持这个浏览器".into());
    }
    let bin = asr::ytdlp(&state.ctx.data).await.map_err(e)?;
    import::youtube_subscriptions(&bin, &browser).await.map_err(e)
}

#[tauri::command]
fn open_privacy_settings() -> Res<()> {
    import::open_privacy_settings().map_err(e)
}

#[tauri::command]
fn export_opml(state: State<'_, AppState>) -> Res<String> {
    ops::export_opml(&state.ctx, None).map_err(e)
}

#[tauri::command]
async fn local_agents() -> Vec<agents::Found> {
    agents::detect().await
}

#[tauri::command]
async fn probe_agent(kind: String) -> Res<String> {
    let k = agents::Kind::parse(&kind).ok_or("未知的本机 AI")?;
    let path = agents::path_of(k).ok_or("没有找到")?;
    agents::probe(k, &path).await.map(str::to_string).map_err(e)
}

#[tauri::command]
async fn agent_models(kind: String) -> Res<Vec<agents::AgentModel>> {
    let k = agents::Kind::parse(&kind).ok_or("未知的本机 AI")?;
    let path = agents::path_of(k).ok_or("没有找到")?;
    agents::models(k, &path).await.map_err(e)
}

#[tauri::command]
async fn app_update_check(app: AppHandle) -> Res<Option<appupdate::Found>> {
    appupdate::check(&app).await.map_err(e)
}

#[tauri::command]
async fn app_update_download(app: AppHandle) -> Res<()> {
    appupdate::download(&app).await.map_err(e)
}

#[tauri::command]
fn app_update_install(app: AppHandle) -> Res<()> {
    appupdate::install(&app).map_err(e)
}

#[tauri::command]
async fn warm_agent(kind: String) {
    if let Some(k) = agents::Kind::parse(&kind) {
        if let Some(p) = agents::path_of(k) {
            agents::warm(k, &p).await;
        }
    }
}

#[tauri::command]
async fn translate_texts(state: State<'_, AppState>, texts: Vec<String>, target: Option<String>) -> Res<Vec<String>> {
    ops::translate_texts(&state.ctx, &texts, target.as_deref()).await.map_err(e)
}

#[tauri::command]
fn db_data_version(state: State<'_, AppState>) -> Res<i64> {
    db::data_version(&state.ctx.db.lock().unwrap()).map_err(e)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CliStatus {
    bundled: bool,
    bundled_path: Option<String>,
    installed: bool,
    link_path: Option<String>,
    target_path: Option<String>,
    needs_update: bool,
    /// A new terminal finds the command (its PATH, not this app's).
    in_path: bool,
    /// Where SubCast just put the command's folder on the PATH (a startup file, or "PATH" on Windows).
    path_added: Option<String>,
}

fn bundled_cli_path() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let name = if cfg!(windows) { "subcast-cli.exe" } else { "subcast-cli" };
    let candidate = dir.join(name);
    candidate.is_file().then_some(candidate)
}

fn cli_install_candidates() -> Vec<std::path::PathBuf> {
    let mut list = Vec::new();
    #[cfg(unix)]
    {
        list.push(std::path::PathBuf::from("/usr/local/bin/subcast"));
        if let Some(h) = dirs::home_dir() {
            list.push(h.join(".local/bin/subcast"));
        }
    }
    #[cfg(windows)]
    {
        if let Some(local) = dirs::data_local_dir() {
            list.push(local.join("SubCast").join("bin").join("subcast.exe"));
        }
    }
    list
}

fn files_identical(a: &std::path::Path, b: &std::path::Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else { return false };
    if ma.len() != mb.len() { return false; }
    std::fs::read(a).ok().zip(std::fs::read(b).ok()).is_some_and(|(x, y)| x == y)
}

fn inspect_cli_status() -> CliStatus {
    let bundled = bundled_cli_path();
    let bundled_canon = bundled.as_ref().and_then(|p| std::fs::canonicalize(p).ok());
    let mut installed = false;
    let mut link_path = None;
    let mut target_path = None;
    let mut needs_update = false;
    let mut in_path = false;

    for cand in cli_install_candidates() {
        if let Ok(meta) = std::fs::symlink_metadata(&cand) {
            if meta.file_type().is_symlink() || meta.is_file() {
                installed = true;
                link_path = Some(cand.to_string_lossy().into_owned());
                let resolved = std::fs::canonicalize(&cand).ok();
                target_path = resolved.as_ref().map(|p| p.to_string_lossy().into_owned());
                if let Some(b) = bundled.as_ref() {
                    if meta.file_type().is_symlink() {
                        needs_update = bundled_canon.as_ref() != resolved.as_ref();
                    } else if cfg!(windows) {
                        // On Windows, `install_cli_link` copies subcast-cli.exe into %LOCALAPPDATA%\SubCast\bin\subcast.exe.
                        needs_update = !files_identical(b, &cand);
                    } else {
                        needs_update = bundled_canon.as_ref() != resolved.as_ref();
                    }
                }
                if let Some(parent) = cand.parent() {
                    in_path = clipath::in_path(parent);
                }
                break;
            }
        }
    }

    CliStatus {
        bundled: bundled.is_some(),
        bundled_path: bundled.map(|p| p.to_string_lossy().into_owned()),
        installed,
        link_path,
        target_path,
        needs_update,
        in_path,
        path_added: None,
    }
}

/// After the command is installed: its folder goes on the PATH new terminals use, when it isn't there yet.
fn with_path(mut status: CliStatus) -> CliStatus {
    if status.installed && !status.in_path {
        if let Some(dir) = status.link_path.as_deref().and_then(|l| std::path::Path::new(l).parent()) {
            status.path_added = clipath::ensure(dir);
            if status.path_added.is_some() { status.in_path = true; }
        }
    }
    status
}

// Async: finding the PATH asks the listener's shell, which can take a few seconds.
#[tauri::command]
async fn cli_status() -> Res<CliStatus> {
    tokio::task::spawn_blocking(inspect_cli_status).await.map_err(e)
}

fn bundled_cli() -> Res<std::path::PathBuf> {
    bundled_cli_path().ok_or_else(|| "当前安装包里没有附带命令行程序（subcast-cli）".to_string())
}

#[tauri::command]
async fn mcp_targets() -> Res<Vec<subcast_core::mcp_targets::McpTarget>> {
    let cli = bundled_cli()?;
    Ok(subcast_core::mcp_targets::list(&cli))
}

#[tauri::command]
async fn mcp_connect(id: String) -> Res<subcast_core::mcp_targets::McpTarget> {
    let cli = bundled_cli()?;
    subcast_core::mcp_targets::connect(&id, &cli).await.map_err(e)
}

#[tauri::command]
async fn mcp_disconnect(id: String) -> Res<subcast_core::mcp_targets::McpTarget> {
    let cli = bundled_cli()?;
    subcast_core::mcp_targets::disconnect(&id, &cli).await.map_err(e)
}

#[tauri::command]
async fn cli_install() -> Res<CliStatus> {
    tokio::task::spawn_blocking(|| install_cli_link().map(with_path)).await.map_err(e)?
}

/// Puts the `subcast` command on the PATH by itself, so it works right after installing the app: once on the first
/// launch (a removed link stays removed after that), and on every launch a link that points at an older copy of the
/// app is pointed at this one. Never replaces a file that isn't our link.
#[cfg(not(debug_assertions))]
fn auto_install_cli(data: &std::path::Path) {
    let status = inspect_cli_status();
    if !status.bundled { return; }
    let marker = data.join("cli-auto-installed");
    let first = !marker.exists();
    let ours_outdated = status.installed && status.needs_update && (
        cfg!(windows) || status.link_path.as_deref().is_some_and(|l| std::fs::symlink_metadata(l).is_ok_and(|m| m.file_type().is_symlink()))
    );
    if (first && !status.installed) || ours_outdated {
        let _ = install_cli_link();
    }
    if first { let _ = std::fs::write(&marker, b"1"); }
    // Once per install (a line removed afterwards stays removed): the command's folder on new terminals' PATH.
    // Covers copies installed before SubCast did this, too.
    let path_marker = data.join("cli-path-checked");
    if !path_marker.exists() {
        let now = inspect_cli_status();
        let ours = now.target_path.is_some() && !now.needs_update;
        if ours { let _ = with_path(now); }
        let _ = std::fs::write(&path_marker, b"1");
    }
    // Agents connected to SubCast keep working after the app moves: their entries follow it.
    if let Some(cli) = bundled_cli_path() { tauri::async_runtime::block_on(subcast_core::mcp_targets::repair(&cli)); }
}

fn install_cli_link() -> Res<CliStatus> {
    let bundled = bundled_cli_path().ok_or_else(|| "当前安装包里没有附带命令行程序（subcast-cli）".to_string())?;
    #[cfg(unix)]
    {
        let mut last_err: Option<String> = None;
        for cand in cli_install_candidates() {
            if let Some(parent) = cand.parent() {
                if !parent.exists() {
                    if let Err(err) = std::fs::create_dir_all(parent) {
                        last_err = Some(err.to_string());
                        continue;
                    }
                }
            }
            if let Ok(meta) = std::fs::symlink_metadata(&cand) {
                if meta.file_type().is_symlink() {
                    if let Err(err) = std::fs::remove_file(&cand) {
                        last_err = Some(err.to_string());
                        continue;
                    }
                } else {
                    last_err = Some(format!("{} 已存在且不是软链接，已跳过以免覆盖其他程序", cand.display())); // i18n-ignore
                    continue;
                }
            }
            match std::os::unix::fs::symlink(&bundled, &cand) {
                Ok(()) => return Ok(inspect_cli_status()),
                Err(err) => last_err = Some(err.to_string()),
            }
        }
        return Err(last_err.unwrap_or_else(|| "安装命令行工具失败".to_string()));
    }
    #[cfg(windows)]
    {
        let dest = cli_install_candidates().into_iter().next().ok_or("找不到本地安装目录")?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(e)?;
        }
        std::fs::copy(&bundled, &dest).map_err(e)?;
        Ok(inspect_cli_status())
    }
}

#[tauri::command]
fn set_menu_labels(app: AppHandle, labels: Vec<String>) -> Res<()> {
    let Some(menu) = app.menu() else { return Ok(()) };
    for sub in menu.items().map_err(e)? {
        let Some(sub) = sub.as_submenu() else { continue };
        if let Some(item) = sub.get("settings").and_then(|i| i.as_menuitem().cloned()) {
            if let Some(l) = labels.first() {
                item.set_text(l).map_err(e)?;
            }
        }
    }
    Ok(())
}

// ---- app ------------------------------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--asr-worker") {
        std::process::exit(asr::worker_main(&args[2..]));
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_process::init())
        .setup(|app| {
            #[cfg(desktop)]
            app.handle().plugin(tauri_plugin_updater::Builder::new().build())?;
            ytlogin::init(app.handle());
            asr::set_cookie_provider(Box::new(|| Box::pin(ytlogin::cookie_file())));
            let data = app.path().app_data_dir()?;
            if let Ok(res) = app.path().resource_dir() {
                let _ = asr::VCRT_DIR.set(res.join("vcrt"));
            }
            let ctx = Arc::new(CoreCtx::open(Some(data.clone()))?);
            // `tauri dev` loads the interface from the dev server (already http://localhost); every built app serves
            // its own. Debug builds skip interface updates: they show the interface they were built with.
            let (ui, ui_port): (ota::Active, u16) = if tauri::is_dev() {
                (Arc::new(std::sync::RwLock::new(None)), 0)
            } else {
                let active = Arc::new(std::sync::RwLock::new(if cfg!(debug_assertions) { None } else { ota::boot(&data) }));
                let port = serve_ui(app.handle(), active.clone());
                (active, port)
            };
            #[cfg(debug_assertions)]
            let _ = &ui;
            #[cfg(not(debug_assertions))]
            let ui_data = data.clone();
            app.manage(AppState { ctx });
            #[cfg(not(debug_assertions))]
            {
                let data = data.clone();
                std::thread::spawn(move || auto_install_cli(&data));
            }
            let url = if tauri::is_dev() {
                tauri::WebviewUrl::App("index.html".into())
            } else {
                tauri::WebviewUrl::External(format!("http://localhost:{ui_port}").parse().unwrap())
            };
            let builder = tauri::WebviewWindowBuilder::new(app, "main", url)
                .title("SubCast")
                .inner_size(1360.0, 880.0)
                .min_inner_size(980.0, 640.0);
            let builder = match webview_proxy() { Some(p) => builder.proxy_url(p), None => builder };
            #[cfg(target_os = "macos")]
            let builder = builder
                .title_bar_style(tauri::TitleBarStyle::Overlay)
                .hidden_title(true)
                .traffic_light_position(tauri::LogicalPosition::new(18.0, 22.0));
            builder.build()?;
            #[cfg(not(debug_assertions))]
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(ota::READY_SECS)).await;
                    if ota::give_up(&ui_data, &ui) {
                        if let Some(w) = handle.get_webview_window("main") {
                            let _ = w.eval("location.reload()");
                        }
                    }
                });
            }
            let menu = tauri::menu::Menu::default(app.handle())?;
            if let Some(first) = menu.items()?.first().and_then(|m| m.as_submenu().cloned()) {
                let item = tauri::menu::MenuItem::with_id(app, "settings", "设置…", true, Some("CmdOrCtrl+,"))?;
                let sep = tauri::menu::PredefinedMenuItem::separator(app)?;
                first.insert_items(&[&sep, &item], 1)?;
            }
            app.set_menu(menu)?;
            Ok(())
        })
        .on_menu_event(|app, event| {
            if event.id() == "settings" {
                let _ = app.emit("open-settings", ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            add_subscription, refresh_show, refresh_all, list_shows, list_episodes, recent_episodes, get_episode, remove_show,
            set_progress, set_watched, mark_show_watched, search_podcasts, search_show, load_transcript, fetch_official_transcript, transcribe, cancel_job, asr_models,
            download_model, delete_model, get_blob, set_blob, get_settings, set_settings, set_api_key, has_api_key, llm_chat, translate_texts, shared_transcript, shared_exists, shared_guide, share_guide, asr_elsewhere, log_listen, listening_stats, rescan_models, youtube_subscriptions, open_privacy_settings, export_opml, local_agents, probe_agent, agent_models, warm_agent, share_translation, ui_ready, ui_check, refresh_config, youtube_stream, prepare_youtube, youtube_sign_in, youtube_signed_in, youtube_sign_out, network_check, youtube_tracks, use_youtube_track, youtube_channel_feed, qiaomu_detail, preview_show, list_models, tts_speak, cached_show, save_image, learn_clip, save_text, export_anki, set_menu_labels,
            app_update_check, app_update_download, app_update_install,
            shadow_start, shadow_stop, shadow_clear, shadow_ready, shadow_hear, open_mic_settings,
            download_episode, list_downloads, delete_download, clear_downloads, align_audio, save_time_map, list_audio_cache, audio_stream, clear_audio_cache, storage_info, open_data_folder, reveal_download, clear_temp, full_notes, tts_synthesize, tts_system_voices, clear_tts_cache, local_tts_status, local_tts_install, local_tts_delete,
            video_clip_env, video_clip_warm, video_clip_prepare, video_clip_render, reveal_file,
            db_data_version, cli_status, cli_install, mcp_targets, mcp_connect, mcp_disconnect
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod live {
    //! Network tests against real services: `cargo test -- --ignored --nocapture`.
    use super::*;
    use subcast_core::{now_ms, qiaomu, shared, transcript, youtube};

    #[tokio::test]
    #[ignore]
    async fn qiaomu_rss_sources() {
        let ids = ["nexttoken", "bannatie", "latentspace", "podscribe-acquired", "podscribe-pivot", "podscribe-freakonomics-radio", "podscribe-invest-like-the-best", "podscribe-the-diary-of-a-ceo-with-steven-bartlett"];
        for id in ids {
            let ch = qiaomu::rss_channel(id).await.unwrap();
            let playable = |x: &Episode| x.audio_url.is_some() || x.video_id.is_some();
            println!("{id}: {} eps={} playable={} video={} deep={} cover={}", ch.show.title, ch.episodes.len(), ch.episodes.iter().filter(|x| playable(x)).count(), ch.episodes.iter().filter(|x| x.video_id.is_some()).count(), ch.episodes.iter().filter(|x| x.deep).count(), ch.show.cover.is_some());
            if id == "nexttoken" { assert!(ch.episodes.iter().filter(|x| x.video_id.is_some()).count() * 2 >= ch.episodes.len(), "nexttoken episodes should mostly have the YouTube video"); }
            assert!(!id.starts_with("podscribe-") || ch.episodes.iter().all(|x| x.transcript_url.is_some()), "{id}: podscribe episodes need a transcript");
            assert!(id.starts_with("podscribe-") || ch.episodes.iter().all(playable), "{id}: unplayable entries should be skipped");
            if id.starts_with("podscribe-") {
                let pool = qiaomu::borrowable(id, &ch.show.title).await;
                for x in ch.episodes.iter().take(6) {
                    let m = qiaomu::find_audio(&pool, &x.title, x.published).map(|b| b.title.chars().take(60).collect::<String>());
                    println!("  {:<62} -> {:?}", x.title.chars().take(60).collect::<String>(), m);
                }
            }
            let Some(e) = ch.episodes.iter().find(|x| x.deep) else { continue };
            let rw = qiaomu::rss_rewrite(&e.guid).await.unwrap();
            assert!(rw.is_some(), "{id}: rewriteReady entry has no rewrite");
            println!("  rewrite={}", rw.as_ref().map(|h| h.chars().take(120).collect::<String>()).unwrap_or_default());
            if let Some(u) = &e.transcript_url { let s = qiaomu::podscribe_transcript(u).await.unwrap(); println!("  segs={} {:?}", s.len(), s.get(1).map(|x| (x.start, x.text.chars().take(40).collect::<String>()))); }
        }
    }
    #[tokio::test]
    #[ignore]
    async fn qiaomu_channels() {
        for id in ["lex", "nopriors", "zhangxiaojun"] {
            let ch = qiaomu::channel(id).await.unwrap();
            let ep = &ch.episodes[0];
            println!("{id}: {} | cover={:?} | eps={} | zh={} | first={} video={:?}", ch.show.title, ch.show.cover, ch.episodes.len(), ch.zh_titles.len(), ep.title, ep.video_id);
            let d = qiaomu::detail(ep.link.as_ref().unwrap()).await.unwrap();
            println!("  audio={:?} deep={} transcript={}", d.audio, d.deep_read.as_ref().map(|h| h.len()).unwrap_or(0), d.transcript.as_ref().map(|t| t.len()).unwrap_or(0));
            if let Some(t) = &d.transcript { let segs = qiaomu::transcript_segments(t, ep.duration.unwrap_or(3600.0)); println!("  segs={} first={:?}", segs.len(), segs.first().map(|s| s.text.chars().take(80).collect::<String>())); }
        }
    }
    #[tokio::test]
    #[ignore]
    async fn sources() {
        for input in ["https://www.youtube.com/@DwarkeshPatel", "https://podcasts.apple.com/us/podcast/no-priors/id1668002688", "https://www.xiaoyuzhoufm.com/podcast/5e5c52c9418a84a04625e6cc", "https://feeds.megaphone.fm/nopriors"] {
            let (show, eps) = match feeds::resolve(input).await.unwrap() {
                feeds::Source::Feed(u) => feeds::fetch_show(&u, "podcast").await.unwrap(),
                feeds::Source::Xiaoyuzhou(u) => feeds::xiaoyuzhou_show(&u).await.unwrap(),
                _ => panic!("unexpected source for {input}"),
            };
            println!("{input} -> {} [{}] {} episodes; first: {:?} audio={:?} video={:?} transcript={:?}", show.title, show.kind, eps.len(), eps[0].title, eps[0].audio_url.is_some(), eps[0].video_id, eps[0].transcript_url);
            assert!(!eps.is_empty());
        }
        let r = feeds::search_podcasts("lex fridman", "US").await.unwrap();
        println!("search: {} results, first {}", r.len(), r[0].title);
    }
    #[tokio::test]
    #[ignore]
    async fn discover_picks() {
        let inputs = [
            "https://www.xiaoyuzhoufm.com/podcast/5e4ee557418a84a0466737b7", "https://www.xiaoyuzhoufm.com/podcast/5e2831ed418a84a046231c00",
            "https://www.xiaoyuzhoufm.com/podcast/5f72b66083c34e85dd14fde9", "https://www.xiaoyuzhoufm.com/podcast/5e4515bd418a84a046e2b11a",
            "https://www.xiaoyuzhoufm.com/podcast/611719d3cb0b82e1df0ad29e", "https://www.xiaoyuzhoufm.com/podcast/6022a180ef5fdaddc30bb101",
            "https://www.xiaoyuzhoufm.com/podcast/61dd99a47b29652ff572257b", "https://www.xiaoyuzhoufm.com/podcast/61358d971c5d56efe5bcb5d2",
            "https://www.xiaoyuzhoufm.com/podcast/5e74b52c418a84a046ecaceb", "https://www.xiaoyuzhoufm.com/podcast/5e280faf418a84a0461fbd39",
            "https://www.xiaoyuzhoufm.com/podcast/646d6bfa53a5e5ea14e69c7c", "https://www.xiaoyuzhoufm.com/podcast/64379296ff8a107611a67208",
            "https://www.xiaoyuzhoufm.com/podcast/6627fda4b56459544087d86a", "https://www.xiaoyuzhoufm.com/podcast/68981df29e7bcd326eb91d88",
            "https://www.xiaoyuzhoufm.com/podcast/5e285856418a84a04627b7be", "https://www.xiaoyuzhoufm.com/podcast/62694abdb221dd5908417d1e",
            "https://www.youtube.com/@hubermanlab", "https://www.youtube.com/@MachineLearningStreetTalk", "https://www.youtube.com/@Bg2Pod",
            "https://www.youtube.com/@MyFirstMillionPod", "https://www.youtube.com/@ChrisWillx", "https://www.youtube.com/@timferriss",
            "https://www.youtube.com/@Alex.kantrowitz", "https://www.youtube.com/@tkppodcast", "https://www.youtube.com/@cheekypint",
            "https://www.youtube.com/@PeterAttiaMD", "https://www.youtube.com/@peterdiamandis", "https://www.youtube.com/@EveryInc",
        ];
        let mut bad = vec![];
        for input in inputs {
            let r = match feeds::resolve(input).await {
                Ok(feeds::Source::Feed(u)) => feeds::fetch_show(&u, "podcast").await,
                Ok(feeds::Source::Xiaoyuzhou(u)) => feeds::xiaoyuzhou_show(&u).await,
                Ok(_) => Err(anyhow::anyhow!("unexpected source")),
                Err(err) => Err(err),
            };
            match r {
                Ok((show, eps)) => {
                    let newest = eps.iter().filter_map(|x| x.published).max().unwrap_or(0);
                    let days = (now_ms() - newest) / 86_400_000;
                    let playable = eps.iter().filter(|x| x.audio_url.is_some() || x.video_id.is_some()).count();
                    println!("{input}\n  -> {} | eps={} playable={} newest={}d ago | cover={:?}", show.title, eps.len(), playable, days, show.cover);
                    if eps.is_empty() || playable == 0 { bad.push(input); }
                }
                Err(err) => { println!("{input}\n  !! {err}"); bad.push(input); }
            }
        }
        assert!(bad.is_empty(), "failed: {bad:?}");
    }
    #[tokio::test]
    #[ignore]
    async fn first_stream_reports_the_download() {
        let data = std::env::temp_dir().join(format!("subcast-ytdlp-{}", uuid::Uuid::new_v4()));
        let mut seen = 0.0f64;
        let (a, b) = tokio::join!(asr::ytdlp_with(&data, |got, total| seen = total.map_or(0.0, |t| got as f64 / t as f64)), asr::ytdlp(&data));
        assert_eq!(a.unwrap(), b.unwrap());
        assert!(seen > 0.99, "progress reached {seen}");
        let url = asr::with_ytdlp(&data, |b| async move { asr::youtube_stream_url(&b, "tBMlXQIrYK4", true, true).await }).await.unwrap();
        assert!(url.contains("m3u8") || url.contains("hls_"));
        let mp4 = asr::with_ytdlp(&data, |b| async move { asr::youtube_stream_url(&b, "tBMlXQIrYK4", false, true).await }).await.unwrap();
        assert!(mp4.contains("googlevideo.com/videoplayback"), "{mp4}");
        let _ = std::fs::remove_dir_all(&data);
    }
    #[tokio::test]
    #[ignore]
    async fn ytdlp_stays_current() {
        let data = std::env::temp_dir().join(format!("subcast-ytdlp-{}", uuid::Uuid::new_v4()));
        let bin = asr::ytdlp(&data).await.unwrap();
        assert!(bin.starts_with(&data), "uses its own copy");
        std::fs::remove_file(data.join("bin/yt-dlp.checked")).ok();
        assert!(!asr::update_ytdlp(&data, false).await.unwrap());
        assert!(!asr::update_ytdlp(&data, false).await.unwrap());
        let url = asr::with_ytdlp(&data, |b| async move { asr::youtube_stream_url(&b, "dQw4w9WgXcQ", true, true).await }).await.unwrap();
        println!("stream: {}", &url[..url.len().min(80)]);
        let _ = std::fs::remove_dir_all(&data);
    }
    #[test]
    fn ui_port_skips_a_port_taken_on_ipv6() {
        let base = 45_000 + (std::process::id() % 1000) as u16 * 10;
        let Ok(_other) = std::net::TcpListener::bind(("::1", base)) else { return };
        let (port, listeners) = super::bind_ui_port(base).expect("a free port");
        assert_eq!(port, base + 1);
        assert_eq!(listeners.len(), 2);
        let (port2, _) = super::bind_ui_port(base + 5).expect("a free port");
        assert_eq!(port2, base + 5);
    }
    #[test]
    fn every_ui_port_is_allowed() {
        let caps = include_str!("../capabilities/default.json");
        for p in super::LOCAL_PORT..super::LOCAL_PORT + super::UI_PORTS {
            assert!(caps.contains(&format!("\"http://localhost:{p}\"")), "port {p} missing from capabilities");
        }
    }
    #[tokio::test]
    #[ignore]
    async fn shared_library_hits() {
        let conn = rusqlite::Connection::open(std::env::var("SUBCAST_DB").unwrap()).unwrap();
        let ids: Vec<(String, String)> = conn.prepare("SELECT episode_id, source FROM transcripts WHERE source NOT LIKE 'youtube%'").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
        for (id, source) in ids {
            let Some(ep) = db::get_episode(&conn, &id).unwrap() else { continue };
            let hit = shared::lookup(&shared::keys(&ep)).await;
            println!("{source:>14} {:<40} shared: {}", ep.title.chars().take(40).collect::<String>(),
                hit.map(|h| format!("{} lines, zh {}", h.segments.len(), h.translation.map(|t| t.items.len()).unwrap_or(0))).unwrap_or("none".into()));
        }
    }
    #[test]
    #[ignore]
    fn search_inside_a_show() {
        let conn = rusqlite::Connection::open(std::env::var("SUBCAST_DB").unwrap()).unwrap();
        let shows: Vec<(String, String)> = conn.prepare("SELECT s.id, s.title FROM shows s WHERE EXISTS(SELECT 1 FROM transcripts t JOIN episodes e ON e.id=t.episode_id WHERE e.show_id=s.id)").unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().map(Result::unwrap).collect();
        for (id, title) in shows.iter().take(4) {
            for q in ["AI", "黄仁勋", "agent model", "the"] {
                let t0 = std::time::Instant::now();
                let hits = ops::search_episodes(&conn, id, q).unwrap();
                let first = hits.first().map(|h| format!("{} [{}] {}", h.field, h.time.map(|t| t as i64).unwrap_or(-1), h.snippet)).unwrap_or_default();
                println!("{title} / {q}: {} hits in {:?}; {first}", hits.len(), t0.elapsed());
            }
        }
    }
    #[tokio::test]
    #[ignore]
    async fn chinese_show_opens_in_chinese() {
        let extra = std::env::var("YT_IDS").unwrap_or_default();
        for id in ["tBMlXQIrYK4", "1NHPDzpfR6Y", "2hJkprFnzhY"].into_iter().chain(extra.split_whitespace()) {
            let info = youtube::video_info(id).await.unwrap();
            let Some(t) = youtube::choose(&info.tracks, info.original_language.as_deref(), youtube::script_hint(&info.title)) else { println!("{id}: no captions"); continue };
            let zh = youtube::official_chinese(&info.tracks, &t.language).map(|z| z.language.clone());
            let langs: Vec<String> = info.tracks.iter().map(|t| format!("{}{}", t.language, if t.auto { "*" } else { "" })).collect();
            println!("{id} {} → {} official_zh={zh:?} tracks={langs:?}", info.title, t.language);
            if id == "tBMlXQIrYK4" { assert!(t.language.starts_with("zh")); }
            if let Some(z) = youtube::official_chinese(&info.tracks, &t.language) {
                let orig = transcript::resegment(transcript::clean(youtube::fetch_track(t).await.unwrap()));
                let items = youtube::align(&orig, &transcript::clean(youtube::fetch_track(z).await.unwrap()));
                println!("  coverage {}/{}; {:?} / {:?}", items.iter().filter(|x| x.is_some()).count(), items.len(), orig[3].text, items[3]);
            }
        }
    }
    #[tokio::test]
    #[ignore]
    async fn captions() {
        let info = youtube::video_info("dQw4w9WgXcQ").await.unwrap();
        let t = youtube::choose(&info.tracks, info.original_language.as_deref(), youtube::script_hint(&info.title)).unwrap();
        let segs = transcript::resegment(transcript::clean(youtube::fetch_track(t).await.unwrap()));
        println!("{} lang={} auto={} {} blocks; {:?}", info.title, t.language, t.auto, segs.len(), &segs[..2]);
        assert!(segs.len() > 5);
    }
    #[tokio::test]
    #[ignore]
    async fn qwen3_pipeline() {
        let dir = std::path::PathBuf::from(std::env::var("SUBCAST_TEST_DIR").unwrap());
        let data = dir.join("data");
        let flag = std::sync::Arc::new(AtomicBool::new(false));
        let bin = match asr::find_crispasr(&data) { Some(b) => b, None => asr::install_crispasr(&data, &flag, |_, _| {}).await.unwrap() };
        let wav = dir.join("t16k.wav");
        asr::write_wav(&wav, &asr::decode_16k_mono(&dir.join("clip.mp3")).unwrap()).unwrap();
        let ctx = asr::AsrContext { background: std::env::var("SUBCAST_TEST_CONTEXT").unwrap_or_default(), ..Default::default() };
        let segs = asr::run_crispasr(&bin, &dir.join("qwen3-asr-0.6b-q8_0.gguf"), &wav, &std::env::var("SUBCAST_TEST_LANG").unwrap_or("auto".into()), &ctx, flag, |p| println!("progress {p}")).await.unwrap();
        for s in &segs { println!("[{:.1}-{:.1}] {}", s.start, s.end, s.text); }
        assert!(segs.len() >= 2);
    }
}
