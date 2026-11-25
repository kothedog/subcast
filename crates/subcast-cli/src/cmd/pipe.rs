//! Composable end-to-end pipeline (`subcast pipe <target>`).
use crate::cmd::{ensure_transcript, resolve_or_ingest_episode};
use crate::exit::CliError;
use crate::output::Out;
use clap::Args;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use subcast_core::{ai, highlights, ops, render, resolver, settings, subexport, CoreCtx, ProgressSink};

#[derive(Args)]
pub struct PipeArgs {
    /// Episode ref, feed/YouTube/Xiaoyuzhou URL, or local audio/video file
    pub target: String,
    /// Comma-separated stages: transcribe,translate,guide,highlights,clip,caption,poster
    #[arg(long, default_value = "transcribe,translate,guide,highlights,caption")]
    pub stages: String,
    #[arg(long)]
    pub intent: Option<String>,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    #[arg(long, default_value = "vertical")]
    pub preset: String,
    #[arg(long, default_value = "srt")]
    pub format: String,
    #[arg(long, default_value = "paper")]
    pub theme: String,
    #[arg(short = 'o', long = "output-dir")]
    pub output_dir: Option<PathBuf>,
    #[arg(long)]
    pub force: bool,
}

pub async fn run_pipe(
    ctx: &CoreCtx,
    args: PipeArgs,
    sink: Arc<dyn ProgressSink>,
    out: Out,
) -> Result<(), CliError> {
    let want: Vec<String> = args
        .stages
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect();
    if want.is_empty() {
        return Err(CliError::usage("specify at least one stage in --stages"));
    }

    let out_dir = args
        .output_dir
        .unwrap_or_else(|| PathBuf::from(format!("./subcast-out")));
    std::fs::create_dir_all(&out_dir)?;

    let ep = resolve_or_ingest_episode(ctx, &args.target).await?;
    let mut result = json!({
        "episodeId": ep.id,
        "title": ep.title,
        "outputDir": out_dir,
        "stages": {}
    });

    if want.iter().any(|s| s == "transcribe") {
        let local = std::path::Path::new(&args.target);
        let eff = settings::load_effective(ctx)?;
        let model = eff["asrModel"].as_str().unwrap_or("qwen3-asr");
        let lang = eff["asrLanguage"].as_str().unwrap_or("auto");
        let tr = if !args.force {
            if let Ok(t) = ensure_transcript(ctx, &ep, true, true).await {
                t
            } else if local.is_file() {
                ops::transcribe_local_file(ctx, local, model, lang, None, sink.as_ref()).await?.1
            } else {
                ops::transcribe(ctx, &ep.id, model, lang, None, false, false, sink.clone()).await?
            }
        } else if local.is_file() {
            ops::transcribe_local_file(ctx, local, model, lang, None, sink.as_ref()).await?.1
        } else {
            ops::transcribe(ctx, &ep.id, model, lang, None, false, false, sink.clone()).await?
        };
        result["stages"]["transcribe"] = json!({ "source": tr.source, "segments": tr.segments.len() });
    }

    if want.iter().any(|s| s == "translate") {
        ensure_transcript(ctx, &ep, true, true).await?;
        let r = ai::translate_episode(
            ctx,
            &ep.id,
            "auto",
            None,
            args.provider.as_deref(),
            args.model.as_deref(),
            "zh-CN",
            args.force,
            true,
            sink.as_ref(),
        )
        .await?;
        result["stages"]["translate"] = serde_json::to_value(r).unwrap_or_default();
    }

    if want.iter().any(|s| s == "guide") {
        ensure_transcript(ctx, &ep, true, true).await?;
        let (md, src) = ai::generate_guide(
            ctx,
            &ep.id,
            args.provider.as_deref(),
            args.model.as_deref(),
            None,
            args.force,
            true,
            sink.as_ref(),
        )
        .await?;
        let path = out_dir.join(subexport::safe_filename(&ep.title, "guide.md"));
        std::fs::write(&path, &md)?;
        result["stages"]["guide"] = json!({ "source": src, "path": path });
    }

    let mut top_clip: Option<highlights::HighlightClip> = None;
    if want.iter().any(|s| s == "highlights") {
        ensure_transcript(ctx, &ep, true, true).await?;
        let b = highlights::extract_highlights(
            ctx,
            &ep.id,
            args.intent.as_deref(),
            args.provider.as_deref(),
            args.model.as_deref(),
            sink.as_ref(),
        )
        .await?;
        top_clip = b.clips.first().cloned();
        let path = out_dir.join(subexport::safe_filename(&ep.title, "highlights.json"));
        std::fs::write(&path, serde_json::to_string_pretty(&b)?)?;
        result["stages"]["highlights"] = json!({ "count": b.clips.len(), "path": path });
    }

    if want.iter().any(|s| s == "caption") {
        let tr = ensure_transcript(ctx, &ep, true, true).await?;
        let zh = ai::load_episode_zh(ctx, &ep.id);
        let fmt = subexport::SubFormat::parse(&args.format)
            .ok_or_else(|| CliError::usage(format!("unsupported caption format: {}", args.format)))?;
        let cues = subexport::build_cues(&tr.segments, zh.as_deref(), subexport::SubContent::Bilingual);
        let blocks = subexport::build_blocks(&tr.segments, zh.as_deref(), subexport::SubContent::Bilingual);
        let meta = subexport::ExportMeta { title: ep.title.clone(), show: ep.show_title.clone(), url: ep.link.clone() };
        let rendered = subexport::render(fmt, &cues, &blocks, &meta);
        let path = out_dir.join(subexport::safe_filename(&ep.title, fmt.ext()));
        std::fs::write(&path, rendered)?;
        result["stages"]["caption"] = json!({ "format": fmt.ext(), "path": path });
    }

    if want.iter().any(|s| s == "clip") {
        let c = if let Some(c) = top_clip.as_ref() {
            (c.start, c.end)
        } else {
            (0.0, ep.duration.unwrap_or(60.0).min(60.0).max(5.0))
        };
        let clip_out = out_dir.join(subexport::safe_filename(&ep.title, "mp4"));
        let res = ops::clip_video_full(
            ctx,
            ops::ClipVideoOptions {
                episode_id: ep.id.clone(),
                job_id: format!("pipe-clip-{}", uuid::Uuid::new_v4().simple()),
                ranges: vec![c],
                preset: Some(args.preset.clone()),
                output_path: Some(clip_out.to_string_lossy().into_owned()),
                ..Default::default()
            },
            sink.as_ref(),
        )
        .await?;
        result["stages"]["clip"] = serde_json::to_value(res).unwrap_or_default();
    }

    if want.iter().any(|s| s == "poster") {
        let tr = ensure_transcript(ctx, &ep, true, true).await?;
        let zh = ai::load_episode_zh(ctx, &ep.id);
        let target_t = top_clip.as_ref().map(|c| c.start).unwrap_or(0.0);
        let (si, _) = highlights::range_from_clock(&tr.segments, target_t, None);
        if let Some(seg) = tr.segments.get(si) {
            let qz = zh.as_ref().and_then(|arr| arr.get(si)).and_then(|x| x.clone()).unwrap_or_default();
            let png = render::render_poster_png(&render::PosterOpts {
                theme: args.theme.clone(),
                show_title: ep.show_title.clone().unwrap_or_default(),
                episode_title: ep.title.clone(),
                time_label: subexport::clock_of(seg.start),
                quote_orig: seg.text.clone(),
                quote_zh: qz,
                share_url: ep.link.clone(),
                ..Default::default()
            })?;
            let path = out_dir.join(subexport::safe_filename(&ep.title, "poster.png"));
            std::fs::write(&path, &png)?;
            result["stages"]["poster"] = json!({ "path": path, "bytes": png.len() });
        }
    }

    let _ = resolver::show_display_name; // ensures resolver link stays exercised if needed
    out.ok(&result, || {
        println!("Pipeline complete for {} -> {}", ep.title, out_dir.display());
    });
    Ok(())
}
