//! Offline Chinese TTS with sherpa-onnx: MeloTTS (one zh/en voice) and Kokoro v1.1 (many zh voices).
//!
//! Nothing ships in the app. The first time a listener picks a model, the pinned sherpa-onnx command-line build
//! (`bin/sherpa-onnx-<ver>/`) and that model's files (`tts-models/<id>/`, from Hugging Face, hf-mirror.com first
//! when asked) are downloaded and checked against the hashes in `local_tts_models.json`. Each sentence runs one
//! `sherpa-onnx-offline-tts` process, which writes a WAV we read back.
//! fp32 weights on purpose: on Apple Silicon the int8 builds were 3–4× slower (RTF ~0.9 vs ~0.2).
use crate::{asr, net};
use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

const SHERPA_VERSION: &str = "v1.13.8";
const MANIFEST: &str = include_str!("local_tts_models.json");

#[derive(Deserialize)]
struct ModelFiles { repo: String, revision: String, files: Vec<FileSpec> }

#[derive(Deserialize, Clone)]
struct FileSpec {
    path: String,
    size: u64,
    /// Hugging Face LFS files: SHA-256 of the content.
    #[serde(default)]
    sha256: Option<String>,
    /// Plain git files: the git blob id, `sha1("blob <len>\0" + content)`.
    #[serde(default)]
    sha1: Option<String>,
}

fn manifest(id: &str) -> Result<ModelFiles> {
    let mut all: std::collections::HashMap<String, ModelFiles> = serde_json::from_str(MANIFEST)?;
    all.remove(id).ok_or_else(|| anyhow!("未知的本地语音模型：{}", id))
}

/// The sherpa-onnx release archive for this machine and its SHA-256 (from the GitHub release).
fn sherpa_asset() -> Option<(&'static str, &'static str)> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some(("osx-arm64-shared", "b10e5c7e2c30ea03de9c442655d14860d9edc475c6251d58a8f5f06e913a1d56"))
    } else if cfg!(target_os = "macos") {
        Some(("osx-x64-shared", "54aad64acee9d2d596535a6080d6f22602a720af5460e1d50461b1e1b06bee40"))
    } else if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        Some(("linux-aarch64-shared-cpu", "4e3734f82bc1379fd91f219f5869c7e9d03b7a4f7561907d8abca4849c51a789"))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some(("linux-x64-shared", "c0bdb7907d3a74bba1d55d22bf4d9fa75586cf1530614ebe88a27b9118e015c4"))
    } else {
        // Windows: the command-line build reads its text from ANSI argv, so Chinese would arrive garbled.
        None
    }
}

/// Whether Melo / Kokoro can run on this system at all.
pub fn supported() -> bool { sherpa_asset().is_some() }

fn runtime_dir(data: &Path) -> PathBuf { data.join("bin").join(format!("sherpa-onnx-{SHERPA_VERSION}")) }
fn runtime_bin(data: &Path) -> PathBuf { runtime_dir(data).join("bin").join("sherpa-onnx-offline-tts") }
pub fn models_dir(data: &Path) -> PathBuf { data.join("tts-models") }
fn model_dir(data: &Path, id: &str) -> PathBuf { models_dir(data).join(id) }
fn done_marker(data: &Path, id: &str) -> PathBuf { model_dir(data, id).join(".complete") }

fn runtime_ready(data: &Path) -> bool { runtime_bin(data).is_file() }
fn model_ready(data: &Path, id: &str) -> bool { done_marker(data, id).is_file() }

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelState {
    pub id: String,
    pub installed: bool,
    /// Download size of the model's files (not counting the ~20 MB runtime).
    pub bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub supported: bool,
    pub runtime: bool,
    pub models: Vec<ModelState>,
}

pub fn status(data: &Path) -> Status {
    let models = ["melo", "kokoro"].iter().map(|id| ModelState {
        id: id.to_string(),
        installed: supported() && runtime_ready(data) && model_ready(data, id),
        bytes: manifest(id).map(|m| m.files.iter().map(|f| f.size).sum()).unwrap_or(0),
    }).collect();
    Status { supported: supported(), runtime: runtime_ready(data), models }
}

pub fn delete(data: &Path, id: &str) -> Result<()> {
    manifest(id)?;
    let dir = model_dir(data, id);
    if dir.exists() { std::fs::remove_dir_all(dir)?; }
    Ok(())
}

/// Downloads whatever is missing for `id` (runtime and model files), reporting (received, total) bytes.
pub async fn install(data: &Path, id: &str, cancel: Arc<AtomicBool>, mirror_first: bool, mut progress: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let (asset, asset_sha) = sherpa_asset().ok_or_else(|| anyhow!("这个系统暂不支持本地语音模型"))?;
    let m = manifest(id)?;
    let dir = model_dir(data, id);
    let missing: Vec<FileSpec> = m.files.iter().filter(|f| !file_ok_by_size(&dir.join(&f.path), f.size)).cloned().collect();
    let model_total: u64 = missing.iter().map(|f| f.size).sum();
    let need_runtime = !runtime_ready(data);
    let runtime_guess: u64 = if need_runtime { 22 * 1024 * 1024 } else { 0 };
    let total = model_total + runtime_guess;
    let mut base = 0u64;

    if need_runtime {
        install_runtime(data, asset, asset_sha, &cancel, |got, _| progress(got.min(runtime_guess), Some(total))).await?;
        base = runtime_guess;
    }

    let counter = Arc::new(AtomicU64::new(0));
    let hosts: [&str; 2] = if mirror_first { ["hf-mirror.com", "huggingface.co"] } else { ["huggingface.co", "hf-mirror.com"] };
    let jobs = missing.into_iter().map(|f| {
        let (dest, counter, cancel) = (dir.join(&f.path), counter.clone(), cancel.clone());
        let (repo, rev) = (m.repo.clone(), m.revision.clone());
        async move { fetch_file(&repo, &rev, &f, &dest, hosts, &counter, &cancel).await }
    });
    let mut stream = futures_util::stream::iter(jobs).buffer_unordered(6);
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    loop {
        tokio::select! {
            next = stream.next() => match next {
                Some(Ok(())) => {}
                Some(Err(e)) => { cancel.store(true, Ordering::Relaxed); return Err(e); }
                None => break,
            },
            _ = tick.tick() => progress(base + counter.load(Ordering::Relaxed), Some(total)),
        }
    }
    std::fs::write(done_marker(data, id), SHERPA_VERSION)?;
    progress(total, Some(total));
    Ok(())
}

fn file_ok_by_size(p: &Path, size: u64) -> bool { std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == size) }

async fn fetch_file(repo: &str, rev: &str, f: &FileSpec, dest: &Path, hosts: [&str; 2], counter: &AtomicU64, cancel: &AtomicBool) -> Result<()> {
    let mut err = anyhow!("下载失败");
    for host in hosts {
        let url = format!("https://{host}/{repo}/resolve/{rev}/{}", f.path);
        match fetch_once(&url, f, dest, counter, cancel).await {
            Ok(()) => return Ok(()),
            Err(e) if cancel.load(Ordering::Relaxed) => return Err(e),
            Err(e) => err = e,
        }
    }
    Err(anyhow!("下载 {} 失败：{}", f.path, err))
}

async fn fetch_once(url: &str, f: &FileSpec, dest: &Path, counter: &AtomicU64, cancel: &AtomicBool) -> Result<()> {
    let res = net::client().get(url).send().await?;
    if !res.status().is_success() { return Err(anyhow!("HTTP {}", res.status().as_u16())); }
    if let Some(dir) = dest.parent() { tokio::fs::create_dir_all(dir).await?; }
    let part = dest.with_file_name(format!("{}.part", dest.file_name().and_then(|n| n.to_str()).unwrap_or("file")));
    let mut file = tokio::fs::File::create(&part).await?;
    let mut sha256 = Sha256::new();
    let mut sha1 = Sha1::new();
    sha1.update(format!("blob {}\0", f.size).as_bytes());
    let mut got = 0u64;
    let mut stream = res.bytes_stream();
    let result: Result<()> = async {
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::Relaxed) { return Err(anyhow!("已取消")); }
            let chunk = chunk?;
            file.write_all(&chunk).await?;
            if f.sha256.is_some() { sha256.update(&chunk); } else { sha1.update(&chunk); }
            got += chunk.len() as u64;
            counter.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        }
        file.flush().await?;
        if got != f.size { return Err(anyhow!("文件大小不对")); }
        let ok = match (&f.sha256, &f.sha1) {
            (Some(want), _) => hex::encode(sha256.clone().finalize()) == *want,
            (None, Some(want)) => hex::encode(sha1.clone().finalize()) == *want,
            _ => true,
        };
        if !ok { return Err(anyhow!("文件校验失败")); }
        Ok(())
    }.await;
    drop(file);
    if let Err(e) = result {
        counter.fetch_sub(got, Ordering::Relaxed);
        let _ = tokio::fs::remove_file(&part).await;
        return Err(e);
    }
    tokio::fs::rename(&part, dest).await?;
    Ok(())
}

async fn install_runtime(data: &Path, asset: &str, sha: &str, cancel: &AtomicBool, progress: impl FnMut(u64, Option<u64>)) -> Result<()> {
    let name = format!("sherpa-onnx-{SHERPA_VERSION}-{asset}.tar.bz2");
    let archive = data.join("bin").join(&name);
    let url = format!("https://github.com/k2-fsa/sherpa-onnx/releases/download/{SHERPA_VERSION}/{name}");
    asr::download(&url, &archive, cancel, progress).await.map_err(|e| anyhow!("下载本地语音程序失败：{}", e))?;
    let dest = runtime_dir(data);
    let (a, d, want) = (archive.clone(), dest.clone(), sha.to_string());
    let r = tokio::task::spawn_blocking(move || -> Result<()> {
        let mut h = Sha256::new();
        std::io::copy(&mut std::fs::File::open(&a)?, &mut h)?;
        if hex::encode(h.finalize()) != want { return Err(anyhow!("本地语音程序校验失败")); }
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d)?;
        let mut tar = tar::Archive::new(bzip2::read::BzDecoder::new(std::fs::File::open(&a)?));
        for entry in tar.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            // Drop the top folder (sherpa-onnx-<ver>-<asset>/); keep only the TTS command and its libraries.
            let rel: PathBuf = path.components().skip(1).collect();
            let s = rel.to_string_lossy().replace('\\', "/");
            let keep = s == "bin/sherpa-onnx-offline-tts"
                || (s.starts_with("lib/") && (s.contains(".dylib") || s.contains(".so")));
            if keep {
                let to = d.join(&rel);
                if let Some(parent) = to.parent() { std::fs::create_dir_all(parent)?; }
                entry.unpack(&to)?;
            }
        }
        Ok(())
    }).await?;
    let _ = std::fs::remove_file(&archive);
    r?;
    let bin = runtime_bin(data);
    if !bin.is_file() { return Err(anyhow!("本地语音程序安装不完整")); }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Kokoro v1.1 speaker ids for the voices we offer (the model has 103; these are the clearest Mandarin ones).
fn kokoro_sid(voice: &str) -> u32 {
    match voice {
        "zf_001" => 3, "zf_002" => 4, "zf_003" => 5, "zf_004" => 6,
        "zm_009" => 58, "zm_010" => 59, "zm_011" => 60, "zm_012" => 61,
        _ => 3,
    }
}

/// Synthesises `text` with `model` ("melo" | "kokoro") and returns WAV bytes.
pub async fn synthesize(data: &Path, model: &str, voice: &str, text: &str, rate: f64) -> Result<Vec<u8>> {
    if !supported() { return Err(anyhow!("这个系统暂不支持本地语音模型，请改用 Edge 在线语音")); }
    if !runtime_ready(data) || !model_ready(data, model) {
        return Err(anyhow!("本地语音模型还没下载，请先在 设置 › 朗读 里下载"));
    }
    let dir = model_dir(data, model);
    let p = |f: &str| dir.join(f).to_string_lossy().into_owned();
    let threads = std::thread::available_parallelism().map(|n| n.get().clamp(1, 4)).unwrap_or(2);
    let out = std::env::temp_dir().join(format!("subcast-local-tts-{}.wav", uuid::Uuid::new_v4().simple()));
    let mut args: Vec<String> = match model {
        "melo" => vec![
            format!("--vits-model={}", p("model.onnx")),
            format!("--vits-tokens={}", p("tokens.txt")),
            format!("--vits-lexicon={}", p("lexicon.txt")),
            format!("--vits-dict-dir={}", p("dict")),
            format!("--tts-rule-fsts={},{}", p("date.fst"), p("number.fst")),
        ],
        "kokoro" => vec![
            format!("--kokoro-model={}", p("model.onnx")),
            format!("--kokoro-voices={}", p("voices.bin")),
            format!("--kokoro-tokens={}", p("tokens.txt")),
            format!("--kokoro-data-dir={}", p("espeak-ng-data")),
            format!("--kokoro-dict-dir={}", p("dict")),
            format!("--kokoro-lexicon={},{}", p("lexicon-us-en.txt"), p("lexicon-zh.txt")),
            format!("--tts-rule-fsts={},{}", p("date-zh.fst"), p("number-zh.fst")),
            format!("--sid={}", kokoro_sid(voice)),
        ],
        other => return Err(anyhow!("未知的本地语音模型：{}", other)),
    };
    args.push(format!("--num-threads={threads}"));
    args.push(format!("--speed={:.2}", rate.clamp(0.5, 2.0)));
    args.push(format!("--output-filename={}", out.to_string_lossy()));
    // A leading '-' would be read as a flag.
    args.push(text.trim_start_matches('-').replace('\n', " "));

    let res = tokio::process::Command::new(runtime_bin(data))
        .args(&args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output().await
        .map_err(|e| anyhow!("无法启动本地语音程序：{}", e))?;
    let bytes = tokio::fs::read(&out).await.unwrap_or_default();
    let _ = tokio::fs::remove_file(&out).await;
    if !res.status.success() || bytes.len() < 64 {
        let msg = String::from_utf8_lossy(&res.stderr);
        let last = msg.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").chars().take(160).collect::<String>();
        return Err(if last.is_empty() { anyhow!("本地语音合成失败") } else { anyhow!("本地语音合成失败：{}", last) });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_lists_both_models() {
        let melo = manifest("melo").unwrap();
        assert!(melo.files.iter().any(|f| f.path == "model.onnx" && f.sha256.is_some()));
        assert!(melo.files.iter().any(|f| f.path.starts_with("dict/")));
        let kokoro = manifest("kokoro").unwrap();
        assert!(kokoro.files.iter().any(|f| f.path == "voices.bin"));
        assert!(kokoro.files.iter().any(|f| f.path.starts_with("espeak-ng-data/")));
        assert!(kokoro.files.iter().all(|f| f.sha256.is_some() || f.sha1.is_some()));
        assert!(manifest("nope").is_err());
    }

    /// Downloads the runtime and both models into $SUBCAST_TTS_TEST_DIR and reads a line with each.
    /// `SUBCAST_TTS_TEST_DIR=/tmp/x cargo test --lib local_tts_end_to_end -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn local_tts_end_to_end() {
        let data = PathBuf::from(std::env::var("SUBCAST_TTS_TEST_DIR").expect("SUBCAST_TTS_TEST_DIR"));
        for (id, voice) in [("melo", "zh_en"), ("kokoro", "zm_009")] {
            let t0 = std::time::Instant::now();
            install(&data, id, Arc::new(AtomicBool::new(false)), true, |_, _| {}).await.unwrap();
            println!("{id}: installed in {:.1}s", t0.elapsed().as_secs_f64());
            assert!(status(&data).models.iter().any(|m| m.id == id && m.installed));
            let t1 = std::time::Instant::now();
            let wav = synthesize(&data, id, voice, "这是 OpenAI 的 CEO Sam Altman 在 2025 年的一次访谈。", 1.25).await.unwrap();
            println!("{id}: {} bytes, {:.2}s audio in {:.2}s", wav.len(), crate::speech::audio_duration(&wav, "wav"), t1.elapsed().as_secs_f64());
            std::fs::write(data.join(format!("{id}.wav")), &wav).unwrap();
        }
    }

    #[test]
    fn git_blob_hash_matches_git() {
        // `printf 'hello\n' | git hash-object --stdin`
        let mut h = Sha1::new();
        h.update(b"blob 6\0hello\n");
        assert_eq!(hex::encode(h.finalize()), "ce013625030ba8dba906f756967f9e9ca394464a");
    }
}
