//! AI translation, guide, deep-read, Q&A, and highlight extraction/copywriting commands.
use crate::cmd::{ensure_transcript, read_arg_or_stdin, resolve_or_ingest_episode};
use crate::exit::CliError;
use crate::output::{format_duration, Out};
use clap::{Args, Subcommand};
use serde_json::json;
use subcast_core::{ai, db, highlights, ops, resolver, CoreCtx, ProgressSink};

#[derive(Args)]
pub struct TranslateArgs {
    pub reference: String,
    /// Translation engine: auto | llm | google
    #[arg(long, default_value = "auto")]
    pub mode: String,
    /// LLM quality: fast | deep
    #[arg(long)]
    pub quality: Option<String>,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long, default_value = "zh-CN")]
    pub target: String,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub no_context: bool,
}

#[derive(Args)]
pub struct GuideArgs {
    pub reference: String,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub lang: Option<String>,
    #[arg(long)]
    pub regenerate: bool,
    #[arg(long)]
    pub no_shared: bool,
    #[arg(short = 'o', long = "output")]
    pub output: Option<String>,
}

#[derive(Args)]
pub struct DeepArgs {
    pub reference: String,
    #[arg(long)]
    pub notes: bool,
    #[arg(short = 'o', long = "output")]
    pub output: Option<String>,
}

#[derive(Args)]
pub struct AskArgs {
    pub reference: String,
    /// Question text (or `-` to read from stdin)
    pub question: String,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long)]
    pub system: Option<String>,
    #[arg(long)]
    pub lang: Option<String>,
    #[arg(long)]
    pub reset: bool,
    #[arg(long)]
    pub no_save: bool,
}

#[derive(Args)]
pub struct HighlightsArgs {
    #[command(subcommand)]
    pub action: HighlightsAction,
}

#[derive(Subcommand)]
pub enum HighlightsAction {
    /// List previously extracted highlights for an episode
    List { reference: String },
    /// Run AI highlight extraction on an episode transcript
    Extract {
        reference: String,
        #[arg(long)]
        intent: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Polish hook/title/social copy for a highlight index (0-based) or time range
    Polish {
        reference: String,
        #[arg(long, default_value_t = 0)]
        index: usize,
        #[arg(long)]
        start: Option<String>,
        #[arg(long)]
        end: Option<String>,
        #[arg(long)]
        previous: Option<String>,
        #[arg(long)]
        provider: Option<String>,
        #[arg(long)]
        model: Option<String>,
    },
    /// Build platform social copy from an existing highlight
    Post {
        reference: String,
        /// Platform: xhs | wechat | douyin | bilibili | twitter
        #[arg(long, default_value = "xhs")]
        platform: String,
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
}

pub async fn run_translate(
    ctx: &CoreCtx,
    args: TranslateArgs,
    sink: &dyn ProgressSink,
    out: Out,
) -> Result<(), CliError> {
    let ep = resolve_or_ingest_episode(ctx, &args.reference).await?;
    ensure_transcript(ctx, &ep, true, true).await?;
    let report = ai::translate_episode(
        ctx,
        &ep.id,
        &args.mode,
        args.quality.as_deref(),
        args.provider.as_deref(),
        args.model.as_deref(),
        &args.target,
        args.force,
        !args.no_context,
        sink,
    )
    .await?;
    out.ok(&report, || {
        println!(
            "Translated {}/{} segments for {} using {} (failed {})",
            report.translated, report.total, ep.title, report.engine, report.failed
        );
    });
    Ok(())
}

pub async fn run_guide(
    ctx: &CoreCtx,
    args: GuideArgs,
    sink: &dyn ProgressSink,
    out: Out,
) -> Result<(), CliError> {
    let ep = resolve_or_ingest_episode(ctx, &args.reference).await?;
    ensure_transcript(ctx, &ep, true, true).await?;
    let (markdown, source) = ai::generate_guide(
        ctx,
        &ep.id,
        args.provider.as_deref(),
        args.model.as_deref(),
        args.lang.as_deref(),
        args.regenerate,
        !args.no_shared,
        sink,
    )
    .await?;

    if let Some(dest) = args.output.as_deref() {
        std::fs::write(dest, &markdown)?;
        out.ok(&json!({ "episodeId": ep.id, "source": source, "path": dest }), || {
            println!("Saved guide ({source}) to {dest}");
        });
    } else if out.is_structured() {
        out.ok(&json!({ "episodeId": ep.id, "source": source, "guide": markdown }), || {});
    } else {
        println!("{markdown}");
    }
    Ok(())
}

pub async fn run_deep(ctx: &CoreCtx, args: DeepArgs, out: Out) -> Result<(), CliError> {
    let ep = resolve_or_ingest_episode(ctx, &args.reference).await?;
    let text = if args.notes {
        ops::full_notes(ctx, &ep.id)
            .await?
            .or(ep.notes.clone())
            .ok_or_else(|| CliError::not_found("这一集没有完整 Shownotes"))?
    } else {
        let cached = db::get_blob(&ctx.db.lock(), &ep.id, "deep-read")?;
        if let Some(h) = cached.filter(|s| !s.trim().is_empty()) {
            h
        } else {
            let d = ops::qiaomu_detail(ctx, &ep.id).await?;
            d.deep_read
                .ok_or_else(|| CliError::not_found("这一集没有精读/改写文章"))?
        }
    };

    if let Some(dest) = args.output.as_deref() {
        std::fs::write(dest, &text)?;
        out.ok(&json!({ "episodeId": ep.id, "path": dest }), || println!("Saved to {dest}"));
    } else if out.is_structured() {
        out.ok(&json!({ "episodeId": ep.id, "content": text }), || {});
    } else {
        println!("{text}");
    }
    Ok(())
}

pub async fn run_ask(
    ctx: &CoreCtx,
    args: AskArgs,
    sink: &dyn ProgressSink,
    out: Out,
) -> Result<(), CliError> {
    let ep = resolve_or_ingest_episode(ctx, &args.reference).await?;
    ensure_transcript(ctx, &ep, true, true).await?;
    let q = read_arg_or_stdin(&args.question)?;
    let (answer, history) = ai::ask_episode(
        ctx,
        &ep.id,
        &q,
        args.provider.as_deref(),
        args.model.as_deref(),
        args.system.as_deref(),
        args.lang.as_deref(),
        args.reset,
        !args.no_save,
        sink,
    )
    .await?;
    out.ok(&json!({ "episodeId": ep.id, "answer": answer, "turns": history.len() }), || {
        println!("{answer}");
    });
    Ok(())
}

pub async fn run_highlights(
    ctx: &CoreCtx,
    args: HighlightsArgs,
    sink: &dyn ProgressSink,
    out: Out,
) -> Result<(), CliError> {
    match args.action {
        HighlightsAction::List { reference } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let raw = db::get_blob(&ctx.db.lock(), &ep.id, "highlights")?;
            let bundle: Option<highlights::HighlightBundle> =
                raw.and_then(|s| serde_json::from_str(&s).ok());
            match bundle {
                Some(b) => out.ok(&b, || {
                    for (i, c) in b.clips.iter().enumerate() {
                        println!(
                            "[{i}] {:>5}–{:<5} (score {})  {}",
                            format_duration(c.start),
                            format_duration(c.end),
                            c.score,
                            c.title
                        );
                        if let Some(hook) = c.cover_hook.as_deref().filter(|s| !s.is_empty()) {
                            println!("    {hook}");
                        }
                    }
                }),
                None => out.ok(&json!({ "clips": [] }), || {
                    println!("No highlights yet. Run `subcast highlights extract {}`.", ep.id);
                }),
            }
        }
        HighlightsAction::Extract { reference, intent, provider, model } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            ensure_transcript(ctx, &ep, true, true).await?;
            let b = highlights::extract_highlights(
                ctx,
                &ep.id,
                intent.as_deref(),
                provider.as_deref(),
                model.as_deref(),
                sink,
            )
            .await?;
            out.ok(&b, || {
                println!("Extracted {} highlight(s):", b.clips.len());
                for (i, c) in b.clips.iter().enumerate() {
                    println!(
                        "[{i}] {}–{} ({}): {}",
                        format_duration(c.start),
                        format_duration(c.end),
                        c.score,
                        c.title
                    );
                }
            });
        }
        HighlightsAction::Polish { reference, index, start, end, previous, provider, model } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let tr = ensure_transcript(ctx, &ep, true, true).await?;
            let (s_idx, e_idx) = if let Some(st) = start {
                let ss = resolver::parse_timestamp(&st)?;
                let es = end.map(|x| resolver::parse_timestamp(&x)).transpose()?;
                highlights::range_from_clock(&tr.segments, ss, es)
            } else {
                let raw = db::get_blob(&ctx.db.lock(), &ep.id, "highlights")?
                    .ok_or_else(|| CliError::not_found("没有高光记录，请先 extract 或传入 --start/--end"))?;
                let b: highlights::HighlightBundle = serde_json::from_str(&raw)
                    .map_err(|e| CliError::new(1, "parse", e.to_string()))?;
                let c = b.clips.get(index).ok_or_else(|| CliError::not_found("高光序号超出范围"))?;
                (c.start_idx, c.end_idx)
            };
            let copy = highlights::polish_clip_copy(
                ctx,
                &ep.id,
                s_idx,
                e_idx,
                previous.as_deref(),
                provider.as_deref(),
                model.as_deref(),
                sink,
            )
            .await?;
            out.ok(&copy, || {
                println!("Title      : {}", copy.title);
                println!("Cover Hook : {}", copy.cover_hook);
                println!("Cover Sub  : {}", copy.cover_sub);
                println!("\n--- Share Copy ---\n{}", copy.share_copy);
                println!("\n--- Article Copy ---\n{}", copy.article_copy);
            });
        }
        HighlightsAction::Post { reference, platform, index } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let tr = ensure_transcript(ctx, &ep, true, true).await?;
            let zh = ai::load_episode_zh(ctx, &ep.id);
            let raw = db::get_blob(&ctx.db.lock(), &ep.id, "highlights")?
                .ok_or_else(|| CliError::not_found("没有高光记录，请先运行 highlights extract"))?;
            let b: highlights::HighlightBundle = serde_json::from_str(&raw)
                .map_err(|e| CliError::new(1, "parse", e.to_string()))?;
            let c = b.clips.get(index).ok_or_else(|| CliError::not_found("高光序号超出范围"))?;
            let text = highlights::build_platform_post(&platform, &ep, c, &tr.segments, zh.as_deref());
            out.ok(&json!({ "platform": platform, "text": text }), || println!("{text}"));
        }
    }
    Ok(())
}
