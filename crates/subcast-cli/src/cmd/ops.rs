//! Downloads, ASR models, local CLI AI agents, config, auth, storage, doctor & shell completions.
use crate::cmd::resolve_or_ingest_episode;
use crate::exit::CliError;
use crate::output::Out;
use clap::{Args, CommandFactory, Subcommand};
use clap_complete::{generate, Shell};
use serde_json::json;
use std::io::{self, Read};
use subcast_core::{agents, asr, db, llm, local_tts, offline, ops, settings, videoclip, vocab, CoreCtx, ProgressSink};

#[derive(Args)]
pub struct DownloadArgs {
    #[command(subcommand)]
    pub action: Option<DownloadAction>,
}

#[derive(Subcommand)]
pub enum DownloadAction {
    List,
    Add { reference: String },
    Remove { reference: String },
    Clear,
}

#[derive(Args)]
pub struct ModelsArgs {
    #[command(subcommand)]
    pub action: Option<ModelsAction>,
}

#[derive(Subcommand)]
pub enum ModelsAction {
    List,
    Download {
        model_id: String,
        #[arg(long)]
        no_mirror: bool,
    },
    Delete { model_id: String },
    Rescan,
}

#[derive(Args)]
pub struct AgentsArgs {
    #[command(subcommand)]
    pub action: Option<AgentsAction>,
}

#[derive(Subcommand)]
pub enum AgentsAction {
    List,
    Probe { kind: String },
    Models { kind: String },
}

#[derive(Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub action: Option<ConfigAction>,
}

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Get the full effective settings JSON or one dot-path value
    Get { path: Option<String> },
    /// Set a dot-path setting value (preserves unknown desktop fields)
    Set { path: String, value: String },
    /// Reset stored settings back to defaults
    Reset {
        #[arg(long)]
        yes: bool,
    },
    /// Show the data directory & SQLite DB path
    Path,
}

#[derive(Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub action: Option<AuthAction>,
}

#[derive(Subcommand)]
pub enum AuthAction {
    /// Show API key sources (env / keyring / unset) across providers
    Status,
    /// Store an API key in the system keychain
    SetKey {
        provider: String,
        key: Option<String>,
        #[arg(long)]
        stdin: bool,
    },
    /// Remove an API key from the system keychain
    ClearKey { provider: String },
    /// Show YouTube cookie/browser configuration
    YtStatus,
    /// Configure YouTube cookies (`--browser chrome` or `--file path`)
    YtCookies {
        #[arg(long)]
        browser: Option<String>,
        #[arg(long)]
        file: Option<String>,
    },
}

#[derive(Args)]
pub struct StorageArgs {
    #[command(subcommand)]
    pub action: Option<StorageAction>,
}

#[derive(Subcommand)]
pub enum StorageAction {
    Info,
    ClearTemp,
    ClearAudioCache,
}

#[derive(Args)]
pub struct DoctorArgs {}

#[derive(Args)]
pub struct CompletionsArgs {
    pub shell: Shell,
}

pub async fn run_download(
    ctx: &CoreCtx,
    args: DownloadArgs,
    sink: &dyn ProgressSink,
    out: Out,
) -> Result<(), CliError> {
    match args.action.unwrap_or(DownloadAction::List) {
        DownloadAction::List => {
            let list = offline::list(&ctx.data);
            out.ok(&list, || {
                for s in &list {
                    println!("{:<20}  {:>6} MB  {}", s.episode_id, s.bytes / 1_048_576, s.path);
                }
            });
        }
        DownloadAction::Add { reference } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let saved = ops::download_episode(ctx, &ep.id, sink).await?;
            out.ok(&saved, || println!("Downloaded {} -> {}", ep.title, saved.path));
        }
        DownloadAction::Remove { reference } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            offline::remove(&ctx.data, &ep.id)?;
            out.ok(&json!({ "removed": ep.id }), || println!("Removed offline file for {}", ep.id));
        }
        DownloadAction::Clear => {
            offline::clear(&ctx.data)?;
            out.ok(&json!({ "cleared": true }), || println!("Cleared all downloads"));
        }
    }
    Ok(())
}

pub async fn run_models(
    ctx: &CoreCtx,
    args: ModelsArgs,
    sink: &dyn ProgressSink,
    out: Out,
) -> Result<(), CliError> {
    match args.action.unwrap_or(ModelsAction::List) {
        ModelsAction::List => {
            let models = asr::list_models(&ctx.data);
            let elsewhere = asr::elsewhere();
            out.ok(&json!({ "models": models, "elsewhere": elsewhere }), || {
                for m in &models {
                    let st = if m.installed {
                        m.from.as_deref().unwrap_or("installed")
                    } else {
                        "not installed"
                    };
                    println!("{:<18}  {:>4} MB  [{:<13}]  {}", m.id, m.size_mb, st, m.name);
                }
            });
        }
        ModelsAction::Download { model_id, no_mirror } => {
            ops::download_model(ctx, &model_id, !no_mirror, sink).await?;
            out.ok(&json!({ "downloaded": model_id }), || println!("Downloaded model {model_id}"));
        }
        ModelsAction::Delete { model_id } => {
            ops::delete_model(ctx, &model_id)?;
            out.ok(&json!({ "deleted": model_id }), || println!("Deleted model {model_id}"));
        }
        ModelsAction::Rescan => {
            asr::rescan();
            let models = asr::list_models(&ctx.data);
            out.ok(&models, || println!("Rescanned local ASR models ({} total)", models.len()));
        }
    }
    Ok(())
}

pub async fn run_agents(args: AgentsArgs, out: Out) -> Result<(), CliError> {
    match args.action.unwrap_or(AgentsAction::List) {
        AgentsAction::List => {
            let found = agents::detect().await;
            out.ok(&found, || {
                if found.is_empty() {
                    println!("No local CLI agents found.");
                    return;
                }
                for a in &found {
                    println!("{:<12}  {}  ({} / {})", a.kind.id(), a.path, a.version, a.login);
                }
            });
        }
        AgentsAction::Probe { kind } => {
            let k = agents::Kind::parse(&kind)
                .ok_or_else(|| CliError::usage(format!("unknown agent kind: {kind}")))?;
            let path = agents::path_of(k)
                .ok_or_else(|| CliError::missing(format!("agent {kind} not installed")))?;
            let status = agents::probe(k, &path).await?;
            out.ok(&json!({ "kind": kind, "status": status }), || println!("{kind}: {status}"));
        }
        AgentsAction::Models { kind } => {
            let k = agents::Kind::parse(&kind)
                .ok_or_else(|| CliError::usage(format!("unknown agent kind: {kind}")))?;
            let path = agents::path_of(k)
                .ok_or_else(|| CliError::missing(format!("agent {kind} not installed")))?;
            let list = agents::models(k, &path).await?;
            out.ok(&list, || {
                for m in &list {
                    println!("{:<28}  {}", m.id, m.name);
                }
            });
        }
    }
    Ok(())
}

pub fn run_config(ctx: &CoreCtx, args: ConfigArgs, out: Out, no_input: bool) -> Result<(), CliError> {
    match args.action.unwrap_or(ConfigAction::Get { path: None }) {
        ConfigAction::Get { path } => {
            let eff = settings::load_effective(ctx)?;
            if let Some(p) = path {
                let v = settings::get_path(&eff, &p)
                    .ok_or_else(|| CliError::not_found(format!("未知配置键：{p}")))?;
                out.ok(v, || println!("{}", serde_json::to_string_pretty(v).unwrap_or_default()));
            } else {
                out.ok(&eff, || println!("{}", serde_json::to_string_pretty(&eff).unwrap_or_default()));
            }
        }
        ConfigAction::Set { path, value } => {
            let mut stored = ops::get_settings(ctx)?;
            if !stored.is_object() {
                stored = json!({});
            }
            let parsed = settings::parse_cli_value(&value);
            settings::set_path(&mut stored, &path, parsed.clone())?;
            ops::set_settings(ctx, &stored)?;
            out.ok(&json!({ "path": path, "value": parsed }), || println!("Updated {path} = {parsed}"));
        }
        ConfigAction::Reset { yes } => {
            if !yes && no_input {
                return Err(CliError::usage("Pass --yes to reset settings when --no-input is set"));
            }
            let d = settings::defaults_json();
            ops::set_settings(ctx, &d)?;
            out.ok(&json!({ "reset": true }), || println!("Settings reset to defaults."));
        }
        ConfigAction::Path => {
            let db_path = ctx.data.join("subcast.db");
            out.ok(&json!({ "dataDir": ctx.data, "dbPath": db_path }), || {
                println!("Data Dir : {}", ctx.data.display());
                println!("Database : {}", db_path.display());
            });
        }
    }
    Ok(())
}

pub fn run_auth(ctx: &CoreCtx, args: AuthArgs, out: Out) -> Result<(), CliError> {
    match args.action.unwrap_or(AuthAction::Status) {
        AuthAction::Status => {
            let mut entries = Vec::new();
            for p in settings::PROVIDER_PRESETS {
                entries.push(json!({
                    "id": p.id,
                    "group": "llm",
                    "needsKey": p.needs_key,
                    "source": llm::key_source(p.id),
                }));
            }
            for a in settings::ASR_PRESETS {
                let acct = format!("asr.{}", a.id);
                entries.push(json!({
                    "id": acct,
                    "group": "asr",
                    "needsKey": a.needs_key,
                    "source": llm::key_source(&acct),
                }));
            }
            for t in settings::TTS_PRESETS {
                let acct = format!("tts.{}", t.id);
                entries.push(json!({
                    "id": acct,
                    "group": "tts",
                    "needsKey": t.needs_key,
                    "source": llm::key_source(&acct),
                }));
            }
            out.ok(&entries, || {
                for e in &entries {
                    let src = e["source"].as_str().unwrap_or("unset");
                    println!("{:<20}  [{:<3}]  {}", e["id"].as_str().unwrap_or(""), e["group"].as_str().unwrap_or(""), src);
                }
            });
        }
        AuthAction::SetKey { provider, key, stdin } => {
            let secret = if stdin {
                let mut buf = String::new();
                io::stdin().read_to_string(&mut buf)?;
                buf.trim().to_string()
            } else if let Some(k) = key {
                k
            } else {
                return Err(CliError::usage("Provide <key> or pass --stdin"));
            };
            llm::set_key(&provider, &secret)?;
            out.ok(&json!({ "provider": provider, "saved": true }), || println!("Saved key for {provider}"));
        }
        AuthAction::ClearKey { provider } => {
            llm::set_key(&provider, "")?;
            out.ok(&json!({ "provider": provider, "cleared": true }), || println!("Cleared key for {provider}"));
        }
        AuthAction::YtStatus => {
            let eff = settings::load_effective(ctx)?;
            let browser = eff["ytCookiesBrowser"].as_str().unwrap_or("");
            let file = eff["ytCookiesFile"].as_str().unwrap_or("");
            out.ok(&json!({ "browser": browser, "file": file }), || {
                println!("YouTube cookies browser : {}", if browser.is_empty() { "-" } else { browser });
                println!("YouTube cookies file    : {}", if file.is_empty() { "-" } else { file });
            });
        }
        AuthAction::YtCookies { browser, file } => {
            let mut stored = ops::get_settings(ctx)?;
            if !stored.is_object() {
                stored = json!({});
            }
            if let Some(b) = browser {
                settings::set_path(&mut stored, "ytCookiesBrowser", serde_json::Value::String(b))?;
            }
            if let Some(f) = file {
                settings::set_path(&mut stored, "ytCookiesFile", serde_json::Value::String(f))?;
            }
            ops::set_settings(ctx, &stored)?;
            out.ok(&json!({ "updated": true }), || println!("Updated YouTube cookie settings."));
        }
    }
    Ok(())
}

pub fn run_storage(ctx: &CoreCtx, args: StorageArgs, out: Out) -> Result<(), CliError> {
    match args.action.unwrap_or(StorageAction::Info) {
        StorageAction::Info => {
            let info = ops::storage_info(ctx);
            out.ok(&info, || println!("{}", serde_json::to_string_pretty(&info).unwrap_or_default()));
        }
        StorageAction::ClearTemp => {
            ops::clear_temp(ctx)?;
            out.ok(&json!({ "cleared": "temp" }), || println!("Cleared temp directory."));
        }
        StorageAction::ClearAudioCache => {
            offline::clear_cache(&ctx.data)?;
            out.ok(&json!({ "cleared": "audio-cache" }), || println!("Cleared audio cache."));
        }
    }
    Ok(())
}

pub fn run_doctor(ctx: &CoreCtx, _args: DoctorArgs, out: Out) -> Result<(), CliError> {
    let db_path = ctx.data.join("subcast.db");
    let (shows_count, ep_count) = {
        let conn = ctx.db.lock();
        let sc: i64 = db::list_shows(&conn).map(|v| v.len() as i64).unwrap_or(0);
        let ec: i64 = conn.query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0)).unwrap_or(0);
        (sc, ec)
    };
    let ff = videoclip::env_status(&ctx.data);
    let ytdlp_path = asr::find_ytdlp(&ctx.data).map(|p| p.to_string_lossy().into_owned());
    let crispasr_path = asr::find_crispasr(&ctx.data).map(|p| p.to_string_lossy().into_owned());
    let installed_asr: Vec<String> = asr::list_models(&ctx.data)
        .into_iter()
        .filter(|m| m.installed)
        .map(|m| m.id)
        .collect();
    let ltts = local_tts::status(&ctx.data);
    let dict_path = vocab::Dict::find_path(&ctx.data, None).map(|p| p.to_string_lossy().into_owned());
    let chat_ready = settings::resolve_chat_provider(ctx, None, None)
        .ok()
        .is_some_and(|r| settings::provider_ready(&r));
    let tr_ready = settings::resolve_translate_provider(ctx, None, None)
        .is_some_and(|r| settings::provider_ready(&r));
    let installed_agents: Vec<&'static str> = agents::Kind::ALL
        .into_iter()
        .filter(|&k| agents::path_of(k).is_some())
        .map(|k| k.id())
        .collect();

    let report = json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "dataDir": ctx.data,
        "dbPath": db_path,
        "shows": shows_count,
        "episodes": ep_count,
        "ffmpeg": ff,
        "ytdlp": ytdlp_path,
        "crispasr": crispasr_path,
        "asrModels": installed_asr,
        "localTts": ltts,
        "dict": dict_path,
        "chatProviderReady": chat_ready,
        "translateProviderReady": tr_ready,
        "agents": installed_agents,
    });
    out.ok(&report, || {
        println!("SubCast CLI v{}", env!("CARGO_PKG_VERSION"));
        println!("Data Dir  : {}", ctx.data.display());
        println!("Database  : {} ({shows_count} shows, {ep_count} episodes)", db_path.display());
        println!("FFmpeg    : {}", ff.path.as_deref().unwrap_or("not installed (auto-downloaded on clip)"));
        println!("yt-dlp    : {}", ytdlp_path.as_deref().unwrap_or("not installed"));
        println!("CrispASR  : {}", crispasr_path.as_deref().unwrap_or("not installed"));
        println!("ASR Models: {}", if installed_asr.is_empty() { "none".into() } else { installed_asr.join(", ") });
        println!("Dict      : {}", dict_path.as_deref().unwrap_or("not found"));
        println!("LLM Ready : chat={} translate={}", chat_ready, tr_ready);
        println!("Agents    : {}", if installed_agents.is_empty() { "none".into() } else { installed_agents.join(", ") });
    });
    Ok(())
}

pub fn run_completions(args: CompletionsArgs) -> Result<(), CliError> {
    let mut cmd = crate::Cli::command();
    generate(args.shell, &mut cmd, "subcast", &mut io::stdout());
    Ok(())
}
