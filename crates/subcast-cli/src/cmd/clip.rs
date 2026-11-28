//! Audio/video highlight cutting, clip cover PNG rendering, and quote poster rendering.
use crate::cmd::{ensure_transcript, resolve_or_ingest_episode};
use crate::exit::CliError;
use crate::output::Out;
use clap::{Args, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use subcast_core::{ai, db, highlights, ops, render, resolver, subexport, CoreCtx, ProgressSink};

#[derive(Args)]
pub struct ClipArgs {
    #[command(subcommand)]
    pub action: ClipAction,
}

#[derive(Subcommand)]
pub enum ClipAction {
    /// Cut an audio excerpt from an episode
    Audio {
        reference: String,
        #[arg(long)]
        start: String,
        #[arg(long)]
        end: String,
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
    },
    /// Render a subtitled video clip from a highlight index, time range(s), or montage
    Video {
        reference: String,
        /// Start time (seconds or HH:MM:SS); repeatable for montage ranges paired with --end
        #[arg(long)]
        start: Vec<String>,
        #[arg(long)]
        end: Vec<String>,
        /// Highlight index (0-based) if --start/--end omitted
        #[arg(long)]
        highlight: Option<usize>,
        /// Preset: vertical | xhs | landscape | moments (or douyin | wechat | xiaohongshu | bilibili | youtube | square)
        #[arg(long, default_value = "vertical")]
        preset: String,
        /// Aspect override: source | 9:16 | 3:4 | 1:1 | 16:9
        #[arg(long)]
        aspect: Option<String>,
        /// Fit override: card | fill
        #[arg(long)]
        fit: Option<String>,
        /// Card crop override: none | 4:3 | 1:1
        #[arg(long)]
        card_crop: Option<String>,
        /// Card background: dark | paper | white | blur
        #[arg(long)]
        card_bg: Option<String>,
        /// Subtitle content: bilingual | original | translation
        #[arg(long, default_value = "bilingual")]
        subs: String,
        /// Place subtitles below the video card (`true`/`false`)
        #[arg(long)]
        subs_below: Option<bool>,
        /// Playback speed (0.5–2.0)
        #[arg(long)]
        speed: Option<f64>,
        /// Remove silent pauses (`true`/`false`)
        #[arg(long)]
        remove_pauses: Option<bool>,
        /// Normalize audio loudness to -14 LUFS (`true`/`false`)
        #[arg(long)]
        loudness: Option<bool>,
        /// Cover mode: flash | hold | none
        #[arg(long)]
        cover_mode: Option<String>,
        /// Cover template: impact | marker | split | bold | quote | magazine | cinema | minimal
        #[arg(long)]
        cover_template: Option<String>,
        /// Override title / hook headline (supports `【highlight】`)
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        sub_title: Option<String>,
        #[arg(long)]
        accent: Option<String>,
        #[arg(long)]
        crop_x: Option<f64>,
        #[arg(long)]
        no_brand: bool,
        #[arg(long)]
        save_cover: bool,
        #[arg(long)]
        save_landscape_cover: bool,
        #[arg(short = 'o', long = "output")]
        output: Option<String>,
        #[arg(long)]
        output_dir: Option<String>,
    },
    /// Render a standalone share-ready cover PNG
    Cover {
        #[arg(long, default_value = "impact")]
        template: String,
        #[arg(long, default_value_t = 1080)]
        width: u32,
        #[arg(long, default_value_t = 1440)]
        height: u32,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        sub_title: String,
        #[arg(long, default_value = "")]
        kicker: String,
        #[arg(long, default_value = "")]
        show: String,
        #[arg(long, default_value = "SubCast")]
        brand: String,
        #[arg(long, default_value_t = 0.0)]
        duration: f64,
        #[arg(long, default_value = "")]
        accent: String,
        #[arg(long, default_value = "sans")]
        font: String,
        #[arg(long)]
        backdrop: Option<String>,
        #[arg(short = 'o', long = "output")]
        output: PathBuf,
    },
}

#[derive(Args)]
pub struct PosterArgs {
    /// Episode reference (or omit when passing --orig / --zh directly)
    pub reference: Option<String>,
    /// Theme: paper | warm | night | bamboo | sand
    #[arg(long, default_value = "paper")]
    pub theme: String,
    #[arg(long)]
    pub at: Option<String>,
    #[arg(long)]
    pub orig: Option<String>,
    #[arg(long)]
    pub zh: Option<String>,
    #[arg(long)]
    pub show: Option<String>,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long)]
    pub time_label: Option<String>,
    #[arg(long)]
    pub cover: Option<String>,
    #[arg(long)]
    pub share_url: Option<String>,
    #[arg(short = 'o', long = "output", default_value = "poster.png")]
    pub output: PathBuf,
}

pub async fn run_clip(
    ctx: &CoreCtx,
    args: ClipArgs,
    sink: &dyn ProgressSink,
    out: Out,
) -> Result<(), CliError> {
    match args.action {
        ClipAction::Audio { reference, start, end, output } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let s = resolver::parse_timestamp(&start)?;
            let e = resolver::parse_timestamp(&end)?;
            let generated = ops::learn_clip(ctx, &ep.id, s, e).await?;
            let final_path = if let Some(dest) = output {
                if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&generated, &dest)?;
                dest.to_string_lossy().into_owned()
            } else {
                generated
            };
            out.ok(&json!({ "episodeId": ep.id, "path": final_path, "start": s, "end": e }), || {
                println!("Saved audio clip to {final_path}");
            });
        }
        ClipAction::Video {
            reference,
            start,
            end,
            highlight,
            preset,
            aspect,
            fit,
            card_crop,
            card_bg,
            subs,
            subs_below,
            speed,
            remove_pauses,
            loudness,
            cover_mode,
            cover_template,
            title,
            sub_title,
            accent,
            crop_x,
            no_brand,
            save_cover,
            save_landscape_cover,
            output,
            output_dir,
        } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let subs_content = subexport::SubContent::parse(&subs)
                .ok_or_else(|| CliError::usage(format!("不支持的字幕内容选项：{subs}")))?;

            let mut ranges = Vec::new();
            if !start.is_empty() || !end.is_empty() {
                if start.len() != end.len() {
                    return Err(CliError::usage("--start and --end counts must match"));
                }
                for (sa, ea) in start.iter().zip(end.iter()) {
                    let s = resolver::parse_timestamp(sa)?;
                    let e = resolver::parse_timestamp(ea)?;
                    if e <= s {
                        return Err(CliError::usage("each --end must be greater than --start"));
                    }
                    ranges.push((s, e));
                }
            } else {
                let raw = db::get_blob(&ctx.db.lock(), &ep.id, "highlights")?
                    .ok_or_else(|| CliError::not_found("请提供 --start/--end，或先运行 highlights extract"))?;
                let b: highlights::HighlightBundle = serde_json::from_str(&raw)
                    .map_err(|e| CliError::new(1, "parse", e.to_string()))?;
                let idx = highlight.unwrap_or(0);
                let c = b.clips.get(idx).ok_or_else(|| CliError::not_found("高光序号超出范围"))?;
                if let Some(parts) = c.parts.as_ref().filter(|p| p.len() > 1) {
                    for p in parts {
                        ranges.push((p.start, p.end));
                    }
                } else {
                    ranges.push((c.start, c.end));
                }
            }

            let opts = ops::ClipVideoOptions {
                episode_id: ep.id.clone(),
                job_id: format!("cli-clip-{}", uuid::Uuid::new_v4().simple()),
                ranges,
                preset: Some(preset),
                aspect,
                fit,
                card_crop,
                card_bg,
                subs_content,
                subs_below,
                speed,
                remove_pauses,
                loudness,
                cover_mode,
                cover_template,
                hook_title: title,
                sub_title,
                accent,
                crop_x,
                embed_thumbnail: true,
                save_cover_sidecar: save_cover,
                save_landscape_cover,
                output_path: output,
                output_dir,
                show_brand: !no_brand,
            };
            let res = ops::clip_video_full(ctx, opts, sink).await?;
            out.ok(&res, || {
                println!("Video : {}", res.video_path);
                if let Some(c) = &res.cover_path {
                    println!("Cover : {c}");
                }
                if let Some(l) = &res.landscape_cover_path {
                    println!("Land  : {l}");
                }
            });
        }
        ClipAction::Cover {
            template,
            width,
            height,
            title,
            sub_title,
            kicker,
            show,
            brand,
            duration,
            accent,
            font,
            backdrop,
            output,
        } => {
            let png = render::render_cover_png(&render::CoverOpts {
                template,
                width,
                height,
                hook_title: title,
                sub_title,
                kicker,
                show_name: show,
                brand,
                duration_sec: duration,
                accent,
                font,
                backdrop_path: backdrop,
                crop_bottom: 0.0,
            })?;
            if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&output, &png)?;
            out.ok(&json!({ "path": output, "bytes": png.len(), "width": width, "height": height }), || {
                println!("Rendered cover PNG to {}", output.display());
            });
        }
    }
    Ok(())
}

pub async fn run_poster(ctx: &CoreCtx, args: PosterArgs, out: Out) -> Result<(), CliError> {
    let mut show_title = args.show.clone().unwrap_or_default();
    let mut ep_title = args.title.clone().unwrap_or_default();
    let mut quote_orig = args.orig.clone().unwrap_or_default();
    let mut quote_zh = args.zh.clone().unwrap_or_default();
    let mut time_label = args.time_label.clone().unwrap_or_default();

    if let Some(ref r) = args.reference {
        let ep = resolve_or_ingest_episode(ctx, r).await?;
        if ep_title.is_empty() {
            ep_title = ep.title.clone();
        }
        if show_title.is_empty() {
            show_title = ep.show_title.clone().unwrap_or_default();
        }
        if quote_orig.is_empty() && quote_zh.is_empty() {
            if let Ok(tr) = ensure_transcript(ctx, &ep, true, true).await {
                let zh = ai::load_episode_zh(ctx, &ep.id);
                let target_sec = match args.at {
                    Some(ref t) => resolver::parse_timestamp(t)?,
                    None => 0.0,
                };
                let (si, _) = highlights::range_from_clock(&tr.segments, target_sec, None);
                if let Some(seg) = tr.segments.get(si) {
                    quote_orig = seg.text.clone();
                    if let Some(z) = zh.as_ref().and_then(|arr| arr.get(si)).and_then(|x| x.clone()) {
                        quote_zh = z;
                    }
                    if time_label.is_empty() {
                        time_label = subexport::clock_of(seg.start);
                    }
                }
            }
        }
    }

    if quote_orig.is_empty() && quote_zh.is_empty() {
        return Err(CliError::usage("Provide --orig/--zh or an episode reference with transcript"));
    }

    let png = render::render_poster_png(&render::PosterOpts {
        theme: args.theme.clone(),
        show_title,
        episode_title: ep_title,
        time_label,
        quote_orig,
        quote_zh,
        cover_path: args.cover,
        share_url: args.share_url,
        ..Default::default()
    })?;
    if let Some(parent) = args.output.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&args.output, &png)?;
    out.ok(&json!({ "path": args.output, "theme": args.theme, "bytes": png.len() }), || {
        println!("Rendered poster to {}", args.output.display());
    });
    Ok(())
}
