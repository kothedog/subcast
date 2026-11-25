//! `subcast do "<一句话>"`: a plain-language request ("Lex Fridman 最新一期总结下", "硅谷101 上一期的高光") turned into
//! one of the regular commands by the AI set in SubCast, then run. Shows don't need to be subscribed.
use crate::cmd::{dispatch, Commands};
use crate::exit::CliError;
use crate::output::Out;
use clap::{Args, Parser};
use serde_json::{json, Value};
use std::sync::Arc;
use subcast_core::{llm, ops as core_ops, settings, CoreCtx, NoopSink, ProgressSink};

#[derive(Args)]
pub struct DoArgs {
    /// What you want, in your own words (中文 or English)
    pub request: Vec<String>,
    /// AI used to understand the request (and for the work, when it needs AI)
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Only print the command it would run
    #[arg(long)]
    pub dry_run: bool,
}

/// The regular commands, parsed from the planned argument list.
#[derive(Parser)]
#[command(name = "subcast", no_binary_name = true)]
struct Planned {
    #[command(subcommand)]
    cmd: Commands,
}

const SYSTEM: &str = r#"You turn a request about podcasts or YouTube videos into one SubCast action. Reply with JSON only:
{"action": "guide|highlights|ask|caption|transcribe|translate|clip|list", "show": "", "episode": "", "question": "", "start": "", "end": "", "highlight": null}
- action: guide = summary / 总结 / 导读; highlights = 高光 / 金句 / best moments; ask = a question about the content; caption = subtitles / 字幕 / transcript text; transcribe = 转写 (make subtitles from audio); translate = 翻译字幕; clip = 剪视频 / 剪辑 a part; list = 列出单集 / what episodes.
- show: the podcast or channel name as the user wrote it (e.g. "Lex Fridman", "硅谷101"); empty when the user gave a link or an exact episode title only.
- episode: "latest" for 最新/latest/this week's, "@2" for 上一期/the one before, "@N" for the Nth newest; a link if the user gave one; otherwise words from the episode title. Default "latest" when a show is named.
- question: for ask, the question in the user's words.
- start/end: for clip, times like "12:30"; highlight: for clip, a 0-based highlight number if the user said 第N个高光.
"#;

pub async fn run_do(ctx: &Arc<CoreCtx>, args: DoArgs, sink: Arc<dyn ProgressSink>, out: Out, no_input: bool) -> Result<(), CliError> {
    let request = args.request.join(" ");
    if request.trim().is_empty() {
        return Err(CliError::usage("说说你想做什么，例如：subcast do \"Lex Fridman 最新一期总结下\""));
    }
    let resolved = settings::resolve_chat_provider(ctx, args.provider.as_deref(), args.model.as_deref())?;
    let messages = [llm::Message { role: "user".into(), content: request.clone() }];
    let reply = core_ops::llm_chat(ctx, "do-plan", &resolved.provider, SYSTEM, &messages, Some(600), Some(true), Some(true), &NoopSink).await?;
    let plan = parse_plan(&reply).ok_or_else(|| CliError::from(anyhow::anyhow!("没看懂这个请求，换个说法试试：{}", reply.chars().take(120).collect::<String>())))?;
    let argv = plan_to_argv(&plan, args.provider.as_deref(), args.model.as_deref())
        .ok_or_else(|| CliError::from(anyhow::anyhow!("没看懂要处理哪个节目或单集，请说出节目名或给个链接")))?;

    if args.dry_run {
        out.ok(&json!({ "plan": plan, "argv": argv }), || println!("subcast {}", shell_join(&argv)));
        return Ok(());
    }
    if !out.is_structured() {
        eprintln!("→ subcast {}", shell_join(&argv));
    }
    let planned = Planned::try_parse_from(&argv).map_err(|e| CliError::from(anyhow::anyhow!("生成的命令无效：{e}")))?;
    Box::pin(dispatch(planned.cmd, ctx, sink, out, no_input)).await
}

fn parse_plan(reply: &str) -> Option<Value> {
    let t = reply.trim().trim_start_matches("```json").trim_start_matches("```").trim_end_matches("```").trim();
    let start = t.find('{')?;
    let end = t.rfind('}')?;
    serde_json::from_str::<Value>(&t[start..=end]).ok().filter(|v| v["action"].is_string())
}

fn plan_to_argv(plan: &Value, provider: Option<&str>, model: Option<&str>) -> Option<Vec<String>> {
    let s = |k: &str| plan[k].as_str().map(str::trim).unwrap_or("").to_string();
    let (action, show, episode) = (s("action"), s("show"), s("episode"));
    let is_link = episode.starts_with("http") || std::path::Path::new(&episode).exists();
    let reference = if is_link || show.is_empty() {
        episode.clone()
    } else {
        format!("{show}:{}", if episode.is_empty() { "latest" } else { &episode })
    };
    if reference.is_empty() && action != "list" {
        return None;
    }
    let ai = |mut v: Vec<String>| {
        if let Some(p) = provider { v.extend(["--provider".into(), p.into()]); }
        if let Some(m) = model { v.extend(["--model".into(), m.into()]); }
        v
    };
    let v: Vec<String> = match action.as_str() {
        "guide" => ai(vec!["guide".into(), reference]),
        "highlights" => ai(vec!["highlights".into(), "extract".into(), reference]),
        "ask" => {
            let q = s("question");
            ai(vec!["ask".into(), reference, if q.is_empty() { "总结一下这期的要点".into() } else { q }])
        }
        "caption" => vec!["caption".into(), reference],
        "transcribe" => vec!["transcribe".into(), reference],
        "translate" => vec!["translate".into(), reference],
        "clip" => {
            let mut v = vec!["clip".into(), "video".into(), reference];
            let (start, end) = (s("start"), s("end"));
            if !start.is_empty() && !end.is_empty() {
                v.extend(["--start".into(), start, "--end".into(), end]);
            } else {
                v.extend(["--highlight".into(), plan["highlight"].as_u64().unwrap_or(0).to_string()]);
            }
            v
        }
        "list" => {
            if show.is_empty() { return None; }
            vec!["ep".into(), "list".into(), show]
        }
        _ => return None,
    };
    Some(v)
}

fn shell_join(argv: &[String]) -> String {
    argv.iter().map(|a| if a.contains(' ') || a.is_empty() { format!("\"{a}\"") } else { a.clone() }).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plans_become_commands() {
        let p = json!({ "action": "guide", "show": "Lex Fridman", "episode": "latest" });
        assert_eq!(plan_to_argv(&p, None, None).unwrap(), ["guide", "Lex Fridman:latest"]);
        let p = json!({ "action": "highlights", "show": "", "episode": "https://youtu.be/x" });
        assert_eq!(plan_to_argv(&p, Some("cli-antigravity"), None).unwrap(), ["highlights", "extract", "https://youtu.be/x", "--provider", "cli-antigravity"]);
        let p = json!({ "action": "clip", "show": "硅谷101", "episode": "@2", "start": "1:00", "end": "1:30" });
        assert_eq!(plan_to_argv(&p, None, None).unwrap(), ["clip", "video", "硅谷101:@2", "--start", "1:00", "--end", "1:30"]);
        assert!(parse_plan("```json\n{\"action\":\"ask\",\"show\":\"a\"}\n```").is_some());
        let argv = plan_to_argv(&json!({ "action": "guide", "show": "Lex Fridman", "episode": "latest" }), None, None).unwrap();
        assert!(Planned::try_parse_from(&argv).is_ok());
    }
}
