//! `subcast` CLI entry point, `--asr-worker` child dispatch, global flags, and Ctrl+C handling.
mod cmd;
mod exit;
mod output;

use clap::Parser;
use cmd::Commands;
use exit::{CliError, CANCELLED};
use output::{CliSink, Mode, Out};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use subcast_core::{asr, net, CoreCtx, NoopSink, ProgressSink};

#[derive(Parser)]
#[command(
    name = "subcast",
    bin_name = "subcast",
    version,
    about = "SubCast CLI — podcasts, YouTube, bilingual subtitles, AI workflows, clips, vocab & MCP",
    long_about = None
)]
pub struct Cli {
    /// Output machine-readable JSON on stdout
    #[arg(long, global = true, conflicts_with = "jsonl")]
    pub json: bool,

    /// Stream progress events on stderr and result on stdout as JSON Lines
    #[arg(long, global = true, conflicts_with = "json")]
    pub jsonl: bool,

    /// Suppress non-essential stdout/stderr output
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Never prompt for confirmation
    #[arg(long, global = true)]
    pub no_input: bool,

    /// Override SubCast data directory (defaults to OS app data directory)
    #[arg(long, env = "SUBCAST_DATA_DIR", global = true)]
    pub data_dir: Option<PathBuf>,

    /// Override HTTP/SOCKS proxy URL (or pass empty string for direct)
    #[arg(long, env = "SUBCAST_PROXY", global = true)]
    pub proxy: Option<String>,

    /// Override Netscape-format cookies file for YouTube yt-dlp requests
    #[arg(long, env = "SUBCAST_YT_COOKIES", global = true)]
    pub yt_cookies: Option<PathBuf>,

    /// Skip OS keychain reads (rely on environment variables only)
    #[arg(long, env = "SUBCAST_NO_KEYCHAIN", value_parser = clap::builder::BoolishValueParser::new(), global = true)]
    pub no_keychain: bool,

    #[command(subcommand)]
    pub command: Commands,
}

fn main() {
    // Whisper runs in a dedicated child process invoked as `<exe> --asr-worker ...`.
    let raw_args: Vec<String> = std::env::args().collect();
    if raw_args.get(1).map(String::as_str) == Some("--asr-worker") {
        std::process::exit(asr::worker_main(&raw_args[2..]));
    }

    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(err) => {
            let code = if err.use_stderr() { exit::USAGE } else { exit::OK };
            let _ = err.print();
            std::process::exit(code);
        }
    };

    let mode = if cli.quiet {
        Mode::Quiet
    } else if cli.jsonl {
        Mode::Jsonl
    } else if cli.json {
        Mode::Json
    } else {
        Mode::Human
    };
    let out = Out { mode };

    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            out.print_error(exit::ERR, "runtime", &err.to_string());
            std::process::exit(exit::ERR);
        }
    };

    let code = rt.block_on(async move {
        let ctx = match CoreCtx::open(cli.data_dir) {
            Ok(c) => Arc::new(c),
            Err(err) => {
                let ce = CliError::from_anyhow(err);
                out.print_error(ce.code, ce.kind, &ce.message);
                return ce.code;
            }
        };

        if cli.no_keychain {
            subcast_core::llm::set_no_keychain(true);
        }
        if let Some(proxy) = cli.proxy {
            let _ = net::set_proxy(&proxy);
        }
        if let Some(cookies) = cli.yt_cookies {
            *asr::YT_COOKIES_FILE.write() = Some(cookies);
        }

        let cancelled = Arc::new(AtomicBool::new(false));
        {
            let ctx_c = ctx.clone();
            let flag = cancelled.clone();
            tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    flag.store(true, Ordering::SeqCst);
                    ctx_c.cancel_all();
                }
            });
        }

        let sink: Arc<dyn ProgressSink> = match &cli.command {
            Commands::Mcp(_) => Arc::new(NoopSink),
            _ => Arc::new(CliSink { mode }),
        };

        let res = cmd::dispatch(cli.command, &ctx, sink, out, cli.no_input).await;
        if cancelled.load(Ordering::SeqCst) {
            let ce = CliError::cancelled();
            out.print_error(ce.code, ce.kind, &ce.message);
            return CANCELLED;
        }
        match res {
            Ok(()) => exit::OK,
            Err(err) => {
                out.print_error(err.code, err.kind, &err.message);
                err.code
            }
        }
    });

    std::process::exit(code);
}
