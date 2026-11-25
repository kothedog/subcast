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
            let conn = Acp::start(kind, path).await?;
            let r = conn.rpc.request("session/new", json!({ "cwd": work_dir()?.to_string_lossy(), "mcpServers": [] }), 60).await;
            conn.close().await;
            Ok(parse_acp_models(&r?))
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
    let conn = POOL.take(kind, path, req.system, req.model, &cancel).await?;
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
    let name = display(kind);
    let l = m.to_lowercase();
    if l.contains("no longer supported") || l.contains("migrate to") {
        anyhow!("Google 已不再让个人账号使用 {name}。个人 Google 账号请改用 Antigravity CLI（agy）；{name} 只适用于 API Key 或企业版账号")
    } else if l.contains("model is unavailable") || l.contains("model not found") || l.contains("unknown model") {
        anyhow!("{name} 的默认模型不可用，请在 SubCast 里为它选一个模型")
    } else if l.contains("not logged in") || l.contains("login") || l.contains("unauthor") || l.contains("auth_required") || l.contains("authentication") {
        anyhow!("{name} 需要登录：在终端运行 {} 完成登录后，回到 SubCast 点「重新检测」", kind.exe())
    } else if kind == Kind::Antigravity && (l.contains("resource exhausted") || l.contains("429")) {
        // agy answers some prompts with a "Resource exhausted" that has nothing to do with quota.
        anyhow!("{name} 暂时拒绝了这次请求（不一定是额度用完），稍后再试：{}", m.chars().take(160).collect::<String>())
    } else if l.contains("rate limit") || l.contains("usage limit") || l.contains("quota") || l.contains("429") || l.contains("limit reached") {
        anyhow!("{name} 的订阅额度暂时用完了（请求太频繁或额度不足）：{}", m.chars().take(160).collect::<String>())
    } else { e }
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
            let tail = err_tail.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut t = tail.lock().unwrap();
                    t.push_str(&line); t.push('\n');
                    if t.len() > 4000 { let cut = t.len() - 2000; *t = t[cut..].to_string(); }
                }
            });
        }
        Ok(Rpc { child: Mutex::new(Some(child)), stdin: Mutex::new(stdin), next: AtomicU64::new(1), waiting, events: Mutex::new(rx), dead, stderr: err_tail })
    }

    fn alive(&self) -> bool { !self.dead.load(Ordering::Relaxed) }

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
struct Acp { rpc: Rpc, spare: std::sync::Mutex<Option<oneshot::Receiver<Value>>> }

fn acp_args(kind: Kind) -> &'static [&'static str] {
    match kind {
        // Its own agent definition (written into the work folder): no tools, none of the user's MCP servers,
        // skills or customisations.
        Kind::Antigravity => &["acp", "--agent", "subcast", "--include-default-customizations=false", "--disable-telemetry"],
        Kind::Gemini => &["--acp"], Kind::Qwen => &["--acp"], Kind::Kimi => &["acp"], Kind::Opencode => &["acp"], _ => &[]
    }
}

impl Acp {
    async fn start(kind: Kind, path: &Path) -> Result<Acp> {
        if kind == Kind::Antigravity {
            let dir = work_dir()?.join(".agents/agents/subcast");
            std::fs::create_dir_all(&dir)?;
            std::fs::write(dir.join("agent.md"), "---\nname: subcast\ndescription: Answers in text only\nmainAgent: true\nsubagent: false\ntools: []\ninheritMcp: false\ninheritCustomizations: false\nexcludeDefaultComponents: true\n---\nAnswer in text only. You have no tools.\n")?;
        }
        let rpc = Rpc::spawn(path, acp_args(kind)).await?;
        // No file system and no terminal for the agent.
        rpc.request("initialize", json!({ "protocolVersion": 1, "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
            "clientInfo": { "name": "subcast", "version": env!("CARGO_PKG_VERSION") } }), 60).await?;
        let acp = Acp { rpc, spare: std::sync::Mutex::new(None) };
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
            // Newer agents take the model as a config option; older ones have set_model.
            if self.rpc.request("session/set_config_option", json!({ "sessionId": session, "configId": "model", "value": m }), 15).await.is_err() {
                let _ = self.rpc.request("session/set_model", json!({ "sessionId": session, "modelId": m }), 15).await;
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
                let r = complete(f.kind, Path::new(&f.path), Request { system: "Answer with JSON only.", user: r#"Reply exactly {"ok":true}"#, model: None, json: true }, Arc::new(AtomicBool::new(false)), |_| {}).await;
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
