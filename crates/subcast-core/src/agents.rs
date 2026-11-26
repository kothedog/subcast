//! Local AI agents the listener already has (their own subscription, no API key), kept running between requests:
//!
//! - Claude Code: `claude -p --input-format stream-json --output-format stream-json` (its own long-lived mode).
//! - Codex: `codex app-server` (JSON-RPC: thread/start with base instructions, read-only sandbox, turn/start).
//! - Gemini CLI, Qwen Code, Kimi Code, OpenCode: the Agent Client Protocol (ACP) over stdio.
//!
//! Every agent runs in an empty temporary folder with its tools off: no file access, no commands, every permission
//! request refused. It answers text only. Executables are started directly (never through a shell: a user alias
//! such as `claude --dangerously-skip-permissions` must not apply), with a PATH that includes the usual install
//! folders, since apps opened from the Dock don't get the terminal's PATH.
use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{mpsc, oneshot, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind { Claude, Codex, Antigravity, Gemini, Qwen, Kimi, Opencode }

impl Kind {
    pub const ALL: [Kind; 7] = [Kind::Claude, Kind::Codex, Kind::Antigravity, Kind::Gemini, Kind::Qwen, Kind::Kimi, Kind::Opencode];
    pub fn parse(s: &str) -> Option<Kind> { Kind::ALL.into_iter().find(|k| k.id() == s) }
    pub fn id(self) -> &'static str {
        match self { Kind::Claude => "claude", Kind::Codex => "codex", Kind::Antigravity => "antigravity", Kind::Gemini => "gemini", Kind::Qwen => "qwen", Kind::Kimi => "kimi", Kind::Opencode => "opencode" }
    }
    fn exe(self) -> &'static str { if self == Kind::Antigravity { "agy" } else { self.id() } }
    /// Most parallel requests worth running: each is a process (or a session in one) on the user's subscription.
    pub fn max_parallel(self) -> usize { match self { Kind::Codex => 2, _ => 2 } }
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Found {
    pub kind: Kind,
    pub path: String,
    pub version: String,
    /// ready | needsLogin | unknown
    pub login: &'static str,
}

fn home() -> PathBuf { dirs::home_dir().unwrap_or_default() }

/// Folders to look in, beyond PATH: where the official installers, npm, Homebrew, bun, nvm and volta put them.
fn search_dirs() -> Vec<PathBuf> {
    let h = home();
    let mut v: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    for rel in [".local/bin", ".claude/local", ".bun/bin", ".npm-global/bin", ".cargo/bin", ".opencode/bin", ".kimi-code/bin", ".volta/bin", "bin"] { v.push(h.join(rel)); }
    for d in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] { v.push(PathBuf::from(d)); }
    for base in [h.join(".nvm/versions/node"), h.join(".volta/tools/image/node")] {
        if let Ok(rd) = std::fs::read_dir(&base) {
            let mut vers: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).collect();
            vers.sort();
            v.extend(vers.into_iter().rev());
        }
    }
    if cfg!(windows) {
        if let Some(a) = std::env::var_os("APPDATA") { v.push(PathBuf::from(a).join("npm")); }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") { v.push(PathBuf::from(&l).join("Programs")); v.push(PathBuf::from(l).join("Microsoft\\WinGet\\Links")); }
    }
    let mut seen = std::collections::HashSet::new();
    v.retain(|p| seen.insert(p.clone()));
    v
}

/// PATH for the agents themselves (they start node, git and the like).
pub fn child_path() -> std::ffi::OsString { std::env::join_paths(search_dirs()).unwrap_or_default() }

fn find(kind: Kind) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) { ["exe", "cmd", "bat"].iter().map(|e| format!("{}.{e}", kind.exe())).collect() } else { vec![kind.exe().to_string()] };
    for d in search_dirs() {
        for n in &names {
            let p = d.join(n);
            if p.is_file() { return Some(p); }
        }
    }
    None
}

/// Where an agent is installed, if it is.
pub fn path_of(kind: Kind) -> Option<PathBuf> { find(kind) }

/// A Command for an agent executable. Windows .cmd shims need cmd.exe; no console windows either way.
fn command(path: &Path) -> Command {
    let is_script = cfg!(windows) && path.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let mut c = if is_script { let mut c = Command::new("cmd"); c.arg("/C").arg(path); c } else { Command::new(path) };
    c.env("PATH", child_path()).kill_on_drop(true);
    // agy updates itself when it starts: the first start after a release downloads ~230 MB before answering, which
    // made SubCast's first question hang. The terminal agy keeps updating; the copy SubCast starts doesn't.
    if path.file_stem().is_some_and(|n| n.eq_ignore_ascii_case("agy")) { c.env("AGY_CLI_DISABLE_AUTO_UPDATE", "1"); }
    #[cfg(windows)]
    { c.creation_flags(0x08000000); }
    // CLIs only honour HTTPS_PROXY; apps opened from the Dock don't inherit it from the shell.
    let proxy = crate::net::effective_proxy();
    if !proxy.is_empty() { c.env("HTTPS_PROXY", &proxy).env("HTTP_PROXY", &proxy); }
    c
}

async fn output(path: &Path, args: &[&str], secs: u64) -> Option<String> {
    let mut c = command(path);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).current_dir(work_dir().ok()?);
    let out = tokio::time::timeout(Duration::from_secs(secs), c.output()).await.ok()?.ok()?;
    Some(format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

/// Runs an agent's own command line (`claude mcp add …`) with the PATH and proxy it needs: success and its output.
pub async fn run_cli(path: &Path, args: &[&str], secs: u64) -> Result<(bool, String)> {
    let mut c = command(path);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(h) = dirs::home_dir() { c.current_dir(h); }
    let out = tokio::time::timeout(Duration::from_secs(secs), c.output()).await.map_err(|_| anyhow!("{} 没有响应", path.display()))??;
    Ok((out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))))
}

/// An empty folder agents run in, so there is nothing for them to read or change.
fn work_dir() -> Result<PathBuf> {
    let d = std::env::temp_dir().join("subcast-agent");
    std::fs::create_dir_all(&d)?;
    Ok(d)
}

/// Whether the agent has a login, from what it keeps on disk or reports; "unknown" when it can't be told without
/// starting a session (probe() settles it).
async fn login(kind: Kind, path: &Path) -> &'static str {
    let h = home();
    match kind {
        Kind::Claude => match output(path, &["auth", "status", "--json"], 10).await {
            Some(s) => match serde_json::from_str::<Value>(s.trim()).ok().or_else(|| s.find('{').and_then(|i| serde_json::from_str(&s[i..]).ok())) {
                Some(v) if v["loggedIn"].as_bool() == Some(true) => "ready",
                Some(_) => "needsLogin",
                None => "unknown",
            },
            None => "unknown",
        },
        Kind::Codex => {
            let dir = std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| h.join(".codex"));
            if dir.join("auth.json").is_file() { "ready" } else { "needsLogin" }
        }
        // agy keeps its Google login in the keychain (service "gemini", account "antigravity"). Checked without
        // reading it (no -w: no keychain prompt). Not starting agy unless logged in matters: a logged-out agy
        // opens a browser on every start.
        Kind::Antigravity => {
            if cfg!(target_os = "macos") {
                let ok = Command::new("/usr/bin/security").args(["find-generic-password", "-s", "gemini", "-a", "antigravity"]).stdout(Stdio::null()).stderr(Stdio::null()).status().await.is_ok_and(|s| s.success());
                if ok { "ready" } else { "needsLogin" }
            } else { "unknown" }
        }
        // Gemini CLI: Google no longer serves personal accounts here (they go to Antigravity); an API key works.
        Kind::Gemini => if std::env::var_os("GEMINI_API_KEY").is_some() { "ready" } else if h.join(".gemini/oauth_creds.json").is_file() || std::env::var_os("GEMINI_API_KEY").is_some() { "ready" } else { "unknown" },
        Kind::Kimi => if h.join(".kimi/credentials").exists() { "ready" } else { "unknown" },
        Kind::Qwen | Kind::Opencode => "unknown",
    }
}

/// Every agent found on this computer, with its login state. Fast (a few seconds at most, run in parallel).
pub async fn detect() -> Vec<Found> {
    let jobs = Kind::ALL.into_iter().filter_map(|k| find(k).map(|p| (k, p))).map(|(kind, path)| async move {
        let version = output(&path, &["--version"], 8).await.unwrap_or_default().lines().next().unwrap_or("").trim().chars().take(60).collect();
        let login = login(kind, &path).await;
        Found { kind, path: path.to_string_lossy().into(), version, login }
    });
    futures_util::future::join_all(jobs).await
}

/// Starts an agent's session once to see whether it answers (no model call): ready, needsLogin or an error.
pub async fn probe(kind: Kind, path: &Path) -> Result<&'static str> {
    match kind {
        Kind::Claude | Kind::Codex | Kind::Antigravity => Ok(login(kind, path).await),
        _ => {
            let conn = Acp::start(kind, path).await?;
            let r = conn.new_session().await;
            conn.close().await;
            match r {
                Ok(_) => Ok("ready"),
                Err(e) if e.to_string().contains("auth") || e.to_string().contains("登录") => Ok("needsLogin"),
                Err(e) => Err(e),
            }
        }
    }
}

/// A model the agent offers (its id, as passed back when asking, and a display name).
#[derive(serde::Serialize, Debug, PartialEq)]
pub struct AgentModel { pub id: String, pub name: String }

/// The models the agent itself says it can use. Claude Code has no listing (its aliases are fixed in the app).
pub async fn models(kind: Kind, path: &Path) -> Result<Vec<AgentModel>> {
    match kind {
        Kind::Claude => Ok(Vec::new()),
        Kind::Codex => {
            let c = Codex::start(path).await?;
            let r = c.rpc.request("model/list", json!({}), 30).await;
            c.rpc.close().await;
            Ok(parse_codex_models(&r?))
        }
        _ => {
            let conn = Acp::start(kind, path).await.map_err(|e| friendly(kind, e))?;
            let r = conn.rpc.request("session/new", json!({ "cwd": work_dir()?.to_string_lossy(), "mcpServers": [] }), 60).await;
            conn.close().await;
            Ok(parse_acp_models(&r.map_err(|e| friendly(kind, e))?))
        }
    }
}

fn parse_codex_models(v: &Value) -> Vec<AgentModel> {
    let list = v["data"].as_array().or(v["models"].as_array()).cloned().unwrap_or_default();
    list.iter().filter(|m| m["hidden"].as_bool() != Some(true)).filter_map(|m| {
        let id = m["model"].as_str().or(m["id"].as_str())?.to_string();
        let name = m["displayName"].as_str().or(m["name"].as_str()).unwrap_or(&id).to_string();
        Some(AgentModel { id, name })
    }).collect()
}

/// session/new answers with models.availableModels (older agents) or a configOptions entry for "model".
fn parse_acp_models(v: &Value) -> Vec<AgentModel> {
    let mut out: Vec<AgentModel> = Vec::new();
    for m in v["models"]["availableModels"].as_array().into_iter().flatten() {
        if let Some(id) = m["modelId"].as_str() { out.push(AgentModel { id: id.into(), name: m["name"].as_str().unwrap_or(id).into() }); }
    }
    if out.is_empty() {
        for o in v["configOptions"].as_array().into_iter().flatten().filter(|o| o["id"].as_str() == Some("model") || o["category"].as_str() == Some("model")) {
            let opts = o["options"].as_array().cloned().unwrap_or_default();
            // Options may be grouped: [{ group, options: [...] }].
            for x in opts.iter().flat_map(|x| x["options"].as_array().cloned().unwrap_or_else(|| vec![x.clone()])) {
                if let Some(id) = x["value"].as_str() { out.push(AgentModel { id: id.into(), name: x["name"].as_str().unwrap_or(id).into() }); }
            }
        }
    }
    out
}


// ---- requests --------------------------------------------------------------------------------------------

pub struct Request<'a> {
    pub system: &'a str,
    pub user: &'a str,
    pub model: Option<&'a str>,
    /// The answer must be JSON (translation). The prompt asks for it; agents have no JSON mode, so it is only
    /// used to remind them once more.
    pub json: bool,
}

/// One answer from a local agent, streamed through `on_delta`. Uses a warm process when one is free.
pub async fn complete(kind: Kind, path: &Path, mut req: Request<'_>, cancel: Arc<AtomicBool>, on_delta: impl Fn(&str) + Send) -> Result<String> {
    let reminded;
    if req.json { reminded = format!("{}\n\n(Answer with the JSON only, no Markdown fences, no other text.)", req.user); req.user = &reminded; }
    let conn = POOL.take(kind, path, req.system, req.model, &cancel).await.map_err(|e| friendly(kind, e))?;
    let result = conn.ask(&req, cancel, &on_delta).await;
    POOL.give_back(conn, result.is_ok()).await;
    match result {
        // An agent can finish a turn with nothing to show (only thinking, a refusal, a cut-off): say so.
        Ok(text) if text.trim().is_empty() => Err(anyhow!("{} 没有给出回答（可能只输出了思考过程，或被中途打断），请重试或换一个模型", display(kind))),
        Ok(text) => Ok(text),
        Err(e) => Err(friendly(kind, e)),
    }
}

fn friendly(kind: Kind, e: anyhow::Error) -> anyhow::Error {
    let m = e.to_string();
    if kind == Kind::Antigravity { if let Some(f) = agy_friendly(&m) { return anyhow!(f); } }
    let name = display(kind);
    let l = m.to_lowercase();
    if l.contains("no longer supported") || l.contains("migrate to") {
        anyhow!("Google 已不再让个人账号使用 {name}。个人 Google 账号请改用 Antigravity CLI（agy）；{name} 只适用于 API Key 或企业版账号")
    } else if l.contains("model is unavailable") || l.contains("model not found") || l.contains("unknown model") {
        anyhow!("{name} 的默认模型不可用，请在 SubCast 里为它选一个模型")
    } else if l.contains("not logged in") || l.contains("login") || l.contains("unauthor") || l.contains("auth_required") || l.contains("authentication") {
        anyhow!("{name} 需要登录：在终端运行 {} 完成登录后，回到 SubCast 点「重新检测」", kind.exe())
    } else if l.contains("rate limit") || l.contains("usage limit") || l.contains("quota") || l.contains("429") || l.contains("limit reached") {
        anyhow!("{name} 的订阅额度暂时用完了（请求太频繁或额度不足）：{}", m.chars().take(160).collect::<String>())
    } else { e }
}

/// What agy's own error words mean for the listener, in their words (agy's are Google's status lines). Matched as
/// agy 1.5.x writes them; the wordings are the ones magpie's agy-cli bridge (magpie-community/plugins) has seen.
fn agy_friendly(m: &str) -> Option<String> {
    let l = m.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| l.contains(w));
    Some(if has(&[AGY_TOO_OLD, "flag provided but not defined", "unknown command", "unknown flag", "unknown argument"]) {
        "Antigravity CLI 版本太旧，不支持 SubCast 的调用方式：在终端运行 agy update 升级后，回到 SubCast 点「重新检测」".to_string()
    } else if has(&["individual quota reached", "third-party quota reached"]) {
        // "⚠ Individual quota reached. Please upgrade your subscription to increase your limits. Resets in 1h18m40s."
        let third = l.contains("third-party");
        match agy_reset_in(m) {
            Some(t) if third => format!("Antigravity CLI 的 Claude / GPT 额度用完了，{t} 后恢复；Gemini 模型的额度是分开算的，可以先在 SubCast 里换成 Gemini 模型"),
            Some(t) => format!("Antigravity CLI 所用 Google 账号的额度用完了，{t} 后恢复；也可以先在 SubCast 里换一个模型"),
            None if third => String::from("Antigravity CLI 的 Claude / GPT 额度用完了；Gemini 模型的额度是分开算的，可以先在 SubCast 里换成 Gemini 模型"),
            None => String::from("Antigravity CLI 所用 Google 账号的额度用完了，稍后再试，或先在 SubCast 里换一个模型"),
        }
    } else if has(&["credits balance is too low"]) {
        "Antigravity CLI 的 AI 点数余额不足，等额度恢复或在 Google 账号里补充后再试".to_string()
    } else if has(&["violation of terms of service", "service has been disabled"]) {
        "Google 停用了这个账号的 Antigravity 服务（判定违反服务条款），需要先向 Google 申诉".to_string()
    } else if has(&["verification required", "validation_required"]) {
        "Google 要求先验证这个账号：在终端运行 agy，按提示完成验证后再试".to_string()
    } else if has(&["high traffic", "no capacity available", "code 503"]) {
        "Google 的服务器正忙，所选模型暂时没有空余算力：过一分钟再试，或在 SubCast 里换一个模型".to_string()
    } else if has(&["network issue connecting"]) {
        "Antigravity CLI 连不上 Google 的服务器：检查网络或代理后重试".to_string()
    } else if has(&["resource_exhausted", "resource has been exhausted", "resource exhausted", "code 429"]) {
        // agy answers some prompts with a bare 429 that has nothing to do with quota.
        "Antigravity CLI 暂时拒绝了这次请求（Google 返回 429，不一定是额度用完）：稍后再试，或在 SubCast 里换一个模型".to_string()
    } else if has(&["agent execution terminated"]) {
        "Antigravity CLI 回答到一半出错停止了，请重试".to_string()
    } else { return None })
}

/// "… Resets in 1h18m40s." → "1h18m40s".
fn agy_reset_in(m: &str) -> Option<String> {
    let i = m.to_lowercase().find("resets in ")?;
    let t: String = m[i + "resets in ".len()..].chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (!t.is_empty()).then_some(t)
}

/// agy ends a turn whose model call failed normally, with the failure as the reply's last line
/// ("⚠ RESOURCE_EXHAUSTED (code 429): Resource has been exhausted (e.g. check quota)."). That line, if it is one.
fn agy_error_line(text: &str) -> Option<&str> {
    const STARTS: [&str; 10] = ["RESOURCE_EXHAUSTED", "PERMISSION_DENIED", "Individual quota reached", "Third-party quota reached",
        "Verification Required", "Agent execution terminated", "There was a network issue", "Your AI credits balance",
        "Our servers are experiencing high traffic", "This service has been disabled"];
    let last = text.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    let body = last.strip_prefix('⚠')?.trim_start();
    STARTS.iter().any(|s| body.starts_with(s)).then_some(last)
}

pub fn display(kind: Kind) -> &'static str {
    match kind { Kind::Claude => "Claude Code", Kind::Codex => "Codex", Kind::Antigravity => "Antigravity CLI", Kind::Gemini => "Gemini CLI", Kind::Qwen => "Qwen Code", Kind::Kimi => "Kimi Code", Kind::Opencode => "OpenCode" }
}

// ---- the pool of warm processes ----------------------------------------------------------------------------

const IDLE: Duration = Duration::from_secs(240);
/// How long a request waits for a free process before saying the agent is busy.
const BUSY_WAIT: Duration = Duration::from_secs(90);
/// Claude Code keeps the conversation in one process; after this many requests it is restarted so old turns
/// don't pile up in every new request.
const CLAUDE_TURNS: u32 = 6;

enum Conn { Claude(ClaudeConn), Codex(Codex), Acp(Acp) }

struct Pooled { kind: Kind, key: String, conn: Conn, idle_since: Instant, turns: u32 }

struct Pool { idle: Mutex<Vec<Pooled>>, busy: Mutex<HashMap<Kind, usize>> }

static POOL: once_cell::sync::Lazy<Pool> = once_cell::sync::Lazy::new(|| {
    tokio::spawn(async {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let stale: Vec<Pooled> = { let mut v = POOL.idle.lock().await; let (old, keep) = v.drain(..).partition(|p| p.idle_since.elapsed() > IDLE); *v = keep; old };
            for p in stale { p.conn.close().await; }
        }
    });
    Pool { idle: Mutex::new(Vec::new()), busy: Mutex::new(HashMap::new()) }
});

impl Pool {
    async fn take(&self, kind: Kind, path: &Path, system: &str, model: Option<&str>, cancel: &AtomicBool) -> Result<Pooled> {
        // Claude Code's system prompt is fixed per process; the others take it per request.
        let key = if kind == Kind::Claude { format!("{}|{}", model.unwrap_or(""), crate::model::short_hash(system, 16)) } else { String::new() };
        let waiting = Instant::now();
        loop {
            // Every process of this kind is busy (a long translation, say): wait a while, never forever.
            if cancel.load(Ordering::Relaxed) { return Err(anyhow!("已取消")); }
            if waiting.elapsed() > BUSY_WAIT { return Err(anyhow!("{} 正忙（可能在翻译整集），请稍后再试", display(kind))); }
            {
                let mut idle = self.idle.lock().await;
                if let Some(i) = idle.iter().position(|p| p.kind == kind && p.key == key) {
                    let p = idle.remove(i);
                    *self.busy.lock().await.entry(kind).or_default() += 1;
                    return Ok(p);
                }
                let busy = *self.busy.lock().await.get(&kind).unwrap_or(&0);
                if busy < kind.max_parallel() {
                    // A warm process of the same kind with another system prompt makes room for this one.
                    if let Some(i) = idle.iter().position(|p| p.kind == kind) { let old = idle.remove(i); drop(idle); old.conn.close().await; }
                    *self.busy.lock().await.entry(kind).or_default() += 1;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        let conn = match kind {
            Kind::Claude => ClaudeConn::start(path, system, model).await.map(Conn::Claude),
            Kind::Codex => Codex::start(path).await.map(Conn::Codex),
            _ => Acp::start(kind, path).await.map(Conn::Acp),
        };
        match conn {
            Ok(conn) => Ok(Pooled { kind, key, conn, idle_since: Instant::now(), turns: 0 }),
            Err(e) => { self.release(kind).await; Err(e) }
        }
    }

    async fn release(&self, kind: Kind) { if let Some(n) = self.busy.lock().await.get_mut(&kind) { *n = n.saturating_sub(1); } }

    async fn give_back(&self, mut p: Pooled, ok: bool) {
        self.release(p.kind).await;
        p.turns += 1;
        if !ok || !p.conn.alive() || (p.kind == Kind::Claude && p.turns >= CLAUDE_TURNS) { p.conn.close().await; return; }
        p.idle_since = Instant::now();
        let mut idle = self.idle.lock().await;
        idle.push(p);
        // At most two warm processes per kind.
        while idle.iter().filter(|x| x.kind == idle.last().unwrap().kind).count() > 2 { let i = idle.iter().position(|x| x.kind == idle.last().unwrap().kind).unwrap(); let old = idle.remove(i); old.conn.close_now(); }
    }
}

impl Pooled {
    async fn ask(&self, req: &Request<'_>, cancel: Arc<AtomicBool>, on_delta: &(impl Fn(&str) + Send)) -> Result<String> {
        match &self.conn {
            Conn::Claude(c) => c.ask(req, cancel, on_delta).await,
            Conn::Codex(c) => c.ask(req, cancel, on_delta).await,
            Conn::Acp(c) => c.ask(req, cancel, on_delta).await,
        }
    }
}

impl Conn {
    fn alive(&self) -> bool { match self { Conn::Claude(c) => c.rpc.alive(), Conn::Codex(c) => c.rpc.alive(), Conn::Acp(c) => c.rpc.alive() } }
    async fn close(self) { match self { Conn::Claude(c) => c.rpc.close().await, Conn::Codex(c) => c.rpc.close().await, Conn::Acp(c) => c.close().await } }
    fn close_now(self) { tokio::spawn(self.close()); }
}

/// Warms an agent up (opening the Q&A panel), so the first question doesn't wait for it to start.
pub async fn warm(kind: Kind, path: &Path) {
    if kind == Kind::Claude { return; } // its process is bound to a system prompt; nothing to share yet
    if let Ok(p) = POOL.take(kind, path, "", None, &AtomicBool::new(false)).await { POOL.give_back(p, true).await; }
}

// ---- line-oriented JSON over a child's stdio -------------------------------------------------------------------

/// A child process speaking newline-delimited JSON. Responses to our requests (by id) go to their waiters; every
/// other message goes to `events`.
struct Rpc {
    child: Mutex<Option<Child>>,
    stdin: Mutex<ChildStdin>,
    next: AtomicU64,
    waiting: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    events: Mutex<mpsc::UnboundedReceiver<Value>>,
    dead: Arc<AtomicBool>,
    stderr: Arc<std::sync::Mutex<String>>,
    /// Set once the child's stderr has closed, so everything it wrote before exiting is in `stderr`.
    stderr_done: Arc<AtomicBool>,
}

impl Rpc {
    async fn spawn(path: &Path, args: &[&str]) -> Result<Rpc> {
        let mut c = command(path);
        c.args(args).current_dir(work_dir()?).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = c.spawn().with_context(|| format!("无法启动 {}", path.display()))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let waiting: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> = Arc::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let dead = Arc::new(AtomicBool::new(false));
        let err_tail = Arc::new(std::sync::Mutex::new(String::new()));
        let err_done = Arc::new(AtomicBool::new(false));
        {
            let (waiting, dead) = (waiting.clone(), dead.clone());
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let Ok(v) = serde_json::from_str::<Value>(line.trim()) else { continue };
                    // A response: has an id and a result or error, and no method.
                    if v.get("method").is_none() && (v.get("result").is_some() || v.get("error").is_some()) {
                        if let Some(id) = v["id"].as_u64() { if let Some(w) = waiting.lock().await.remove(&id) { let _ = w.send(v); continue; } }
                    }
                    let _ = tx.send(v);
                }
                dead.store(true, Ordering::Relaxed);
                waiting.lock().await.clear();
            });
        }
        {
            let (tail, done) = (err_tail.clone(), err_done.clone());
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut t = tail.lock().unwrap();
                    t.push_str(&line); t.push('\n');
                    if t.len() > 4000 { let cut = t.len() - 2000; *t = t[cut..].to_string(); }
                }
                done.store(true, Ordering::Relaxed);
            });
        }
        Ok(Rpc { child: Mutex::new(Some(child)), stdin: Mutex::new(stdin), next: AtomicU64::new(1), waiting, events: Mutex::new(rx), dead, stderr: err_tail, stderr_done: err_done })
    }

    fn alive(&self) -> bool { !self.dead.load(Ordering::Relaxed) }

    /// Everything the child wrote to stderr, once it has exited (waiting a moment for the last lines); None while
    /// it still runs.
    async fn stderr_after_exit(&self) -> Option<String> {
        if self.alive() { return None; }
        let t = Instant::now();
        while !self.stderr_done.load(Ordering::Relaxed) && t.elapsed() < Duration::from_secs(2) { tokio::time::sleep(Duration::from_millis(50)).await; }
        Some(self.stderr.lock().unwrap().clone())
    }

    fn last_error(&self) -> String { self.stderr.lock().unwrap().lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(200).collect() }

    async fn send(&self, v: &Value) -> Result<()> {
        let mut s = self.stdin.lock().await;
        s.write_all(format!("{v}\n").as_bytes()).await?;
        s.flush().await?;
        Ok(())
    }

    async fn request(&self, method: &str, params: Value, secs: u64) -> Result<Value> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.waiting.lock().await.insert(id, tx);
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })).await?;
        let v = tokio::time::timeout(Duration::from_secs(secs), rx).await
            .map_err(|_| anyhow!("{method} 超时"))?
            .map_err(|_| anyhow!("本机 AI 意外退出：{}", self.last_error()))?;
        if let Some(e) = v.get("error") { return Err(anyhow!("{}", e["message"].as_str().unwrap_or(&e.to_string()))); }
        Ok(v["result"].clone())
    }

    async fn notify(&self, method: &str, params: Value) -> Result<()> { self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params })).await }

    async fn reply(&self, id: &Value, result: Value) -> Result<()> { self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result })).await }

    async fn reply_error(&self, id: &Value, message: &str) -> Result<()> { self.send(&json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": message } })).await }

    /// The next message that isn't a response, waiting at most `secs`.
    async fn event(&self, secs: u64) -> Result<Value> {
        let mut rx = self.events.lock().await;
        match tokio::time::timeout(Duration::from_secs(secs), rx.recv()).await {
            Ok(Some(v)) => Ok(v),
            Ok(None) => Err(anyhow!("本机 AI 意外退出：{}", self.last_error())),
            Err(_) => Err(anyhow!("本机 AI 长时间没有回应")),
        }
    }

    async fn close(&self) {
        if let Some(mut c) = self.child.lock().await.take() {
            let _ = c.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(3), c.wait()).await;
        }
    }
}

/// Waits for the next event, giving up when the request is cancelled.
async fn next_event(rpc: &Rpc, cancel: &AtomicBool, started: Instant) -> Result<Value> {
    loop {
        if cancel.load(Ordering::Relaxed) { return Err(anyhow!("已取消")); }
        if started.elapsed() > Duration::from_secs(300) { return Err(anyhow!("本机 AI 超过 5 分钟没有答完")); }
        match rpc.event(1).await {
            Ok(v) => return Ok(v),
            Err(e) if e.to_string().contains("长时间没有回应") => continue,
            Err(e) => return Err(e),
        }
    }
}

// ---- Claude Code: stream-json in, stream-json out ------------------------------------------------------------

struct ClaudeConn { rpc: Rpc }

impl ClaudeConn {
    async fn start(path: &Path, system: &str, model: Option<&str>) -> Result<ClaudeConn> {
        let mut args = vec!["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages",
            "--tools", "", "--strict-mcp-config", "--no-session-persistence", "--system-prompt", system];
        if let Some(m) = model.filter(|m| !m.is_empty()) { args.extend(["--model", m]); }
        Ok(ClaudeConn { rpc: Rpc::spawn(path, &args).await? })
    }

    async fn ask(&self, req: &Request<'_>, cancel: Arc<AtomicBool>, on_delta: &(impl Fn(&str) + Send)) -> Result<String> {
        self.rpc.send(&json!({ "type": "user", "message": { "role": "user", "content": req.user } })).await?;
        let started = Instant::now();
        let mut text = String::new();
        loop {
            let v = match next_event(&self.rpc, &cancel, started).await { Ok(v) => v, Err(e) => { if cancel.load(Ordering::Relaxed) { self.rpc.close().await; } return Err(e); } };
            match v["type"].as_str() {
                Some("stream_event") => {
                    let ev = &v["event"];
                    if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
                        if let Some(d) = ev["delta"]["text"].as_str() { text.push_str(d); on_delta(d); }
                    }
                }
                Some("result") => {
                    if v["is_error"].as_bool() == Some(true) { return Err(anyhow!("{}", v["result"].as_str().unwrap_or("Claude Code 出错"))); }
                    if text.is_empty() { text = v["result"].as_str().unwrap_or("").to_string(); on_delta(&text); }
                    return Ok(text);
                }
                _ => {}
            }
        }
    }
}

// ---- Codex app-server ---------------------------------------------------------------------------------------------

struct Codex { rpc: Rpc }

impl Codex {
    async fn start(path: &Path) -> Result<Codex> {
        let rpc = Rpc::spawn(path, &["app-server"]).await?;
        rpc.request("initialize", json!({ "clientInfo": { "name": "subcast", "title": "SubCast", "version": env!("CARGO_PKG_VERSION") } }), 30).await?;
        rpc.notify("initialized", json!({})).await?;
        Ok(Codex { rpc })
    }

    async fn ask(&self, req: &Request<'_>, cancel: Arc<AtomicBool>, on_delta: &(impl Fn(&str) + Send)) -> Result<String> {
        let cwd = work_dir()?.to_string_lossy().to_string();
        let mut thread = json!({ "cwd": cwd, "sandbox": "read-only", "approvalPolicy": "never", "ephemeral": true, "baseInstructions": req.system });
        if let Some(m) = req.model.filter(|m| !m.is_empty()) { thread["model"] = json!(m); }
        let t = self.rpc.request("thread/start", thread, 60).await?;
        let thread_id = t["thread"]["id"].as_str().or(t["threadId"].as_str()).ok_or_else(|| anyhow!("Codex 没有返回会话"))?.to_string();
        // Low reasoning effort: translating and answering about a transcript doesn't need long deliberation, and
        // the default made a one-line answer take half a minute. JSON is asked for in the prompt (an output schema
        // would need every translation field spelled out).
        let turn = json!({ "threadId": thread_id, "input": [{ "type": "text", "text": req.user }], "effort": "low" });
        self.rpc.request("turn/start", turn, 60).await?;
        let started = Instant::now();
        let mut text = String::new();
        loop {
            let v = match next_event(&self.rpc, &cancel, started).await {
                Ok(v) => v,
                Err(e) => { if cancel.load(Ordering::Relaxed) { let _ = self.rpc.request("turn/interrupt", json!({ "threadId": thread_id }), 5).await; } return Err(e); }
            };
            let m = v["method"].as_str().unwrap_or("");
            // The server asking us something (approvals): refuse everything.
            if v.get("id").is_some() && !m.is_empty() { let _ = self.rpc.reply_error(&v["id"], "SubCast 不允许本机 AI 执行操作").await; continue; }
            if v["params"]["threadId"].as_str().is_some_and(|t| t != thread_id) { continue; }
            match m {
                "item/agentMessage/delta" => if let Some(d) = v["params"]["delta"].as_str() { text.push_str(d); on_delta(d); },
                // The finished message, in case its pieces never came as deltas.
                "item/completed" => if text.is_empty() && v["params"]["item"]["type"].as_str() == Some("agentMessage") {
                    if let Some(t) = v["params"]["item"]["text"].as_str() { text.push_str(t); on_delta(t); }
                },
                "turn/completed" => {
                    let turn = &v["params"]["turn"];
                    if turn["status"].as_str() == Some("failed") {
                        return Err(anyhow!("{}", turn["error"]["message"].as_str().unwrap_or("Codex 没有完成回答")));
                    }
                    return Ok(text);
                }
                "error" => return Err(anyhow!("{}", v["params"]["error"]["message"].as_str().or(v["params"]["message"].as_str()).unwrap_or("Codex 出错"))),
                _ => {}
            }
        }
    }
}

// ---- Agent Client Protocol (Gemini CLI, Qwen Code, Kimi Code, OpenCode) -------------------------------------

/// `spare`: a session already asked for while the process was idle, so the next question skips the ~2–3 s
/// session/new (each request still gets its own session, so answers never see each other's text).
struct Acp { kind: Kind, rpc: Rpc, spare: std::sync::Mutex<Option<oneshot::Receiver<Value>>> }

/// Marks a start refused because this agy has no `acp` (agy_friendly turns it into an upgrade hint).
const AGY_TOO_OLD: &str = "agy has no acp server";

/// `agy acp --help` per binary (path, size, modified time), so an update asks again.
static AGY_HELP: once_cell::sync::Lazy<std::sync::Mutex<HashMap<String, String>>> = once_cell::sync::Lazy::new(Default::default);

async fn agy_acp_help(path: &Path) -> String {
    let key = match std::fs::metadata(path) {
        Ok(md) => format!("{}|{}|{:?}", path.display(), md.len(), md.modified().ok()),
        Err(_) => path.display().to_string(),
    };
    if let Some(h) = AGY_HELP.lock().unwrap().get(&key) { return h.clone(); }
    let help = output(path, &["acp", "--help"], 15).await.unwrap_or_default();
    // An empty answer (timed out) is asked again next time.
    if !help.trim().is_empty() { AGY_HELP.lock().unwrap().insert(key, help.clone()); }
    help
}

/// The optional flags this agy's `acp` takes. An agy refuses to start on a flag it doesn't know (both came in 1.5.x),
/// so each is passed only when its help lists it, as magpie's agy bridge does. Err when the help shows no ACP server
/// at all (an agy from before `acp`); an empty help (it didn't answer) is not known to be old, so it goes ahead.
fn agy_flags(help: &str) -> Result<Vec<&'static str>> {
    if !help.trim().is_empty() && !help.contains("--agent") && !help.to_lowercase().contains("agent client protocol") {
        return Err(anyhow!("{AGY_TOO_OLD}"));
    }
    let mut v = Vec::new();
    if help.contains("--include-default-customizations") { v.push("--include-default-customizations=false"); }
    if help.contains("--disable-telemetry") || help.contains("--disable_telemetry") { v.push("--disable-telemetry"); }
    Ok(v)
}

/// The effort words agy puts after a model family in the ids it lists ("gemini-3.8-flash-medium").
const EFFORTS: [&str; 6] = ["minimal", "low", "medium", "high", "xhigh", "max"];

/// Whether a session/set_config_option answer shows `id` set to `value`. agy answers a value it doesn't offer (one of
/// its own listed ids, "gemini-4-neon-high", for the "gemini-4-neon" option) with success and changes nothing, so
/// only its read-back says. An answer without configOptions is taken at its word.
fn config_applied(result: &Value, id: &str, value: &str) -> bool {
    match result["configOptions"].as_array() {
        Some(opts) => opts.iter().find(|o| o["id"].as_str() == Some(id)).is_some_and(|o| o["currentValue"].as_str() == Some(value)),
        None => true,
    }
}

/// A listed id as the model option's value plus a thinking level ("gemini-3.8-flash-medium" → "gemini-3.8-flash",
/// "medium"), from the configOptions an answer gave; the longest value that fits.
fn split_leveled(config_options: &Value, model: &str) -> Option<(String, String)> {
    let opt = config_options.as_array()?.iter().find(|o| o["id"].as_str() == Some("model") || o["category"].as_str() == Some("model"))?;
    let values: Vec<String> = opt["options"].as_array()?.iter()
        .flat_map(|x| x["options"].as_array().cloned().unwrap_or_else(|| vec![x.clone()]))
        .filter_map(|x| x["value"].as_str().map(str::to_string)).collect();
    values.into_iter()
        .filter_map(|v| { let level = model.strip_prefix(v.as_str())?.strip_prefix('-')?; EFFORTS.contains(&level).then(|| (v.clone(), level.to_string())) })
        .max_by_key(|(v, _)| v.len())
}

fn acp_args(kind: Kind) -> &'static [&'static str] {
    match kind {
        // Its own agent definition (written into the work folder): no tools, none of the user's MCP servers,
        // skills or customisations.
        // --include-default-customizations=false and --disable-telemetry too, where this agy has them (agy_flags).
        Kind::Antigravity => &["acp", "--agent", "subcast"],
        Kind::Kimi => &["acp"], Kind::Opencode => &["acp"], _ => &[]
    }
}

/// Gemini CLI has taken `--acp` since 0.34 and `--experimental-acp` from 0.1.13 (still accepted, deprecated);
/// Qwen Code, its fork, followed it. Older versions have neither and exit with yargs' "Unknown argument: acp".
const ACP_FLAGS: [&str; 2] = ["--acp", "--experimental-acp"];

/// The flag each executable accepted, so later starts try it first and don't spend a process on the other.
static ACP_FLAG: once_cell::sync::Lazy<std::sync::Mutex<HashMap<PathBuf, &'static str>>> = once_cell::sync::Lazy::new(Default::default);

fn uses_acp_flag(kind: Kind) -> bool { matches!(kind, Kind::Gemini | Kind::Qwen) }

/// The flags to try, the one that worked last time first.
fn acp_flag_order(known: Option<&'static str>) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = known.into_iter().collect();
    v.extend(ACP_FLAGS.iter().copied().filter(|f| Some(*f) != known));
    v
}

/// What yargs prints when a version doesn't know the flag: "Unknown argument: acp",
/// "Unknown arguments: experimental-acp, experimentalAcp".
fn is_unknown_acp_arg(stderr: &str) -> bool {
    stderr.lines().any(|l| { let l = l.to_lowercase(); l.contains("unknown argument") && l.contains("acp") })
}

fn upgrade_command(kind: Kind) -> &'static str {
    if kind == Kind::Qwen { "npm i -g @qwen-code/qwen-code" } else { "npm i -g @google/gemini-cli" }
}

/// Said instead of the raw yargs error when neither flag is known.
fn too_old(kind: Kind, path: &Path) -> anyhow::Error {
    let (name, at, cmd) = (display(kind), path.display(), upgrade_command(kind));
    anyhow!("{name} 版本太旧，不支持 SubCast 需要的 ACP 模式（{at}）。请在终端运行 {cmd} 升级，再回到 SubCast 点「重新检测」")
}

async fn initialize(rpc: &Rpc) -> Result<()> {
    // No file system and no terminal for the agent.
    rpc.request("initialize", json!({ "protocolVersion": 1, "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
        "clientInfo": { "name": "subcast", "version": env!("CARGO_PKG_VERSION") } }), 60).await?;
    Ok(())
}

/// Starts Gemini CLI or Qwen Code with the ACP flag its version knows.
async fn spawn_with_acp_flag(kind: Kind, path: &Path) -> Result<Rpc> {
    let known = ACP_FLAG.lock().unwrap().get(path).copied();
    for flag in acp_flag_order(known) {
        let rpc = Rpc::spawn(path, &[flag]).await?;
        match initialize(&rpc).await {
            Ok(()) => { ACP_FLAG.lock().unwrap().insert(path.to_path_buf(), flag); return Ok(rpc); }
            Err(e) => {
                let rejected = rpc.stderr_after_exit().await.is_some_and(|s| is_unknown_acp_arg(&s));
                rpc.close().await;
                if !rejected { return Err(e); }
            }
        }
    }
    // Not remembered: after upgrading, 重新检测 tries again.
    ACP_FLAG.lock().unwrap().remove(path);
    Err(too_old(kind, path))
}

impl Acp {
    async fn start(kind: Kind, path: &Path) -> Result<Acp> {
        let mut args: Vec<&str> = acp_args(kind).to_vec();
        if kind == Kind::Antigravity {
            args.extend(agy_flags(&agy_acp_help(path).await)?);
            let dir = work_dir()?.join(".agents/agents/subcast");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("agent.md"), "---\nname: subcast\ndescription: Answers in text only\nmainAgent: true\nsubagent: false\ntools: []\ninheritMcp: false\ninheritCustomizations: false\nexcludeDefaultComponents: true\n---\nAnswer in text only. You have no tools.\n")?;
        }
        let rpc = if uses_acp_flag(kind) { spawn_with_acp_flag(kind, path).await? } else {
            let rpc = Rpc::spawn(path, &args).await?;
            initialize(&rpc).await?;
            rpc
        };
        let acp = Acp { kind, rpc, spare: std::sync::Mutex::new(None) };
        acp.prepare_session().await;
        Ok(acp)
    }

    /// Asks for the next session without waiting for it.
    async fn prepare_session(&self) {
        let Ok(dir) = work_dir() else { return };
        let id = self.rpc.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.rpc.waiting.lock().await.insert(id, tx);
        if self.rpc.send(&json!({ "jsonrpc": "2.0", "id": id, "method": "session/new", "params": { "cwd": dir.to_string_lossy(), "mcpServers": [] } })).await.is_ok() {
            *self.spare.lock().unwrap() = Some(rx);
        }
    }

    /// The prepared session if it came through, else a new one.
    async fn session(&self) -> Result<String> {
        let spare = self.spare.lock().unwrap().take();
        if let Some(rx) = spare {
            if let Ok(Ok(v)) = tokio::time::timeout(Duration::from_secs(30), rx).await {
                if let Some(id) = v["result"]["sessionId"].as_str() { return Ok(id.to_string()); }
            }
        }
        self.new_session().await
    }

    async fn new_session(&self) -> Result<String> {
        let r = self.rpc.request("session/new", json!({ "cwd": work_dir()?.to_string_lossy(), "mcpServers": [] }), 60).await?;
        r["sessionId"].as_str().map(str::to_string).ok_or_else(|| anyhow!("本机 AI 没有返回会话"))
    }

    async fn ask(&self, req: &Request<'_>, cancel: Arc<AtomicBool>, on_delta: &(impl Fn(&str) + Send)) -> Result<String> {
        // A session per request (cheap in a running agent), so one answer never sees another's text.
        let session = self.session().await?;
        if let Some(m) = req.model.filter(|m| !m.is_empty()) {
            // A model the agent doesn't take answers with its own default; agy's is a slow high-thinking one, and its
            // choice is the listener's, so it says so rather than answering on another model.
            if !self.select_model(&session, m).await && self.kind == Kind::Antigravity {
                return Err(anyhow!("Antigravity CLI 现在不提供所选的模型「{m}」，请在 SubCast 的设置里重新选一个模型"));
            }
        }
        // ACP has no system prompt: it goes first in the message.
        let prompt = if req.system.is_empty() { req.user.to_string() } else { format!("{}\n\n---\n\n{}", req.system, req.user) };
        let id = self.rpc.next.fetch_add(1, Ordering::Relaxed);
        let (tx, mut rx) = oneshot::channel();
        self.rpc.waiting.lock().await.insert(id, tx);
        self.rpc.send(&json!({ "jsonrpc": "2.0", "id": id, "method": "session/prompt", "params": { "sessionId": session, "prompt": [{ "type": "text", "text": prompt }] } })).await?;
        let started = Instant::now();
        let mut text = String::new();
        loop {
            if let Ok(done) = rx.try_recv() {
                if let Some(e) = done.get("error") { return Err(anyhow!("{}", e["message"].as_str().unwrap_or("本机 AI 出错"))); }
                // Late chunks can still be queued: take what is there.
                while let Ok(v) = self.rpc.event(0).await { self.on_update(&v, &session, &mut text, on_delta).await; }
                self.prepare_session().await;
                // agy's failed model call is an ordinary reply: its last line is the error (friendly() words it).
                if self.kind == Kind::Antigravity { if let Some(line) = agy_error_line(&text) { return Err(anyhow!("{line}")); } }
                return Ok(text);
            }
            if cancel.load(Ordering::Relaxed) {
                let _ = self.rpc.notify("session/cancel", json!({ "sessionId": session })).await;
                return Err(anyhow!("已取消"));
            }
            if started.elapsed() > Duration::from_secs(300) { return Err(anyhow!("本机 AI 超过 5 分钟没有答完")); }
            let v = match tokio::time::timeout(Duration::from_millis(200), async { self.rpc.events.lock().await.recv().await }).await {
                Ok(Some(v)) => v,
                Ok(None) => return Err(anyhow!("本机 AI 意外退出：{}", self.rpc.last_error())),
                Err(_) => continue,
            };
            self.on_update(&v, &session, &mut text, on_delta).await;
        }
    }

    /// Sets the session's model: as a config option (newer agents), else with set_model (older ones; agy also takes
    /// its listed ids there, level and all), else as agy's config values, the family and then its thinking level.
    /// Whether it took.
    async fn select_model(&self, session: &str, m: &str) -> bool {
        let set = |id: &str, value: &str| self.rpc.request("session/set_config_option", json!({ "sessionId": session, "configId": id, "value": value }), 15);
        let first = set("model", m).await;
        if first.as_ref().is_ok_and(|r| config_applied(r, "model", m)) { return true; }
        if self.rpc.request("session/set_model", json!({ "sessionId": session, "modelId": m }), 15).await.is_ok() { return true; }
        let Some((family, level)) = first.ok().and_then(|r| split_leveled(&r["configOptions"], m)) else { return false };
        // The family first: a model change can replace the thinking levels on offer.
        set("model", &family).await.is_ok_and(|r| config_applied(&r, "model", &family))
            && set("thought_level", &level).await.is_ok_and(|r| config_applied(&r, "thought_level", &level))
    }

    async fn on_update(&self, v: &Value, session: &str, text: &mut String, on_delta: &(impl Fn(&str) + Send)) {
        let m = v["method"].as_str().unwrap_or("");
        if v.get("id").is_some() && !m.is_empty() {
            // Requests from the agent: permissions are refused, anything else (files, terminals) is unsupported.
            let _ = if m == "session/request_permission" { self.rpc.reply(&v["id"], json!({ "outcome": { "outcome": "cancelled" } })).await }
                else { self.rpc.reply_error(&v["id"], "not supported").await };
            return;
        }
        if m != "session/update" || v["params"]["sessionId"].as_str() != Some(session) { return; }
        let u = &v["params"]["update"];
        if u["sessionUpdate"] == "agent_message_chunk" && u["content"]["type"] == "text" {
            if let Some(d) = u["content"]["text"].as_str() { text.push_str(d); on_delta(d); }
        }
    }

    async fn close(&self) { self.rpc.close().await }
}

#[cfg(test)]
mod live {
    use super::*;
    /// AGENTS=claude,codex,gemini… : asks each a tiny question twice (cold, then warm) and prints the times.
    /// MODEL=… asks on that model; ASK=… asks that instead.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn ask_each_agent() {
        let want = std::env::var("AGENTS").unwrap_or_default();
        for f in detect().await {
            println!("found {:?} {} login={} at {}", f.kind, f.version, f.login, f.path);
            if !want.split(',').any(|w| w == f.kind.id()) { continue; }
            for round in ["cold", "warm"] {
                if round == "warm" { tokio::time::sleep(Duration::from_secs(5)).await; } // as between two questions
                let t0 = Instant::now();
                let (model, ask) = (std::env::var("MODEL").ok(), std::env::var("ASK").ok());
                let user = ask.as_deref().unwrap_or(r#"Reply exactly {"ok":true}"#);
                let r = complete(f.kind, Path::new(&f.path), Request { system: if ask.is_some() { "" } else { "Answer with JSON only." }, user, model: model.as_deref(), json: ask.is_none() }, Arc::new(AtomicBool::new(false)), |_| {}).await;
                println!("  {round}: {:.1}s -> {:?}", t0.elapsed().as_secs_f64(), r.map(|s| s.chars().take(80).collect::<String>()));
            }
        }
    }
}

#[cfg(test)]
mod model_list_tests {
    use super::*;
    #[test]
    fn reads_codex_and_acp_listings() {
        let codex = json!({ "data": [{ "id": "a", "model": "gpt-x", "displayName": "GPT X" }, { "model": "old", "hidden": true }] });
        assert_eq!(parse_codex_models(&codex), vec![AgentModel { id: "gpt-x".into(), name: "GPT X".into() }]);
        let acp = json!({ "sessionId": "s", "models": { "currentModelId": "k3", "availableModels": [{ "modelId": "k3", "name": "Kimi K3" }] } });
        assert_eq!(parse_acp_models(&acp)[0].id, "k3");
        let cfg = json!({ "configOptions": [{ "id": "model", "options": [{ "group": "x", "options": [{ "value": "m1", "name": "M1" }] }, { "value": "m2" }] }] });
        assert_eq!(parse_acp_models(&cfg).iter().map(|m| m.id.as_str()).collect::<Vec<_>>(), ["m1", "m2"]);
    }
}

#[cfg(test)]
mod agy_tests {
    use super::*;

    // As agy 1.5.16 answers session/new (shortened).
    fn options() -> Value {
        json!([
            { "id": "model", "category": "model", "type": "select", "currentValue": "gemini-4-neon", "options": [
                { "value": "gemini-4-neon", "name": "Gemini 4 Neon" }, { "value": "gemini-3.8-flash", "name": "Gemini 3.8 Flash" },
                { "value": "claude-sonnet-5-5", "name": "Claude Sonnet 5.5" }, { "value": "gpt-oss-120b-medium", "name": "GPT-OSS 120B (Medium)" }] },
            { "id": "thought_level", "category": "thought_level", "type": "select", "currentValue": "high", "options": [{ "value": "low" }, { "value": "high" }] }
        ])
    }

    #[test]
    fn a_listed_id_agy_ignores_is_not_applied() {
        // agy answered set_config_option model=gemini-4-neon-high with success and kept gemini-4-neon.
        let r = json!({ "configOptions": options() });
        assert!(!config_applied(&r, "model", "gemini-4-neon-high"));
        assert!(config_applied(&r, "model", "gemini-4-neon"));
        assert!(config_applied(&r, "thought_level", "high"));
        // Agents that answer {} are taken at their word.
        assert!(config_applied(&json!({}), "model", "anything"));
    }

    #[test]
    fn listed_ids_split_into_family_and_level() {
        let o = options();
        assert_eq!(split_leveled(&o, "gemini-3.8-flash-medium"), Some(("gemini-3.8-flash".into(), "medium".into())));
        assert_eq!(split_leveled(&o, "claude-sonnet-5-5-low"), Some(("claude-sonnet-5-5".into(), "low".into())));
        // A value that is itself a model (its "-medium" is part of the name) has nothing to split.
        assert_eq!(split_leveled(&o, "gpt-oss-120b-medium"), None);
        assert_eq!(split_leveled(&o, "gemini-9-unknown-high"), None);
        assert_eq!(split_leveled(&o, "gemini-3.8-flash-turbo"), None);
    }

    #[test]
    fn optional_flags_follow_the_help() {
        let new = "Usage: agy acp [flags]\n\nStart the Agent Client Protocol (ACP) stdio server\n  --agent  Agent\n  --disable-telemetry  If true\n  --include-default-customizations  Include default customizations (default true)\n";
        assert_eq!(agy_flags(new).unwrap(), ["--include-default-customizations=false", "--disable-telemetry"]);
        let older = "Usage: agy acp [flags]\n\nStart the Agent Client Protocol (ACP) stdio server\n  --agent  Agent\n";
        assert!(agy_flags(older).unwrap().is_empty());
        // No answer: not known to be old.
        assert!(agy_flags("").unwrap().is_empty());
        // An agy from before `acp` prints its own usage.
        let before = "Usage of agy:\n  --model  Model for the current CLI session\n  -p  Short alias for --print\n";
        let e = agy_flags(before).unwrap_err();
        assert!(friendly(Kind::Antigravity, e).to_string().contains("agy update"));
    }

    #[test]
    fn errors_agy_writes_as_the_reply() {
        let quota = "Here is part of it.\n\n⚠ Individual quota reached. Please upgrade your subscription to increase your limits. Resets in 1h18m40s.\n";
        let line = agy_error_line(quota).unwrap();
        let f = friendly(Kind::Antigravity, anyhow!("{line}")).to_string();
        assert!(f.contains("1h18m40s") && f.contains("额度用完"), "{f}");
        let third = friendly(Kind::Antigravity, anyhow!("⚠ Third-party quota reached. Please switch to Gemini models. Resets in 3h22m19s.")).to_string();
        assert!(third.contains("Claude / GPT") && third.contains("3h22m19s"), "{third}");
        let r429 = agy_error_line("\n⚠ RESOURCE_EXHAUSTED (code 429): Resource has been exhausted (e.g. check quota).\n").unwrap();
        assert!(friendly(Kind::Antigravity, anyhow!("{r429}")).to_string().contains("429"));
        let busy = "⚠ Our servers are experiencing high traffic right now, please try again in a minute. (UNAVAILABLE (code 503): No capacity available for model gemini-3.8-flash-high on the server)";
        assert!(friendly(Kind::Antigravity, anyhow!("{}", agy_error_line(busy).unwrap())).to_string().contains("服务器正忙"));
        let banned = "⚠ This service has been disabled in this account for violation of Terms of Service. Please submit an appeal to continue using this product.";
        assert!(friendly(Kind::Antigravity, anyhow!("{}", agy_error_line(banned).unwrap())).to_string().contains("申诉"));
        // An answer that merely mentions a warning is an answer.
        assert_eq!(agy_error_line("{\"ok\":true}"), None);
        assert_eq!(agy_error_line("⚠ Note: this word has two meanings."), None);
        assert_eq!(agy_error_line("⚠ RESOURCE_EXHAUSTED came up earlier.\nBut here is the answer."), None);
    }

    #[test]
    fn start_failures_of_an_old_agy_say_to_update() {
        let f = friendly(Kind::Antigravity, anyhow!("本机 AI 意外退出：flag provided but not defined: -disable-telemetry")).to_string();
        assert!(f.contains("agy update"), "{f}");
        // Other agents keep their own wording.
        let g = friendly(Kind::Kimi, anyhow!("flag provided but not defined: -x")).to_string();
        assert!(!g.contains("agy"));
    }
}

#[cfg(test)]
mod acp_flag_tests {
    use super::*;
    #[test]
    fn tries_the_remembered_flag_first() {
        assert_eq!(acp_flag_order(None), ["--acp", "--experimental-acp"]);
        assert_eq!(acp_flag_order(Some("--experimental-acp")), ["--experimental-acp", "--acp"]);
        assert_eq!(acp_flag_order(Some("--acp")), ["--acp", "--experimental-acp"]);
        assert!(uses_acp_flag(Kind::Gemini) && uses_acp_flag(Kind::Qwen) && !uses_acp_flag(Kind::Kimi));
        assert!(acp_args(Kind::Gemini).is_empty());
    }

    #[test]
    fn recognises_yargs_rejecting_the_flag() {
        // What Gemini CLI 0.1.9 prints for each flag.
        assert!(is_unknown_acp_arg("Usage: gemini [options]\n\nUnknown argument: acp\n"));
        assert!(is_unknown_acp_arg("Unknown arguments: experimental-acp, experimentalAcp"));
        assert!(!is_unknown_acp_arg("Unknown argument: disable-telemetry"));
        assert!(!is_unknown_acp_arg("Error authenticating: acp session failed"));
        assert!(!is_unknown_acp_arg(""));
    }

    #[test]
    fn too_old_names_the_install_and_the_upgrade() {
        let m = too_old(Kind::Gemini, Path::new("/opt/homebrew/bin/gemini")).to_string();
        assert!(m.starts_with("Gemini CLI 版本太旧"), "{m}");
        assert!(m.contains("/opt/homebrew/bin/gemini") && m.contains("npm i -g @google/gemini-cli"), "{m}");
        assert!(!m.contains("Unknown argument"));
        assert!(too_old(Kind::Qwen, Path::new("qwen")).to_string().contains("@qwen-code/qwen-code"));
    }

    /// GEMINI_PATH=/path/to/gemini : starts it the way SubCast does and prints the outcome and the flag it took.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn start_given_gemini() {
        let path = PathBuf::from(std::env::var("GEMINI_PATH").expect("GEMINI_PATH"));
        let r = probe(Kind::Gemini, &path).await;
        println!("probe: {r:?}\nflag: {:?}", ACP_FLAG.lock().unwrap().get(&path));
    }
}
