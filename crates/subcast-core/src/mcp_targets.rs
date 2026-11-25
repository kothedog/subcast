//! Connecting SubCast's MCP server (`subcast-cli mcp`) to the AI agents on this computer, so they can call it
//! ("总结 Lex Fridman 最新一期"). Each agent is written the way it expects: through its own `mcp add` command when it
//! has one, else by adding one entry to its JSON config (backed up first, never rewritten when it doesn't parse).
use crate::agents::{self, Kind};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const NAME: &str = "subcast";

#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct McpTarget {
    pub id: &'static str,
    pub name: &'static str,
    /// The provider id its icon goes by in the app.
    pub icon: &'static str,
    pub connected: bool,
    /// Connected, but to a SubCast that is no longer there (the app moved): connecting again fixes it.
    pub outdated: bool,
    /// Where the entry lives, shown to the user.
    pub config: String,
}

#[derive(Clone, Copy)]
enum How {
    /// The agent's own command line, plus the file it writes to (to read the state back).
    Cli(Kind),
    /// One entry in a JSON file: `mcpServers.subcast` = {command, args}, or OpenCode's `mcp.subcast`.
    Json,
    OpenCode,
}

struct Def { id: &'static str, name: &'static str, icon: &'static str, how: How }

const DEFS: &[Def] = &[
    Def { id: "claude-code", name: "Claude Code", icon: "cli-claude", how: How::Cli(Kind::Claude) },
    Def { id: "claude-desktop", name: "Claude 桌面版", icon: "anthropic", how: How::Json },
    Def { id: "codex", name: "Codex", icon: "cli-codex", how: How::Cli(Kind::Codex) },
    Def { id: "antigravity", name: "Antigravity CLI", icon: "cli-antigravity", how: How::Cli(Kind::Antigravity) },
    Def { id: "gemini", name: "Gemini CLI", icon: "cli-gemini", how: How::Cli(Kind::Gemini) },
    Def { id: "qwen", name: "Qwen Code", icon: "cli-qwen", how: How::Cli(Kind::Qwen) },
    Def { id: "kimi", name: "Kimi Code", icon: "cli-kimi", how: How::Json },
    Def { id: "opencode", name: "OpenCode", icon: "cli-opencode", how: How::OpenCode },
    Def { id: "cursor", name: "Cursor", icon: "cursor", how: How::Json },
];

fn home() -> PathBuf { dirs::home_dir().unwrap_or_default() }

/// The config file holding the entry.
fn config_path(id: &str) -> PathBuf {
    let h = home();
    match id {
        "claude-code" => h.join(".claude.json"),
        "claude-desktop" => dirs::config_dir().unwrap_or_default().join("Claude/claude_desktop_config.json"),
        "codex" => std::env::var_os("CODEX_HOME").map(PathBuf::from).unwrap_or_else(|| h.join(".codex")).join("config.toml"),
        "antigravity" => h.join(".gemini/config/mcp_config.json"),
        "gemini" => h.join(".gemini/settings.json"),
        "qwen" => h.join(".qwen/settings.json"),
        "kimi" => h.join(".kimi-code/mcp.json"),
        "opencode" => dirs::home_dir().unwrap_or_default().join(".config/opencode/opencode.json"),
        "cursor" => h.join(".cursor/mcp.json"),
        _ => PathBuf::new(),
    }
}

/// Whether the agent is on this computer.
fn present(d: &Def) -> bool {
    match d.how {
        How::Cli(k) => agents::path_of(k).is_some(),
        _ => match d.id {
            "claude-desktop" => (cfg!(target_os = "macos") || cfg!(target_os = "windows")) && config_path(d.id).parent().is_some_and(Path::exists),
            "kimi" => agents::path_of(Kind::Kimi).is_some(),
            "opencode" => agents::path_of(Kind::Opencode).is_some(),
            "cursor" => home().join(".cursor").is_dir(),
            _ => false,
        },
    }
}

/// The command the entry runs now, if there is an entry.
fn current_command(id: &str) -> Option<String> {
    let path = config_path(id);
    let text = std::fs::read_to_string(&path).ok()?;
    if id == "codex" {
        // [mcp_servers.subcast] … command = "…"
        let start = text.find(&format!("[mcp_servers.{NAME}]"))?;
        let rest = &text[start..];
        let block = &rest[..rest[1..].find("\n[").map(|i| i + 1).unwrap_or(rest.len())];
        let line = block.lines().find(|l| l.trim_start().starts_with("command"))?;
        let raw = line.split_once('=')?.1.trim();
        let val = if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
            raw[1..raw.len() - 1].replace("\\\\", "\\")
        } else {
            raw.trim_matches('\'').to_string()
        };
        return Some(val);
    }
    let v: Value = serde_json::from_str(&text).ok()?;
    if id == "opencode" {
        let e = &v["mcp"][NAME];
        return e["command"][0].as_str().map(str::to_string).or_else(|| e.is_object().then(String::new));
    }
    let e = &v["mcpServers"][NAME];
    e["command"].as_str().map(str::to_string).or_else(|| e.is_object().then(String::new))
}

fn target(d: &Def, cli: &Path) -> McpTarget {
    let cmd = current_command(d.id);
    let connected = cmd.is_some();
    let outdated = cmd.as_deref().is_some_and(|c| !c.is_empty() && Path::new(c) != cli);
    McpTarget { id: d.id, name: d.name, icon: d.icon, connected, outdated, config: config_path(d.id).to_string_lossy().into() }
}

/// The agents on this computer and whether each has SubCast.
pub fn list(cli: &Path) -> Vec<McpTarget> {
    DEFS.iter().filter(|d| present(d)).map(|d| target(d, cli)).collect()
}

/// Adds SubCast to one agent (or points an old entry at this copy of the app).
pub async fn connect(id: &str, cli: &Path) -> Result<McpTarget> {
    let d = DEFS.iter().find(|d| d.id == id).ok_or_else(|| anyhow!("未知的 AI 助手：{id}"))?;
    let c = cli.to_string_lossy().to_string();
    match d.how {
        How::Cli(kind) => {
            let exe = agents::path_of(kind).ok_or_else(|| anyhow!("没有找到 {}", d.name))?;
            let _ = agents::run_cli(&exe, &remove_args(kind), 30).await; // replace an old entry
            let add = add_args(kind, &c);
            let add: Vec<&str> = add.iter().map(String::as_str).collect();
            let (ok, out) = agents::run_cli(&exe, &add, 60).await?;
            if !ok || current_command(id).is_none() {
                // Agents that keep it in JSON can still be written directly.
                if matches!(kind, Kind::Antigravity | Kind::Gemini | Kind::Qwen) {
                    edit_json(&config_path(id), |v| { v["mcpServers"][NAME] = json!({ "command": c, "args": ["mcp"] }); })?;
                } else {
                    return Err(anyhow!("{} 没有接受：{}", d.name, out.trim().lines().last().unwrap_or("")));
                }
            }
        }
        How::Json => edit_json(&config_path(id), |v| { v["mcpServers"][NAME] = json!({ "command": c, "args": ["mcp"] }); })?,
        How::OpenCode => edit_json(&config_path(id), |v| { v["mcp"][NAME] = json!({ "type": "local", "command": [c, "mcp"], "enabled": true }); })?,
    }
    Ok(target(d, cli))
}

/// Takes SubCast out of one agent.
pub async fn disconnect(id: &str, cli: &Path) -> Result<McpTarget> {
    let d = DEFS.iter().find(|d| d.id == id).ok_or_else(|| anyhow!("未知的 AI 助手：{id}"))?;
    match d.how {
        How::Cli(kind) => {
            let exe = agents::path_of(kind).ok_or_else(|| anyhow!("没有找到 {}", d.name))?;
            let (ok, out) = agents::run_cli(&exe, &remove_args(kind), 30).await?;
            if current_command(id).is_some() {
                if matches!(kind, Kind::Antigravity | Kind::Gemini | Kind::Qwen) {
                    edit_json(&config_path(id), |v| { if let Some(m) = v["mcpServers"].as_object_mut() { m.remove(NAME); } })?;
                } else if !ok {
                    return Err(anyhow!("{} 没有移除：{}", d.name, out.trim().lines().last().unwrap_or("")));
                }
            }
        }
        How::Json => edit_json(&config_path(id), |v| { if let Some(m) = v["mcpServers"].as_object_mut() { m.remove(NAME); } })?,
        How::OpenCode => edit_json(&config_path(id), |v| { if let Some(m) = v["mcp"].as_object_mut() { m.remove(NAME); } })?,
    }
    Ok(target(d, cli))
}

/// Points every outdated entry at this copy of the app (after the app moved or was renamed).
pub async fn repair(cli: &Path) {
    for t in list(cli).into_iter().filter(|t| t.outdated) {
        let _ = connect(t.id, cli).await;
    }
}

fn add_args(kind: Kind, cli: &str) -> Vec<String> {
    let v: Vec<&str> = match kind {
        Kind::Claude => vec!["mcp", "add", "--scope", "user", NAME, "--", cli, "mcp"],
        Kind::Codex => vec!["mcp", "add", NAME, "--", cli, "mcp"],
        Kind::Gemini | Kind::Qwen => vec!["mcp", "add", "-s", "user", NAME, cli, "mcp"],
        _ => vec!["mcp", "add", NAME, cli, "mcp"],
    };
    v.into_iter().map(String::from).collect()
}

fn remove_args(kind: Kind) -> Vec<&'static str> {
    match kind {
        Kind::Claude => vec!["mcp", "remove", "--scope", "user", NAME],
        Kind::Gemini | Kind::Qwen => vec!["mcp", "remove", "-s", "user", NAME],
        _ => vec!["mcp", "remove", NAME],
    }
}

/// Changes one JSON config in place: a copy of the original is kept once (`<file>.subcast-backup`); a file that
/// doesn't parse is left alone.
fn edit_json(path: &Path, change: impl FnOnce(&mut Value)) -> Result<()> {
    let mut v: Value = match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => json!({}),
        Ok(t) => {
            let v: Value = serde_json::from_str(&t).map_err(|e| anyhow!("{} 不是有效的 JSON，没有改动：{e}", path.display()))?;
            let backup = path.with_extension(format!("{}subcast-backup", path.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()));
            if !backup.exists() { let _ = std::fs::write(&backup, &t); }
            v
        }
        Err(_) => json!({}),
    };
    if !v.is_object() { return Err(anyhow!("{} 的格式不认识，没有改动", path.display())); }
    change(&mut v);
    if let Some(dir) = path.parent() { std::fs::create_dir_all(dir)?; }
    std::fs::write(path, serde_json::to_string_pretty(&v)? + "\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_json_without_losing_other_entries() {
        let dir = std::env::temp_dir().join(format!("subcast-mcp-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("mcp.json");
        std::fs::write(&f, r#"{"mcpServers":{"kaze":{"command":"k"}},"other":1}"#).unwrap();
        edit_json(&f, |v| { v["mcpServers"][NAME] = json!({ "command": "/a/subcast-cli", "args": ["mcp"] }); }).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&f).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["kaze"]["command"], "k");
        assert_eq!(v["other"], 1);
        assert_eq!(v["mcpServers"][NAME]["command"], "/a/subcast-cli");
        assert!(dir.join("mcp.json.subcast-backup").exists());
        std::fs::write(&f, "{ not json").unwrap();
        assert!(edit_json(&f, |_| {}).is_err());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{ not json");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod live {
    use super::*;
    /// MCP_CONNECT=antigravity connects then disconnects that agent; without it, only lists.
    #[tokio::test]
    #[ignore]
    async fn list_and_round_trip() {
        let cli = Path::new("/Applications/SubCast.app/Contents/MacOS/subcast-cli");
        for t in list(cli) { println!("{:<15} connected={} outdated={} {}", t.id, t.connected, t.outdated, t.config); }
        if let Ok(id) = std::env::var("MCP_CONNECT") {
            let t = connect(&id, cli).await.unwrap();
            println!("after connect: {t:?}");
            assert!(t.connected && !t.outdated);
            let t = disconnect(&id, cli).await.unwrap();
            println!("after disconnect: {t:?}");
            assert!(!t.connected);
        }
    }
}
