//! Episode listing, metadata, favorites, playback, and listening stats.
use crate::cmd::resolve_or_ingest_episode;
use crate::exit::CliError;
use crate::output::{format_duration, truncate, Out};
use clap::{Args, Subcommand};
use serde_json::{json, Value};
use subcast_core::{ai, db, ops, resolver, CoreCtx, ProgressSink};

#[derive(Args)]
pub struct EpArgs {
    #[command(subcommand)]
    pub action: Option<EpAction>,
}

#[derive(Subcommand)]
pub enum EpAction {
    /// List episodes (newest first, optionally filtered by show)
    List {
        /// Optional show reference
        show: Option<String>,
        #[arg(long, default_value_t = 30)]
        limit: i64,
    },
    /// Show detailed metadata for an episode
    Show { reference: String },
    /// Update episode playback progress (seconds or HH:MM:SS)
    Progress { reference: String, time: String },
    /// Set or clear watched override (done | unseen | auto)
    Watched {
        reference: String,
        #[arg(default_value = "done")]
        state: String,
    },
}

#[derive(Args)]
pub struct FavArgs {
    #[command(subcommand)]
    pub action: Option<FavAction>,
}

#[derive(Subcommand)]
pub enum FavAction {
    List,
    Add { reference: String },
    Remove { reference: String },
}

#[derive(Args)]
pub struct StatsArgs {
    /// Range: 7d | 30d | all
    #[arg(long, default_value = "30d")]
    pub range: String,
    /// Generate AI listener portrait
    #[arg(long)]
    pub portrait: bool,
    #[arg(long)]
    pub regenerate: bool,
    #[arg(long)]
    pub provider: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
}

#[derive(Args)]
pub struct PlayArgs {
    pub reference: String,
    #[arg(long)]
    pub at: Option<String>,
    #[arg(long)]
    pub url_only: bool,
}

pub async fn run_ep(ctx: &CoreCtx, args: EpArgs, out: Out) -> Result<(), CliError> {
    let action = args.action.unwrap_or(EpAction::List { show: None, limit: 30 });
    match action {
        EpAction::List { show, limit } => {
            let library = {
                let conn = ctx.db.lock();
                match show.as_deref() {
                    Some(r) => resolver::resolve_show(&conn, r).ok().map(|s| db::list_episodes(&conn, Some(&s.id), limit)).transpose()?,
                    None => Some(db::recent_episodes(&conn, limit)?),
                }
            };
            let eps = match (library, show.as_deref()) {
                (Some(eps), _) => eps,
                // Not subscribed: a feed or channel link, or a show name looked up in the directory.
                (None, Some(r)) => {
                    let prev = if resolver::is_url_or_path(r) { subcast_core::ops::preview_show(ctx, r).await? } else { crate::cmd::find_show(ctx, r).await? };
                    if !out.is_structured() { eprintln!("{}（未订阅）", prev.show.title); }
                    prev.episodes.into_iter().take(limit.max(0) as usize).collect()
                }
                (None, None) => Vec::new(),
            };
            out.ok(&eps, || {
                for e in &eps {
                    let dur = e.duration.map(format_duration).unwrap_or_else(|| "-".into());
                    let flags = format!(
                        "{}{}",
                        if e.has_transcript { "T" } else { "-" },
                        if e.video_id.is_some() { "V" } else { "-" }
                    );
                    println!("{:<20}  [{flags}]  {:>7}  {}", e.id, dur, truncate(&e.title, 64));
                }
            });
        }
        EpAction::Show { reference } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let (tr, guide) = {
                let conn = ctx.db.lock();
                let tr = db::get_transcript(&conn, &ep.id)?;
                let guide = db::get_blob(&conn, &ep.id, "summary")?.filter(|s| !s.trim().is_empty());
                (tr, guide.is_some())
            };
            let zh = ai::load_episode_zh(ctx, &ep.id);
            let zh_count = zh.map(|v| v.iter().filter(|x| x.is_some()).count()).unwrap_or(0);
            let guide = guide;
            let payload = json!({
                "episode": ep,
                "transcriptSegments": tr.as_ref().map(|t| t.segments.len()).unwrap_or(0),
                "transcriptSource": tr.as_ref().map(|t| t.source.as_str()),
                "translatedSegments": zh_count,
                "hasGuide": guide,
            });
            out.ok(&payload, || {
                println!("Title      : {}", ep.title);
                println!("ID         : {}", ep.id);
                println!("Show       : {}", ep.show_title.as_deref().unwrap_or(&ep.show_id));
                if let Some(d) = ep.duration {
                    println!("Duration   : {}", format_duration(d));
                }
                if let Some(v) = &ep.video_id {
                    println!("Video      : https://youtube.com/watch?v={v}");
                }
                if let Some(a) = &ep.audio_url {
                    println!("Audio      : {a}");
                }
                println!(
                    "Transcript : {} ({} segments, {} zh)",
                    tr.as_ref().map(|t| t.source.as_str()).unwrap_or("none"),
                    tr.as_ref().map(|t| t.segments.len()).unwrap_or(0),
                    zh_count
                );
                println!("Guide      : {}", if guide { "yes" } else { "no" });
            });
        }
        EpAction::Progress { reference, time } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let sec = resolver::parse_timestamp(&time)?;
            db::set_progress(&ctx.db.lock(), &ep.id, sec)?;
            out.ok(&json!({ "episodeId": ep.id, "progress": sec }), || {
                println!("Updated progress for {} to {}", ep.id, format_duration(sec));
            });
        }
        EpAction::Watched { reference, state } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let val = match state.trim().to_ascii_lowercase().as_str() {
                "done" | "watched" | "true" => Some("done"),
                "unseen" | "false" => Some("unseen"),
                "auto" | "clear" | "none" => None,
                _ => return Err(CliError::usage("state must be done | unseen | auto")),
            };
            db::set_watched(&ctx.db.lock(), &ep.id, val)?;
            out.ok(&json!({ "episodeId": ep.id, "watchedOverride": val }), || {
                println!("Set watched state for {} -> {:?}", ep.id, val);
            });
        }
    }
    Ok(())
}

pub async fn run_fav(ctx: &CoreCtx, args: FavArgs, out: Out) -> Result<(), CliError> {
    let action = args.action.unwrap_or(FavAction::List);
    match action {
        FavAction::List => {
            let favs = ops::favorites_list(ctx)?;
            out.ok(&favs, || {
                let conn = ctx.db.lock();
                for f in &favs {
                    let title = db::get_episode(&conn, &f.id)
                        .ok()
                        .flatten()
                        .map(|e| e.title)
                        .unwrap_or_default();
                    println!("{:<20}  {}", f.id, title);
                }
            });
        }
        FavAction::Add { reference } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let favs = ops::favorites_add(ctx, &ep.id)?;
            out.ok(&favs, || println!("Favorited: {} ({})", ep.title, ep.id));
        }
        FavAction::Remove { reference } => {
            let ep = resolve_or_ingest_episode(ctx, &reference).await?;
            let favs = ops::favorites_remove(ctx, &ep.id)?;
            out.ok(&favs, || println!("Unfavorited: {} ({})", ep.title, ep.id));
        }
    }
    Ok(())
}

pub async fn run_stats(
    ctx: &CoreCtx,
    args: StatsArgs,
    sink: &dyn ProgressSink,
    out: Out,
) -> Result<(), CliError> {
    let since = match args.range.as_str() {
        "7d" => (chrono::Local::now() - chrono::Duration::days(7)).format("%Y-%m-%d").to_string(),
        "30d" => (chrono::Local::now() - chrono::Duration::days(30)).format("%Y-%m-%d").to_string(),
        _ => "1970-01-01".to_string(),
    };
    let rows: Vec<Value> = db::listens_since(&ctx.db.lock(), &since)?;
    let total_secs: f64 = rows.iter().filter_map(|r| r["seconds"].as_f64()).sum();
    if args.portrait {
        let portrait = ai::generate_portrait(
            ctx,
            &args.range,
            args.provider.as_deref(),
            args.model.as_deref(),
            args.regenerate,
            sink,
        )
        .await?;
        out.ok(&json!({ "range": args.range, "seconds": total_secs, "portrait": portrait }), || {
            println!("Listened {} ({} sessions)\n\n{}", format_duration(total_secs), rows.len(), portrait);
        });
    } else {
        out.ok(&json!({ "range": args.range, "seconds": total_secs, "sessions": rows.len(), "rows": rows }), || {
            println!("Range    : {}", args.range);
            println!("Listened : {}", format_duration(total_secs));
            println!("Sessions : {}", rows.len());
        });
    }
    Ok(())
}

pub async fn run_play(ctx: &CoreCtx, args: PlayArgs, out: Out) -> Result<(), CliError> {
    let ep = resolve_or_ingest_episode(ctx, &args.reference).await?;
    let at_sec = match args.at {
        Some(ref t) => Some(resolver::parse_timestamp(t)?),
        None => None,
    };
    let target = if let Some(vid) = &ep.video_id {
        if let Some(s) = at_sec {
            format!("https://www.youtube.com/watch?v={vid}&t={}s", s.round() as u64)
        } else {
            format!("https://www.youtube.com/watch?v={vid}")
        }
    } else if let Some(saved) = subcast_core::offline::find(&ctx.data, &ep.id) {
        saved.path
    } else if let Some(a) = &ep.audio_url {
        a.clone()
    } else {
        return Err(CliError::not_found("这一集没有可播放的音频或视频链接"));
    };

    if args.url_only || out.is_structured() {
        out.ok(&json!({ "episodeId": ep.id, "url": target }), || println!("{target}"));
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&target).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&target).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd").args(["/C", "start", "", &target]).spawn();
    }
    println!("Opened: {target}");
    Ok(())
}
