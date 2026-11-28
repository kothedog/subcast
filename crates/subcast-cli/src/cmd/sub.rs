//! Subscription, search, curated picks, OPML import & export commands.
use crate::exit::CliError;
use crate::output::{truncate, Out};
use clap::{Args, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use subcast_core::{db, feeds, import, ops, picks, resolver, CoreCtx};

#[derive(Args)]
pub struct AddArgs {
    /// Podcast RSS URL, Apple Podcasts URL, Xiaoyuzhou URL, YouTube channel/video URL, or search keyword
    pub input: String,
}

#[derive(Args)]
pub struct PreviewArgs {
    /// Feed or channel URL to preview without subscribing
    pub input: String,
    #[arg(long, default_value_t = 10)]
    pub limit: usize,
}

#[derive(Args)]
pub struct ShowsArgs {
    #[command(subcommand)]
    pub action: Option<ShowsAction>,
}

#[derive(Subcommand)]
pub enum ShowsAction {
    /// List subscribed shows
    List,
    /// Refresh one show or all subscriptions
    Refresh {
        /// Show ID prefix or title (omit with --all to refresh every subscription)
        show: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Unsubscribe from a show
    Remove {
        show: String,
        #[arg(long)]
        keep_episode: Option<String>,
    },
    /// Search inside a show's titles, notes, and transcripts
    Search {
        show: String,
        query: String,
    },
}

#[derive(Args)]
pub struct SearchArgs {
    /// Search query on Apple Podcasts
    pub query: String,
    #[arg(long, default_value = "CN")]
    pub country: String,
}

#[derive(Args)]
pub struct PicksArgs {
    #[command(subcommand)]
    pub action: Option<PicksAction>,
}

#[derive(Subcommand)]
pub enum PicksAction {
    /// List curated recommended shows
    List {
        #[arg(long)]
        topic: Option<String>,
    },
    /// Subscribe to a curated pick by id or name
    Add { query: String },
}

#[derive(Args)]
pub struct ImportArgs {
    #[command(subcommand)]
    pub kind: ImportKind,
}

#[derive(Subcommand)]
pub enum ImportKind {
    /// Import subscriptions from an OPML XML file
    Opml { file: PathBuf },
    /// Import YouTube channel subscriptions via browser cookies
    Youtube {
        #[arg(default_value = "chrome")]
        browser: String,
    },
}

#[derive(Args)]
pub struct ExportArgs {
    #[command(subcommand)]
    pub kind: ExportKind,
}

#[derive(Subcommand)]
pub enum ExportKind {
    /// Export subscriptions to OPML XML (`-o -` prints XML to stdout)
    Opml {
        #[arg(short = 'o', long = "output")]
        output: Option<String>,
    },
}

pub async fn run_add(ctx: &CoreCtx, args: AddArgs, out: Out) -> Result<(), CliError> {
    let added = ops::add_subscription(ctx, &args.input).await?;
    out.ok(&added, || {
        if let Some(s) = &added.show {
            println!("Subscribed: {} [{}] ({})", s.title, s.kind, s.id);
        }
        if let Some(e) = &added.episode {
            println!("Added episode: {} ({})", e.title, e.id);
        }
    });
    Ok(())
}

pub async fn run_preview(ctx: &CoreCtx, args: PreviewArgs, out: Out) -> Result<(), CliError> {
    let prev = ops::preview_show(ctx, &args.input).await?;
    out.ok(&prev, || {
        println!("{} [{}] ({} episodes)", prev.show.title, prev.show.kind, prev.episodes.len());
        for ep in prev.episodes.iter().take(args.limit) {
            println!("  {}  {}", ep.id, truncate(&ep.title, 76));
        }
    });
    Ok(())
}

pub async fn run_shows(ctx: &CoreCtx, args: ShowsArgs, out: Out) -> Result<(), CliError> {
    let action = args.action.unwrap_or(ShowsAction::List);
    match action {
        ShowsAction::List => {
            let shows = db::list_shows(&ctx.db.lock())?;
            out.ok(&shows, || {
                if shows.is_empty() {
                    println!("No subscriptions yet. Try `subcast picks` or `subcast add <url>`.");
                    return;
                }
                for s in &shows {
                    println!("{:<20}  {:<10}  {:>3} new  {}", s.id, s.kind, s.unplayed, s.title);
                }
            });
        }
        ShowsAction::Refresh { show, all } => {
            if all || show.is_none() {
                let res = ops::refresh_all(ctx).await?;
                out.ok(&res, || {
                    println!(
                        "Refreshed {} shows, {} new episodes{}",
                        res.shows,
                        res.new_episodes,
                        if res.skipped { " (skipped cooldown)" } else { "" }
                    );
                });
            } else if let Some(q) = show {
                let target = resolver::resolve_show(&ctx.db.lock(), &q)?;
                let refreshed = ops::refresh_show(ctx, &target.id).await?;
                out.ok(&refreshed, || {
                    println!("Refreshed {} ({})", refreshed.title, refreshed.id);
                });
            }
        }
        ShowsAction::Remove { show, keep_episode } => {
            let target = resolver::resolve_show(&ctx.db.lock(), &show)?;
            db::delete_show(&ctx.db.lock(), &target.id, keep_episode.as_deref())?;
            out.ok(&json!({ "removed": target.id, "title": target.title }), || {
                println!("Removed subscription: {} ({})", target.title, target.id);
            });
        }
        ShowsAction::Search { show, query } => {
            let target = resolver::resolve_show(&ctx.db.lock(), &show)?;
            let hits = ops::search_show(ctx, &target.id, &query).await?;
            out.ok(&hits, || {
                if hits.is_empty() {
                    println!("No matches in {} for {:?}", target.title, query);
                    return;
                }
                for h in &hits {
                    let tm = h.time.map(crate::output::format_duration).unwrap_or_default();
                    println!(
                        "{}  [{}] {}  {}",
                        h.episode.id,
                        h.field,
                        tm,
                        truncate(&h.episode.title, 58)
                    );
                    if !h.snippet.is_empty() {
                        println!("    {}", h.snippet);
                    }
                }
            });
        }
    }
    Ok(())
}

pub async fn run_search(args: SearchArgs, out: Out) -> Result<(), CliError> {
    let hits = feeds::search_podcasts(&args.query, &args.country).await?;
    out.ok(&hits, || {
        for h in &hits {
            println!("{} — {}", h.title, h.author);
            println!("  {}", h.feed_url);
        }
    });
    Ok(())
}

pub async fn run_picks(ctx: &CoreCtx, args: PicksArgs, out: Out) -> Result<(), CliError> {
    let action = args.action.unwrap_or(PicksAction::List { topic: None });
    match action {
        PicksAction::List { topic } => {
            let items = picks::list(topic.as_deref());
            out.ok(&items, || {
                for p in &items {
                    println!(
                        "{:<22}  [{:<10}]  {} ({})",
                        p.id,
                        p.topics.join(","),
                        p.title,
                        p.by
                    );
                }
            });
        }
        PicksAction::Add { query } => {
            let (pick, added) = picks::subscribe(ctx, &query).await?;
            out.ok(&json!({ "pick": pick, "added": added }), || {
                println!("Subscribed to curated pick: {} — {}", pick.title, pick.by);
            });
        }
    }
    Ok(())
}

pub async fn run_import(ctx: &CoreCtx, args: ImportArgs, out: Out) -> Result<(), CliError> {
    match args.kind {
        ImportKind::Opml { file } => {
            let xml = std::fs::read_to_string(&file)
                .map_err(|e| CliError::not_found(format!("无法读取 OPML 文件: {e}")))?;
            let pairs = import::parse_opml(&xml)?;
            let mut imported = Vec::new();
            let mut failed = Vec::new();
            for (title, url) in pairs {
                match ops::add_subscription(ctx, &url).await {
                    Ok(added) => imported.push(json!({ "title": title, "url": url, "show": added.show })),
                    Err(err) => failed.push(json!({ "title": title, "url": url, "error": err.to_string() })),
                }
            }
            out.ok(&json!({ "imported": imported, "failed": failed }), || {
                println!(
                    "Imported {} show(s), {} failed",
                    imported.len(),
                    failed.len()
                );
            });
        }
        ImportKind::Youtube { browser } => {
            import::ensure_browser_access(&browser)?;
            let bin = subcast_core::asr::ytdlp(&ctx.data).await?;
            let channels = import::youtube_subscriptions(&bin, &browser).await?;
            out.ok(&channels, || {
                for c in &channels {
                    println!("{}  {}", c.id, c.title);
                }
            });
        }
    }
    Ok(())
}

pub async fn run_export(ctx: &CoreCtx, args: ExportArgs, out: Out) -> Result<(), CliError> {
    match args.kind {
        ExportKind::Opml { output } => {
            if output.as_deref() == Some("-") {
                let xml = ops::export_opml_xml(ctx)?;
                if out.is_structured() {
                    out.ok(&json!({ "xml": xml }), || {});
                } else {
                    print!("{xml}");
                }
            } else {
                let path = ops::export_opml(ctx, output.as_deref().map(std::path::Path::new))?;
                out.ok(&json!({ "path": path }), || {
                    println!("Exported OPML to {path}");
                });
            }
        }
    }
    Ok(())
}
