//! Subtitle export, local/cloud transcription, timestamp alignment, and shared library commands.
use crate::cmd::{ensure_transcript, resolve_or_ingest_episode};
use crate::exit::CliError;
use crate::output::Out;
use clap::{Args, Subcommand};
use serde_json::json;
use std::path::Path;
use std::sync::Arc;
use subcast_core::{ai, align, db, ops, settings, shared, subexport, CoreCtx, ProgressSink};

#[derive(Args)]
pub struct CaptionArgs {
    /// Episode reference or URL
    pub reference: String,
    /// Format: srt | vtt | ass | lrc | txt | md
    #[arg(long, default_value = "srt")]
    pub format: String,
    /// Content: bilingual | original | translation
    #[arg(long, default_value = "bilingual")]
    pub content: String,
    /// Map timestamps onto downloaded/cached audio clock when an alignment TimeMap exists
    #[arg(long)]
    pub align_clock: bool,
    /// Write to file (`-` or omitted prints to stdout)
    #[arg(short = 'o', long = "output")]
    pub output: Option<String>,
}

#[derive(Args)]
pub struct TranscribeArgs {
    /// Episode reference, URL, or local audio/video file path
    pub target: String,
    /// Local ASR model id (`qwen3-asr`, `large-v3-turbo`, `small`, `base`, or `file:...`)
    #[arg(long)]
    pub model: Option<String>,
    /// Cloud ASR service id (`openai`, `groq`, `deepseek`, `doubao`, `siliconflow`, `glm`, etc.)
    #[arg(long)]
    pub cloud: Option<String>,
    /// Language hint (`auto`, `en`, `zh`, ...)
    #[arg(long)]
    pub language: Option<String>,
    /// Skip checking community shared subtitles first
    #[arg(long)]
    pub no_shared: bool,
    /// Re-run transcription even if a transcript already exists in the DB
    #[arg(long)]
    pub force: bool,
    /// Wait and upload result to shared community library when finished
    #[arg(long)]
    pub share: bool,
}

#[derive(Args)]
pub struct AlignArgs {
    pub reference: String,
    #[arg(long)]
    pub force: bool,
    #[arg(long)]
    pub model: Option<String>,
}

#[derive(Args)]
pub struct SharedArgs {
    #[command(subcommand)]
    pub action: SharedAction,
}

#[derive(Subcommand)]
pub enum SharedAction {
    /// Check whether a shared transcript is available on the community server
    Check { reference: String },
    /// Fetch and save the community shared transcript for an episode
    Fetch { reference: String },
    /// Upload an episode's transcript + translation to the shared library
    Publish {
        reference: String,
        #[arg(long, default_value = "deepseek")]
        engine: String,
        #[arg(long, default_value = "")]
        model: String,
    },
}

pub async fn run_caption(ctx: &CoreCtx, args: CaptionArgs, out: Out) -> Result<(), CliError> {
    let format = subexport::SubFormat::parse(&args.format)
        .ok_or_else(|| CliError::usage(format!("不支持的字幕格式：{}", args.format)))?;
    let content = subexport::SubContent::parse(&args.content)
        .ok_or_else(|| CliError::usage(format!("不支持的字幕内容选项：{}", args.content)))?;
    let ep = resolve_or_ingest_episode(ctx, &args.reference).await?;
    let tr = ensure_transcript(ctx, &ep, true, true).await?;
    let zh = ai::load_episode_zh(ctx, &ep.id);
    let tmap: Option<align::TimeMap> = if args.align_clock {
        db::get_blob(&ctx.db.lock(), &ep.id, "tmap")?
            .and_then(|s| serde_json::from_str(&s).ok())
    } else {
        None
    };

    let mut cues = subexport::build_cues(&tr.segments, zh.as_deref(), content);
    let mut blocks = subexport::build_blocks(&tr.segments, zh.as_deref(), content);
    if args.align_clock {
        cues = subexport::on_clock(&cues, tmap.as_ref());
        blocks = subexport::on_clock(&blocks, tmap.as_ref());
    }
    let meta = subexport::ExportMeta {
        title: ep.title.clone(),
        show: ep.show_title.clone(),
        url: ep.link.clone().or_else(|| {
            ep.video_id.as_ref().map(|v| format!("https://www.youtube.com/watch?v={v}"))
        }),
    };
    let rendered = subexport::render(format, &cues, &blocks, &meta);

    if let Some(dest) = args.output.as_deref().filter(|p| *p != "-") {
        if let Some(parent) = Path::new(dest).parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(dest, &rendered)?;
        out.ok(&json!({ "path": dest, "format": format.ext(), "cues": cues.len() }), || {
            println!("Saved {} subtitles to {dest}", format.ext());
        });
    } else if out.is_structured() {
        out.ok(
            &json!({ "episodeId": ep.id, "format": format.ext(), "cues": cues.len(), "text": rendered }),
            || {},
        );
    } else {
        print!("{rendered}");
    }
    Ok(())
}

pub async fn run_transcribe(
    ctx: &CoreCtx,
    args: TranscribeArgs,
    sink: Arc<dyn ProgressSink>,
    out: Out,
) -> Result<(), CliError> {
    let eff = settings::load_effective(ctx)?;
    let model = args
        .model
        .as_deref()
        .or_else(|| eff["asrModel"].as_str())
        .unwrap_or("qwen3-asr");
    let language = args
        .language
        .as_deref()
        .or_else(|| eff["asrLanguage"].as_str())
        .unwrap_or("auto");
    let cloud = settings::resolve_cloud_asr(ctx, args.cloud.as_deref(), None)?;

    let local = Path::new(&args.target);
    if local.is_file() {
        let (ep, tr) = ops::transcribe_local_file(ctx, local, model, language, cloud, sink.as_ref()).await?;
        out.ok(&json!({ "episode": ep, "transcript": tr }), || {
            println!("Transcribed {} ({} segments) -> episode {}", ep.title, tr.segments.len(), ep.id);
        });
        return Ok(());
    }

    let ep = resolve_or_ingest_episode(ctx, &args.target).await?;
    if !args.force {
        if let Some(existing) = db::get_transcript(&ctx.db.lock(), &ep.id)? {
            out.ok(&existing, || {
                println!(
                    "Transcript already exists ({}, {} segments). Pass --force to re-transcribe.",
                    existing.source,
                    existing.segments.len()
                );
            });
            return Ok(());
        }
    }
    let tr = ops::transcribe(
        ctx,
        &ep.id,
        model,
        language,
        cloud,
        args.share,
        args.no_shared,
        sink,
    )
    .await?;
    out.ok(&tr, || {
        println!(
            "Transcribed {} [{}] ({} segments)",
            ep.title,
            tr.source,
            tr.segments.len()
        );
    });
    Ok(())
}

pub async fn run_align(
    ctx: &CoreCtx,
    args: AlignArgs,
    sink: &dyn ProgressSink,
    out: Out,
) -> Result<(), CliError> {
    let ep = resolve_or_ingest_episode(ctx, &args.reference).await?;
    let aligned = ops::align_audio(ctx, &ep.id, args.force, args.model.as_deref(), sink).await?;
    out.ok(&aligned, || {
        println!(
            "Align status for {}: {} (bytes: {})",
            ep.id, aligned.status, aligned.local_bytes
        );
    });
    Ok(())
}

pub async fn run_shared(ctx: &CoreCtx, args: SharedArgs, out: Out) -> Result<(), CliError> {
    match args.action {
        SharedAction::Check { reference } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let keys = shared::keys(&ep);
            let hits = ops::shared_exists(ctx, std::slice::from_ref(&ep.id)).await?;
            let available = !hits.is_empty();
            out.ok(&json!({ "episodeId": ep.id, "keys": keys, "available": available }), || {
                println!(
                    "Shared transcript for {}: {} (keys: {})",
                    ep.id,
                    if available { "available" } else { "not found" },
                    keys.join(", ")
                );
            });
        }
        SharedAction::Fetch { reference } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let tr = ops::shared_transcript(ctx, &ep.id).await?;
            match tr {
                Some(t) => out.ok(&t, || {
                    println!("Fetched shared transcript ({} segments) for {}", t.segments.len(), ep.id);
                }),
                None => return Err(CliError::not_found("共享库中暂无这一集的字幕")),
            }
        }
        SharedAction::Publish { reference, engine, model } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let res = ops::share_translation(ctx, &ep.id, &engine, &model).await?;
            out.ok(&json!({ "episodeId": ep.id, "result": res }), || {
                println!("Share result for {}: {res}", ep.id);
            });
        }
    }
    Ok(())
}
