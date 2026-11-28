//! Model Context Protocol (MCP) stdio JSON-RPC 2.0 server (`subcast mcp`).
use crate::exit::CliError;
use clap::Args;
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use subcast_core::{ai, db, highlights, ops, render, resolver, settings, subexport, vocab, CoreCtx, NoopSink};

#[derive(Args)]
pub struct McpArgs {}

fn tool_defs() -> Vec<Value> {
    vec![
        json!({
            "name": "list_shows",
            "description": "List subscribed podcast/YouTube shows in the SubCast library.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "list_episodes",
            "description": "List recent episodes or episodes in a specific show.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "show": { "type": "string", "description": "Optional show id, prefix, or title" },
                    "limit": { "type": "integer", "default": 20 }
                }
            }
        }),
        json!({
            "name": "search_episodes",
            "description": "Search inside a show's titles, notes, and transcripts.",
            "inputSchema": {
                "type": "object",
                "required": ["show", "query"],
                "properties": {
                    "show": { "type": "string" },
                    "query": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "preview_show",
            "description": "List a podcast feed's, Xiaoyuzhou show's or YouTube channel's episodes WITHOUT subscribing. The returned episode ids work as `reference` in every other tool.",
            "inputSchema": {
                "type": "object",
                "required": ["input"],
                "properties": { "input": { "type": "string", "description": "Feed, Xiaoyuzhou, Apple Podcasts or YouTube channel URL" }, "limit": { "type": "integer", "default": 20 } }
            }
        }),
        json!({
            "name": "find_show",
            "description": "Find shows by name (YouTube channels and podcasts), each with source, channel/author, episode count and newest episode (title, date) and its link. When more than one could be what the user means (look-alike names, or the same show as both a podcast and a YouTube channel), ASK THE USER to choose, showing those details, before going on; then pass the chosen feedUrl to preview_show or as a reference.",
            "inputSchema": {
                "type": "object",
                "required": ["name"],
                "properties": { "name": { "type": "string" } }
            }
        }),
        json!({
            "name": "search_podcasts",
            "description": "Search the Apple Podcasts directory by name. Returns feed URLs to use with preview_show.",
            "inputSchema": {
                "type": "object",
                "required": ["term"],
                "properties": { "term": { "type": "string" }, "country": { "type": "string", "default": "cn" } }
            }
        }),
        json!({
            "name": "add_subscription",
            "description": "Subscribe to a feed URL, Xiaoyuzhou URL, Apple Podcasts URL, or YouTube channel/video.",
            "inputSchema": {
                "type": "object",
                "required": ["input"],
                "properties": { "input": { "type": "string" } }
            }
        }),
        json!({
            "name": "get_episode",
            "description": "Get episode metadata, transcript stats, and translation count.",
            "inputSchema": {
                "type": "object",
                "required": ["reference"],
                "properties": { "reference": { "type": "string", "description": "Episode id or title, or a link (YouTube video, episode page, feed or channel; used without subscribing), or a local audio/video file. A show name alone, or `<show>:latest` / `<show>:@2`, means that show's newest or Nth newest episode, subscribed or not. If the reply says several shows match, ask the user which one (list the details given) and call again with the chosen link." } }
            }
        }),
        json!({
            "name": "get_transcript",
            "description": "Get episode subtitles formatted as txt, srt, vtt, md, lrc, or ass.",
            "inputSchema": {
                "type": "object",
                "required": ["reference"],
                "properties": {
                    "reference": { "type": "string", "description": "Episode id or title, or a link (YouTube video, episode page, feed or channel; used without subscribing), or a local audio/video file. A show name alone, or `<show>:latest` / `<show>:@2`, means that show's newest or Nth newest episode, subscribed or not. If the reply says several shows match, ask the user which one (list the details given) and call again with the chosen link." },
                    "format": { "type": "string", "default": "txt" },
                    "content": { "type": "string", "default": "bilingual" }
                }
            }
        }),
        json!({
            "name": "transcribe_episode",
            "description": "Fetch shared/official subtitles or transcribe an episode locally/in cloud.",
            "inputSchema": {
                "type": "object",
                "required": ["reference"],
                "properties": {
                    "reference": { "type": "string", "description": "Episode id or title, or a link (YouTube video, episode page, feed or channel; used without subscribing), or a local audio/video file. A show name alone, or `<show>:latest` / `<show>:@2`, means that show's newest or Nth newest episode, subscribed or not. If the reply says several shows match, ask the user which one (list the details given) and call again with the chosen link." },
                    "model": { "type": "string" },
                    "language": { "type": "string" },
                    "noShared": { "type": "boolean", "default": false }
                }
            }
        }),
        json!({
            "name": "translate_episode",
            "description": "Translate episode subtitles into Chinese using configured LLM or Google Translate.",
            "inputSchema": {
                "type": "object",
                "required": ["reference"],
                "properties": {
                    "reference": { "type": "string", "description": "Episode id or title, or a link (YouTube video, episode page, feed or channel; used without subscribing), or a local audio/video file. A show name alone, or `<show>:latest` / `<show>:@2`, means that show's newest or Nth newest episode, subscribed or not. If the reply says several shows match, ask the user which one (list the details given) and call again with the chosen link." },
                    "mode": { "type": "string", "default": "auto" },
                    "target": { "type": "string", "default": "zh-CN" }
                }
            }
        }),
        json!({
            "name": "generate_guide",
            "description": "Generate or retrieve the Markdown AI study guide for an episode.",
            "inputSchema": {
                "type": "object",
                "required": ["reference"],
                "properties": {
                    "reference": { "type": "string", "description": "Episode id or title, or a link (YouTube video, episode page, feed or channel; used without subscribing), or a local audio/video file. A show name alone, or `<show>:latest` / `<show>:@2`, means that show's newest or Nth newest episode, subscribed or not. If the reply says several shows match, ask the user which one (list the details given) and call again with the chosen link." },
                    "regenerate": { "type": "boolean", "default": false }
                }
            }
        }),
        json!({
            "name": "ask_episode",
            "description": "Ask a question grounded in an episode's bilingual transcript.",
            "inputSchema": {
                "type": "object",
                "required": ["reference", "question"],
                "properties": {
                    "reference": { "type": "string", "description": "Episode id or title, or a link (YouTube video, episode page, feed or channel; used without subscribing), or a local audio/video file. A show name alone, or `<show>:latest` / `<show>:@2`, means that show's newest or Nth newest episode, subscribed or not. If the reply says several shows match, ask the user which one (list the details given) and call again with the chosen link." },
                    "question": { "type": "string" },
                    "reset": { "type": "boolean", "default": false }
                }
            }
        }),
        json!({
            "name": "extract_highlights",
            "description": "Extract viral/insightful highlight clips with hook copy from an episode.",
            "inputSchema": {
                "type": "object",
                "required": ["reference"],
                "properties": {
                    "reference": { "type": "string", "description": "Episode id or title, or a link (YouTube video, episode page, feed or channel; used without subscribing), or a local audio/video file. A show name alone, or `<show>:latest` / `<show>:@2`, means that show's newest or Nth newest episode, subscribed or not. If the reply says several shows match, ask the user which one (list the details given) and call again with the chosen link." },
                    "intent": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "clip_video",
            "description": "Export a subtitled video clip from a YouTube episode.",
            "inputSchema": {
                "type": "object",
                "required": ["reference", "start", "end"],
                "properties": {
                    "reference": { "type": "string", "description": "Episode id or title, or a link (YouTube video, episode page, feed or channel; used without subscribing), or a local audio/video file. A show name alone, or `<show>:latest` / `<show>:@2`, means that show's newest or Nth newest episode, subscribed or not. If the reply says several shows match, ask the user which one (list the details given) and call again with the chosen link." },
                    "start": { "type": "string" },
                    "end": { "type": "string" },
                    "preset": { "type": "string", "default": "vertical" },
                    "outputPath": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "render_poster",
            "description": "Render a shareable quote poster PNG to disk.",
            "inputSchema": {
                "type": "object",
                "required": ["outputPath"],
                "properties": {
                    "reference": { "type": "string", "description": "Episode id or title, or a link (YouTube video, episode page, feed or channel; used without subscribing), or a local audio/video file. A show name alone, or `<show>:latest` / `<show>:@2`, means that show's newest or Nth newest episode, subscribed or not. If the reply says several shows match, ask the user which one (list the details given) and call again with the chosen link." },
                    "orig": { "type": "string" },
                    "zh": { "type": "string" },
                    "theme": { "type": "string", "default": "paper" },
                    "outputPath": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "vocab_list",
            "description": "List saved words in the SubCast FSRS vocabulary book.",
            "inputSchema": {
                "type": "object",
                "properties": { "dueOnly": { "type": "boolean", "default": false } }
            }
        }),
        json!({
            "name": "vocab_lookup",
            "description": "Look up an English word in offline ECDICT.",
            "inputSchema": {
                "type": "object",
                "required": ["word"],
                "properties": { "word": { "type": "string" } }
            }
        }),
        json!({
            "name": "vocab_add",
            "description": "Add a word to the vocabulary book.",
            "inputSchema": {
                "type": "object",
                "required": ["word"],
                "properties": {
                    "word": { "type": "string" },
                    "meaning": { "type": "string" },
                    "sentence": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "vocab_review_due",
            "description": "Get due FSRS review words, or submit a review rating (again/hard/good/easy).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string" },
                    "rating": { "type": "string" }
                }
            }
        }),
        json!({
            "name": "doctor",
            "description": "Inspect SubCast environment health, paths, keys, models, and tools.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}

/// An episode by id, title or link. Links are used without subscribing (see resolve_or_ingest_episode).
async fn episode_of(ctx: &CoreCtx, reference: &str) -> anyhow::Result<subcast_core::model::Episode> {
    crate::cmd::resolve_or_ingest_episode(ctx, reference).await.map_err(|e| anyhow::anyhow!(e.message))
}

async fn call_tool(ctx: &CoreCtx, name: &str, args: &Value) -> anyhow::Result<Value> {
    let sink = NoopSink;
    match name {
        "list_shows" => {
            let shows = db::list_shows(&ctx.db.lock())?;
            Ok(serde_json::to_value(shows)?)
        }
        "list_episodes" => {
            let limit = args["limit"].as_i64().unwrap_or(20);
            let conn = ctx.db.lock();
            let eps = if let Some(s) = args["show"].as_str().filter(|s| !s.is_empty()) {
                let sh = resolver::resolve_show(&conn, s)?;
                db::list_episodes(&conn, Some(&sh.id), limit)?
            } else {
                db::recent_episodes(&conn, limit)?
            };
            Ok(serde_json::to_value(eps)?)
        }
        "search_episodes" => {
            let show = args["show"].as_str().unwrap_or("");
            let query = args["query"].as_str().unwrap_or("");
            let sh = resolver::resolve_show(&ctx.db.lock(), show)?;
            let hits = ops::search_show(ctx, &sh.id, query).await?;
            Ok(serde_json::to_value(hits)?)
        }
        "preview_show" => {
            let input = args["input"].as_str().unwrap_or("");
            let limit = args["limit"].as_u64().unwrap_or(20) as usize;
            let prev = ops::preview_show(ctx, input).await?;
            let eps: Vec<Value> = prev.episodes.iter().take(limit).map(|e| json!({ "id": e.id, "title": e.title, "published": e.published, "duration": e.duration, "link": e.link })).collect();
            Ok(json!({ "show": { "title": prev.show.title, "author": prev.show.author, "kind": prev.show.kind }, "episodes": eps }))
        }
        "find_show" => {
            let name = args["name"].as_str().unwrap_or("");
            Ok(json!({ "candidates": resolver::show_candidates(ctx, name, 6).await? }))
        }
        "search_podcasts" => {
            let term = args["term"].as_str().unwrap_or("");
            let country = args["country"].as_str().unwrap_or("cn");
            Ok(serde_json::to_value(subcast_core::feeds::search_podcasts(term, country).await?)?)
        }
        "add_subscription" => {
            let input = args["input"].as_str().unwrap_or("");
            let added = ops::add_subscription(ctx, input).await?;
            Ok(serde_json::to_value(added)?)
        }
        "get_episode" => {
            let r = args["reference"].as_str().unwrap_or("");
            let ep = episode_of(ctx, r).await?;
            let tr = db::get_transcript(&ctx.db.lock(), &ep.id)?;
            Ok(json!({ "episode": ep, "segments": tr.map(|t| t.segments.len()).unwrap_or(0) }))
        }
        "get_transcript" => {
            let r = args["reference"].as_str().unwrap_or("");
            let fmt = subexport::SubFormat::parse(args["format"].as_str().unwrap_or("txt"))
                .unwrap_or(subexport::SubFormat::Txt);
            let content = subexport::SubContent::parse(args["content"].as_str().unwrap_or("bilingual"))
                .unwrap_or(subexport::SubContent::Bilingual);
            let ep = episode_of(ctx, r).await?;
            let tr = db::get_transcript(&ctx.db.lock(), &ep.id)?
                .ok_or_else(|| anyhow::anyhow!("暂无字幕"))?;
            let zh = ai::load_episode_zh(ctx, &ep.id);
            let cues = subexport::build_cues(&tr.segments, zh.as_deref(), content);
            let blocks = subexport::build_blocks(&tr.segments, zh.as_deref(), content);
            let meta = subexport::ExportMeta { title: ep.title.clone(), show: ep.show_title.clone(), url: ep.link.clone() };
            Ok(json!({ "text": subexport::render(fmt, &cues, &blocks, &meta) }))
        }
        "transcribe_episode" => {
            let r = args["reference"].as_str().unwrap_or("");
            let ep = episode_of(ctx, r).await?;
            let eff = settings::load_effective(ctx)?;
            let model = args["model"].as_str().or_else(|| eff["asrModel"].as_str()).unwrap_or("qwen3-asr");
            let lang = args["language"].as_str().or_else(|| eff["asrLanguage"].as_str()).unwrap_or("auto");
            let no_shared = args["noShared"].as_bool().unwrap_or(false);
            let tr = ops::transcribe(ctx, &ep.id, model, lang, None, false, no_shared, std::sync::Arc::new(NoopSink)).await?;
            Ok(serde_json::to_value(tr)?)
        }
        "translate_episode" => {
            let r = args["reference"].as_str().unwrap_or("");
            let ep = episode_of(ctx, r).await?;
            let rep = ai::translate_episode(
                ctx,
                &ep.id,
                args["mode"].as_str().unwrap_or("auto"),
                None,
                None,
                None,
                args["target"].as_str().unwrap_or("zh-CN"),
                false,
                true,
                &sink,
            )
            .await?;
            Ok(serde_json::to_value(rep)?)
        }
        "generate_guide" => {
            let r = args["reference"].as_str().unwrap_or("");
            let ep = episode_of(ctx, r).await?;
            let (md, src) = ai::generate_guide(
                ctx,
                &ep.id,
                None,
                None,
                None,
                args["regenerate"].as_bool().unwrap_or(false),
                true,
                &sink,
            )
            .await?;
            Ok(json!({ "source": src, "guide": md }))
        }
        "ask_episode" => {
            let r = args["reference"].as_str().unwrap_or("");
            let q = args["question"].as_str().unwrap_or("");
            let ep = episode_of(ctx, r).await?;
            let (ans, turns) = ai::ask_episode(
                ctx,
                &ep.id,
                q,
                None,
                None,
                None,
                None,
                args["reset"].as_bool().unwrap_or(false),
                true,
                &sink,
            )
            .await?;
            Ok(json!({ "answer": ans, "turns": turns.len() }))
        }
        "extract_highlights" => {
            let r = args["reference"].as_str().unwrap_or("");
            let ep = episode_of(ctx, r).await?;
            let b = highlights::extract_highlights(ctx, &ep.id, args["intent"].as_str(), None, None, &sink).await?;
            Ok(serde_json::to_value(b)?)
        }
        "clip_video" => {
            let r = args["reference"].as_str().unwrap_or("");
            let ep = episode_of(ctx, r).await?;
            let s = resolver::parse_timestamp(args["start"].as_str().unwrap_or("0"))?;
            let e = resolver::parse_timestamp(args["end"].as_str().unwrap_or("30"))?;
            let res = ops::clip_video_full(
                ctx,
                ops::ClipVideoOptions {
                    episode_id: ep.id,
                    ranges: vec![(s, e)],
                    preset: Some(args["preset"].as_str().unwrap_or("vertical").into()),
                    output_path: args["outputPath"].as_str().map(String::from),
                    ..Default::default()
                },
                &sink,
            )
            .await?;
            Ok(serde_json::to_value(res)?)
        }
        "render_poster" => {
            let out_path = args["outputPath"].as_str().unwrap_or("poster.png");
            let mut show_title = String::new();
            let mut ep_title = String::new();
            if let Some(r) = args["reference"].as_str().filter(|s| !s.is_empty()) {
                if let Ok(ep) = episode_of(ctx, r).await {
                    ep_title = ep.title;
                    show_title = ep.show_title.unwrap_or_default();
                }
            }
            let png = render::render_poster_png(&render::PosterOpts {
                theme: args["theme"].as_str().unwrap_or("paper").into(),
                show_title,
                episode_title: ep_title,
                quote_orig: args["orig"].as_str().unwrap_or("").into(),
                quote_zh: args["zh"].as_str().unwrap_or("").into(),
                ..Default::default()
            })?;
            std::fs::write(out_path, &png)?;
            Ok(json!({ "path": out_path, "bytes": png.len() }))
        }
        "vocab_list" => {
            let book = vocab::load_book(ctx)?;
            let now = subcast_core::now_ms();
            let due_only = args["dueOnly"].as_bool().unwrap_or(false);
            let words: Vec<_> = book
                .words
                .into_values()
                .filter(|w| !due_only || vocab::is_due(w, now))
                .collect();
            Ok(serde_json::to_value(words)?)
        }
        "vocab_lookup" => {
            let w = args["word"].as_str().unwrap_or("");
            let path = vocab::Dict::find_path(&ctx.data, None)
                .ok_or_else(|| anyhow::anyhow!("找不到词典"))?;
            let entry = vocab::Dict::load(&path)?
                .lookup(w)
                .ok_or_else(|| anyhow::anyhow!("未收录"))?;
            Ok(serde_json::to_value(entry)?)
        }
        "vocab_add" => {
            let word = args["word"].as_str().unwrap_or("");
            let now = subcast_core::now_ms();
            let saved = vocab::mutate_book(ctx, |book| {
                let w = vocab::Word {
                    id: word.trim().to_lowercase(),
                    word: word.trim().to_string(),
                    ipa: String::new(),
                    meaning: args["meaning"].as_str().unwrap_or("").to_string(),
                    ctx: None,
                    tag: String::new(),
                    sentence: args["sentence"].as_str().unwrap_or("").to_string(),
                    sentence_zh: None,
                    episode_id: String::new(),
                    episode_title: String::new(),
                    show_title: None,
                    start: 0.0,
                    end: 0.0,
                    added: now,
                    card: vocab::create_empty_card(now),
                };
                book.words.insert(w.id.clone(), w.clone());
                Ok(w)
            })?;
            Ok(serde_json::to_value(saved)?)
        }
        "vocab_review_due" => {
            let now = subcast_core::now_ms();
            if let (Some(id), Some(r)) = (args["id"].as_str(), args["rating"].as_str()) {
                let grade = vocab::parse_rating(r).ok_or_else(|| anyhow::anyhow!("invalid rating"))?;
                Ok(serde_json::to_value(vocab::rate_word(ctx, id, grade, now)?)?)
            } else {
                let book = vocab::load_book(ctx)?;
                Ok(serde_json::to_value(vocab::review_queue(&book, 20, now))?)
            }
        }
        "doctor" => {
            let conn = ctx.db.lock();
            let shows = db::list_shows(&conn)?.len();
            let eps: i64 = conn.query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))?;
            Ok(json!({ "ok": true, "dataDir": ctx.data, "shows": shows, "episodes": eps }))
        }
        _ => Err(anyhow::anyhow!("Unknown MCP tool: {name}")),
    }
}

pub async fn run_mcp(ctx: &CoreCtx) -> Result<(), CliError> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(err) => {
                let resp = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": { "code": -32700, "message": format!("Parse error: {err}") }
                });
                writeln!(stdout, "{resp}")?;
                stdout.flush()?;
                continue;
            }
        };
        let method = req["method"].as_str().unwrap_or("");
        let id = req.get("id").cloned();
        if id.is_none() || method == "notifications/initialized" {
            continue;
        }
        let id = id.unwrap();
        let resp = match method {
            "initialize" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "tools": {}, "resources": {} },
                    "serverInfo": { "name": "subcast", "version": env!("CARGO_PKG_VERSION") }
                }
            }),
            "ping" => json!({ "jsonrpc": "2.0", "id": id, "result": {} }),
            "tools/list" => json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": tool_defs() } }),
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let arguments = &req["params"]["arguments"];
                match call_tool(ctx, name, arguments).await {
                    Ok(val) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": serde_json::to_string_pretty(&val).unwrap_or_default() }],
                            "isError": false
                        }
                    }),
                    Err(err) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": {
                            "content": [{ "type": "text", "text": format!("{err:#}") }],
                            "isError": true
                        }
                    }),
                }
            }
            "resources/list" => json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "resources": [
                        { "uri": "subcast://shows", "name": "Subscriptions", "mimeType": "application/json" },
                        { "uri": "subcast://vocab", "name": "Vocabulary Book", "mimeType": "application/json" },
                        { "uri": "subcast://stats", "name": "Listening Stats", "mimeType": "application/json" },
                    ]
                }
            }),
            "resources/read" => {
                let uri = req["params"]["uri"].as_str().unwrap_or("");
                let content = match uri {
                    "subcast://shows" => serde_json::to_string_pretty(&db::list_shows(&ctx.db.lock())?).ok(),
                    "subcast://vocab" => serde_json::to_string_pretty(&vocab::load_book(ctx)?).ok(),
                    "subcast://stats" => serde_json::to_string_pretty(&db::listens_since(&ctx.db.lock(), "1970-01-01")?).ok(),
                    _ => None,
                };
                match content {
                    Some(text) => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "result": { "contents": [{ "uri": uri, "mimeType": "application/json", "text": text }] }
                    }),
                    None => json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32602, "message": format!("Unknown resource: {uri}") }
                    }),
                }
            }
            other => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": format!("Method not found: {other}") }
            }),
        };
        writeln!(stdout, "{resp}")?;
        stdout.flush()?;
    }
    Ok(())
}
