//! End-to-end integration tests for the `subcast-cli` binary against isolated `--data-dir` stores.
use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use subcast_core::{db, ops, CoreCtx, Episode, Segment, Show, Transcript};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_subcast-cli")
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("subcast-cli-test-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_cli(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .env("SUBCAST_NO_KEYCHAIN", "1")
        .arg("--data-dir")
        .arg(dir)
        .args(args)
        .output()
        .expect("spawn subcast-cli")
}

fn run_json(dir: &Path, args: &[&str]) -> Value {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let out = run_cli(dir, &full);
    assert!(
        out.status.success(),
        "cli failed ({}):\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("valid JSON stdout")
}

fn seed_library(dir: &Path) {
    let ctx = CoreCtx::open(Some(dir.to_path_buf())).unwrap();
    let show = Show {
        id: "show-demo-1".into(),
        kind: "podcast".into(),
        title: "Demo Tech Podcast".into(),
        author: Some("Host".into()),
        feed_url: "https://example.com/feed.xml".into(),
        link: Some("https://example.com".into()),
        updated_at: 1_700_000_000_000,
        ..Default::default()
    };
    let ep = Episode {
        id: "ep-demo-001".into(),
        show_id: show.id.clone(),
        guid: "guid-1".into(),
        title: "EP001 Small Teams & Sharp Tools".into(),
        published: Some(1_700_000_000_000),
        duration: Some(1800.0),
        audio_url: Some("https://example.com/ep1.mp3".into()),
        link: Some("https://example.com/ep1".into()),
        ..Default::default()
    };
    let mut conn = ctx.db.lock();
    db::upsert_show(&conn, &show).unwrap();
    db::upsert_episodes(&mut conn, &[ep.clone()]).unwrap();
    let tr = Transcript {
        episode_id: ep.id.clone(),
        source: "rss".into(),
        language: "en".into(),
        segments: vec![
            Segment { start: 0.5, end: 3.2, text: "Small teams move faster.".into() },
            Segment { start: 3.4, end: 6.8, text: "Sharp tools beat complexity.".into() },
        ],
    };
    db::save_transcript(&conn, &tr).unwrap();
    db::set_blob(&conn, &ep.id, "zh", r#"["小团队行动更快。","锋利的工具胜过复杂性。"]"#).unwrap();
}

#[test]
fn help_doctor_picks_and_completions() {
    let dir = TempDir::new("basic");
    let help = Command::new(bin()).arg("--help").output().unwrap();
    assert!(help.status.success());
    let help_s = String::from_utf8_lossy(&help.stdout);
    assert!(help_s.contains("SubCast CLI"));

    let doc = run_json(dir.path(), &["doctor"]);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["episodes"], 0);

    let picks = run_json(dir.path(), &["picks", "list"]);
    let arr = picks.as_array().unwrap();
    assert!(arr.len() >= 50, "expected >=50 picks, got {}", arr.len());

    let comp = run_cli(dir.path(), &["completions", "zsh"]);
    assert!(comp.status.success());
    let comp_s = String::from_utf8_lossy(&comp.stdout);
    assert!(comp_s.contains("subcast"));
}

#[test]
fn config_roundtrip_preserves_unknown_keys() {
    let dir = TempDir::new("config");
    {
        let ctx = CoreCtx::open(Some(dir.path().to_path_buf())).unwrap();
        let initial = serde_json::json!({
            "proxy": "",
            "customDesktopFlag": { "nested": 99 }
        });
        ops::set_settings(&ctx, &initial).unwrap();
    }

    let set_res = run_json(dir.path(), &["config", "set", "translateQuality", "deep"]);
    assert_eq!(set_res["value"], "deep");

    let got = run_json(dir.path(), &["config", "get"]);
    assert_eq!(got["translateQuality"], "deep");
    assert_eq!(got["customDesktopFlag"]["nested"], 99);
}

#[test]
fn seeded_library_shows_ep_fav_captions_and_opml() {
    let dir = TempDir::new("library");
    seed_library(dir.path());

    let shows = run_json(dir.path(), &["shows", "list"]);
    assert!(shows.as_array().unwrap().iter().any(|s| s["id"] == "show-demo-1"));

    let eps = run_json(dir.path(), &["ep", "list", "show-demo-1"]);
    assert_eq!(eps.as_array().unwrap().len(), 1);

    let show_ep = run_json(dir.path(), &["ep", "show", "ep-demo-001"]);
    assert_eq!(show_ep["transcriptSegments"], 2);
    assert_eq!(show_ep["translatedSegments"], 2);

    let prog = run_json(dir.path(), &["ep", "progress", "ep-demo-001", "01:15"]);
    assert_eq!(prog["progress"], 75.0);

    let watched = run_json(dir.path(), &["ep", "watched", "ep-demo-001", "done"]);
    assert_eq!(watched["watchedOverride"], "done");

    let favs = run_json(dir.path(), &["fav", "add", "ep-demo-001"]);
    assert_eq!(favs.as_array().unwrap().len(), 1);
    let fav_list = run_json(dir.path(), &["fav", "list"]);
    assert_eq!(fav_list[0]["id"], "ep-demo-001");
    let fav_rm = run_json(dir.path(), &["fav", "remove", "ep-demo-001"]);
    assert_eq!(fav_rm.as_array().unwrap().len(), 0);

    for (fmt, marker) in [
        ("srt", "-->"),
        ("vtt", "WEBVTT"),
        ("ass", "[Script Info]"),
        ("lrc", "[00:00"),
        ("txt", "Small teams"),
        ("md", "# EP001"),
    ] {
        let res = run_json(dir.path(), &["caption", "ep-demo-001", "--format", fmt]);
        let txt = res["text"].as_str().unwrap();
        assert!(txt.contains(marker), "format {fmt} missing marker {marker}: {txt}");
    }

    let opml = run_cli(dir.path(), &["export", "opml", "-o", "-"]);
    assert!(opml.status.success());
    let opml_xml = String::from_utf8_lossy(&opml.stdout);
    assert!(opml_xml.contains("<opml"));
    assert!(opml_xml.contains("https://example.com/feed.xml"));
}

#[test]
fn vocab_fsrs_csv_anki_and_poster() {
    let dir = TempDir::new("vocab");

    let added = run_json(
        dir.path(),
        &[
            "vocab",
            "add",
            "ephemeral",
            "--meaning",
            "adj. 短暂的",
            "--sentence",
            "Trends are ephemeral.",
        ],
    );
    assert_eq!(added["word"], "ephemeral");

    let reviewed = run_json(dir.path(), &["vocab", "review", "ephemeral", "--rate", "good"]);
    assert_eq!(reviewed["card"]["reps"], 1);
    assert!(reviewed["card"]["state"].as_u64().unwrap() >= 1);

    let graduated = run_json(dir.path(), &["vocab", "review", "ephemeral", "--rate", "easy"]);
    assert_eq!(graduated["card"]["reps"], 2);
    assert!(graduated["card"]["scheduled_days"].as_u64().unwrap() >= 1);

    let stats = run_json(dir.path(), &["vocab", "stats"]);
    assert_eq!(stats["total"], 1);

    let csv_path = dir.path().join("words.csv");
    run_json(
        dir.path(),
        &["vocab", "export", "--format", "csv", "-o", csv_path.to_str().unwrap()],
    );
    let csv_text = std::fs::read_to_string(&csv_path).unwrap();
    assert!(csv_text.contains("ephemeral"));

    let apkg_path = dir.path().join("words.apkg");
    run_json(
        dir.path(),
        &["vocab", "export", "--format", "anki", "-o", apkg_path.to_str().unwrap()],
    );
    let apkg_bytes = std::fs::read(&apkg_path).unwrap();
    assert!(apkg_bytes.starts_with(b"PK\x03\x04"));

    let poster_path = dir.path().join("quote.png");
    run_json(
        dir.path(),
        &[
            "poster",
            "--orig",
            "Simplicity is reliability.",
            "--zh",
            "简单即可靠。",
            "--theme",
            "night",
            "--share-url",
            "https://subcast.qiaomu.ai/e/demo",
            "-o",
            poster_path.to_str().unwrap(),
        ],
    );
    let png = std::fs::read(&poster_path).unwrap();
    assert!(png.starts_with(&[0x89, 80, 78, 71, 13, 10, 26, 10]));
}

#[test]
fn mcp_jsonrpc_roundtrip_and_asr_worker_bypass() {
    let dir = TempDir::new("mcp");
    seed_library(dir.path());

    let mut child = Command::new(bin())
        .arg("--data-dir")
        .arg(dir.path())
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list"}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{{"name":"doctor","arguments":{{}}}}}}"#).unwrap();
        writeln!(stdin, r#"{{"jsonrpc":"2.0","id":4,"method":"resources/read","params":{{"uri":"subcast://shows"}}}}"#).unwrap();
    }

    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[0]["result"]["serverInfo"]["name"], "subcast");
    assert!(lines[1]["result"]["tools"].as_array().unwrap().len() >= 15);
    assert_eq!(lines[2]["result"]["isError"], false);
    assert!(lines[3]["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .contains("show-demo-1"));

    // Verify `--asr-worker` bypasses Clap (missing args goes to worker_main, not Clap exit code 2).
    let worker = Command::new(bin()).arg("--asr-worker").output().unwrap();
    assert_ne!(worker.status.code(), Some(2));
}
