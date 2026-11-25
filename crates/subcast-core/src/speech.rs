//! Multi-engine text-to-speech synthesis with on-disk caching (`<data>/tts-cache/`).
//!
//! Supports key-free engines (Microsoft Edge Read Aloud, offline Melo/Kokoro via sherpa-onnx, system voice) and cloud services
//! (Doubao, Xiaomi MiMo, Alibaba Qwen-TTS, OpenAI / SiliconFlow / OpenAI-compatible, MiniMax,
//! ElevenLabs). Audio files are written to disk and returned by path + duration so both
//! ReadAloud and future Chinese dubbing can play them via `convertFileSrc` without base64 bridging.
use crate::{cloud, edge_tts, llm, local_tts, net};
use anyhow::{anyhow, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

pub const CACHE_LIMIT: u64 = 500 * 1024 * 1024;
const DOUBAO_ENDPOINT: &str = "https://openspeech.bytedance.com/api/v3/tts/unidirectional";
const DOUBAO_RATE: u32 = 24_000;

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TtsRequest {
    /// Service id (e.g. "edge", "local", "system", "doubao", "mimo", "qwen", "openai", "siliconflow", "minimax", "elevenlabs", "custom").
    pub engine: String,
    /// Wire protocol: "edge" | "system" | "local" | "doubao" | "mimo" | "openai" | "qwen" | "minimax" | "elevenlabs" | "deepseek".
    pub protocol: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub voice: String,
    #[serde(default)]
    pub resource: Option<String>,
    pub text: String,
    #[serde(default)]
    pub rate: Option<f64>,
    #[serde(default)]
    pub needs_key: Option<bool>,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TtsResult {
    pub path: String,
    pub seconds: f64,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SystemVoice {
    pub id: String,
    pub name: String,
    pub lang: String,
}

pub fn cache_dir(data: &Path) -> PathBuf { data.join("tts-cache") }

pub fn clear_cache(data: &Path) -> Result<()> {
    let dir = cache_dir(data);
    if dir.exists() { std::fs::remove_dir_all(&dir)?; }
    Ok(())
}

fn cache_key(req: &TtsRequest, text: &str, rate: f64) -> String {
    let mut h = Sha256::new();
    let base = req.base_url.as_deref().unwrap_or("").trim();
    let model = req.model.as_deref().unwrap_or("").trim();
    let res = req.resource.as_deref().unwrap_or("").trim();
    let voice = req.voice.trim();
    let raw = format!("v1|{}|{}|{}|{}|{}|{}|{:.2}|{}", req.engine, req.protocol, base, model, res, voice, rate, text);
    h.update(raw.as_bytes());
    hex::encode(&h.finalize()[..12])
}

fn find_cached(dir: &Path, key: &str) -> Option<PathBuf> {
    for ext in ["mp3", "wav"] {
        let p = dir.join(format!("{key}.{ext}"));
        if p.is_file() && std::fs::metadata(&p).map(|m| m.len() > 64).unwrap_or(false) {
            if let Ok(f) = std::fs::File::options().append(true).open(&p) {
                let _ = f.set_modified(SystemTime::now());
            }
            return Some(p);
        }
    }
    None
}

fn trim_cache(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(PathBuf, u64, SystemTime)> = rd.flatten().filter_map(|e| {
        let p = e.path();
        if !p.is_file() { return None; }
        let m = std::fs::metadata(&p).ok()?;
        let t = m.modified().unwrap_or(SystemTime::UNIX_EPOCH);
        Some((p, m.len(), t))
    }).collect();
    let mut total: u64 = files.iter().map(|(_, len, _)| *len).sum();
    if total <= CACHE_LIMIT { return; }
    files.sort_by_key(|(_, _, t)| *t);
    for (p, len, _) in files {
        if total <= CACHE_LIMIT * 4 / 5 { break; }
        if std::fs::remove_file(&p).is_ok() { total = total.saturating_sub(len); }
    }
}

/// Computes audio duration in seconds from WAV header or symphonia probe.
pub fn audio_duration(bytes: &[u8], ext: &str) -> f64 {
    if ext == "wav" || bytes.starts_with(b"RIFF") {
        if let Some(secs) = wav_duration(bytes) { return secs; }
    }
    if let Some(secs) = symphonia_duration(bytes, ext) { return secs; }
    0.0
}

fn wav_duration(b: &[u8]) -> Option<f64> {
    if b.len() < 44 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" { return None; }
    let mut pos = 12usize;
    let mut byte_rate = 0u32;
    let mut data_len = 0u32;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let size = u32::from_le_bytes([b[pos + 4], b[pos + 5], b[pos + 6], b[pos + 7]]);
        pos += 8;
        if id == b"fmt " && pos + 12 <= b.len() {
            byte_rate = u32::from_le_bytes([b[pos + 8], b[pos + 9], b[pos + 10], b[pos + 11]]);
        } else if id == b"data" {
            data_len = size.min((b.len().saturating_sub(pos)) as u32);
            break;
        }
        pos = pos.saturating_add(size as usize);
        if size % 2 == 1 { pos = pos.saturating_add(1); }
    }
    if byte_rate > 0 && data_len > 0 { Some(data_len as f64 / byte_rate as f64) } else { None }
}

fn symphonia_duration(bytes: &[u8], ext: &str) -> Option<f64> {
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let cursor = std::io::Cursor::new(bytes.to_vec());
    let mss = MediaSourceStream::new(Box::new(cursor), Default::default());
    let mut hint = Hint::new();
    if !ext.is_empty() { hint.with_extension(ext); }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;
    let mut format = probed.format;
    let track = format.default_track()?;
    if let (Some(n_frames), Some(rate)) = (track.codec_params.n_frames, track.codec_params.sample_rate) {
        if n_frames > 0 && rate > 0 { return Some(n_frames as f64 / rate as f64); }
    }
    let track_id = track.id;
    let mut dec = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).ok()?;
    let mut total_frames = 0u64;
    let mut rate = track.codec_params.sample_rate.unwrap_or(0);
    while let Ok(pkt) = format.next_packet() {
        if pkt.track_id() != track_id { continue; }
        if let Ok(buf) = dec.decode(&pkt) {
            total_frames += buf.frames() as u64;
            if rate == 0 { rate = buf.spec().rate; }
        }
    }
    if total_frames > 0 && rate > 0 { Some(total_frames as f64 / rate as f64) } else { None }
}

fn detect_ext(bytes: &[u8], fallback: &'static str) -> &'static str {
    if bytes.starts_with(b"RIFF") { "wav" } else { fallback }
}

/// Resolves an API key for a TTS service, falling back to the matching AI/ASR keychain entry.
fn resolve_key(engine: &str, protocol: &str) -> Option<String> {
    let primary = format!("tts.{engine}");
    if let Some(k) = llm::get_key(&primary).filter(|k| !k.trim().is_empty()) {
        return Some(k.trim().to_string());
    }
    let fallbacks: &[&str] = match (engine, protocol) {
        ("doubao", _) | (_, "doubao") => &["asr.doubao"],
        ("mimo", _) | (_, "mimo") => &["mimo"],
        ("qwen", _) | (_, "qwen") => &["qwen-cn"],
        ("minimax", _) | (_, "minimax") => &["minimax-cn"],
        ("openai", _) => &["openai"],
        ("siliconflow", _) => &["siliconflow", "asr.siliconflow"],
        ("deepseek", _) | (_, "deepseek") => &["deepseek", "asr.deepseek"],
        _ => &[],
    };
    for acct in fallbacks {
        if let Some(k) = llm::get_key(acct).filter(|k| !k.trim().is_empty()) {
            return Some(k.trim().to_string());
        }
    }
    None
}

fn require_key(req: &TtsRequest) -> Result<String> {
    if let Some(k) = resolve_key(&req.engine, &req.protocol) {
        return Ok(k);
    }
    if req.needs_key == Some(false) {
        return Ok(String::new());
    }
    Err(anyhow!("请先在设置里填写这个朗读服务的 API Key"))
}

fn http_err(status: u16, body: &str) -> anyhow::Error {
    let detail = serde_json::from_str::<Value>(body).ok()
        .and_then(|v| v["error"]["message"].as_str().or_else(|| v["error"].as_str()).or_else(|| v["message"].as_str()).map(String::from))
        .unwrap_or_else(|| body.chars().take(180).collect());
    match status {
        401 | 403 => anyhow!("朗读服务拒绝了请求：API Key 无效或没有权限（{detail}）"),
        429 => anyhow!("朗读服务请求太频繁或额度不足（{detail}）"),
        _ => anyhow!("朗读服务出错（HTTP {status}）：{detail}"),
    }
}

// ---- Main entry point ----------------------------------------------------------------------------

pub async fn synthesize(data: &Path, req: TtsRequest) -> Result<TtsResult> {
    let text = req.text.trim();
    if text.is_empty() { return Err(anyhow!("没有可朗读的文字")); }
    let rate = req.rate.unwrap_or(1.0).clamp(0.5, 2.0);
    let dir = cache_dir(data);
    let key = cache_key(&req, text, rate);
    if let Some(cached) = find_cached(&dir, &key) {
        let ext = cached.extension().and_then(|e| e.to_str()).unwrap_or("mp3");
        let bytes = std::fs::read(&cached).unwrap_or_default();
        let seconds = audio_duration(&bytes, ext);
        return Ok(TtsResult { path: cached.to_string_lossy().into_owned(), seconds });
    }

    let (bytes, ext) = match req.protocol.as_str() {
        "edge" => (edge_tts::synthesize(&req.voice, text, rate).await?, "mp3"),
        "system" => (system_tts(&req.voice, text, rate).await?, "wav"),
        "local" => (local_tts::synthesize(data, req.model.as_deref().unwrap_or("melo"), &req.voice, text, rate).await?, "wav"),
        "doubao" => (doubao_tts(&req, text, rate).await?, "wav"),
        "mimo" => (mimo_tts(&req, text, rate).await?, "wav"),
        "openai" => openai_tts(&req, text, rate).await?,
        "qwen" => qwen_tts(&req, text).await?,
        "minimax" => (minimax_tts(&req, text, rate).await?, "mp3"),
        "elevenlabs" => (elevenlabs_tts(&req, text, rate).await?, "mp3"),
        "deepseek" => (deepseek_tts(&req, text).await?, "wav"),
        other => return Err(anyhow!("不支持的朗读引擎：{other}")),
    };
    if bytes.len() < 64 { return Err(anyhow!("朗读服务没有返回有效音频")); }
    let ext = detect_ext(&bytes, ext);
    let seconds = audio_duration(&bytes, ext);

    tokio::fs::create_dir_all(&dir).await?;
    let final_path = dir.join(format!("{key}.{ext}"));
    let tmp_path = dir.join(format!("{key}.{}.part", uuid::Uuid::new_v4().simple()));
    tokio::fs::write(&tmp_path, &bytes).await?;
    let _ = tokio::fs::rename(&tmp_path, &final_path).await;
    let dir_bg = dir.clone();
    tokio::task::spawn_blocking(move || trim_cache(&dir_bg));

    Ok(TtsResult { path: final_path.to_string_lossy().into_owned(), seconds })
}

// ---- Engine implementations ----------------------------------------------------------------------

async fn doubao_tts(req: &TtsRequest, text: &str, rate: f64) -> Result<Vec<u8>> {
    let key = resolve_key("doubao", "doubao")
        .ok_or_else(|| anyhow!("请先在设置里填写豆包语音的 API Key"))?;
    if text.chars().count() > 1000 { return Err(anyhow!("每次朗读 1–1000 个字")); }
    let endpoint = req.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or(DOUBAO_ENDPOINT);
    let resource = req.resource.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("seed-tts-2.0");
    let voice = if req.voice.trim().is_empty() { "zh_female_vv_uranus_bigtts" } else { req.voice.trim() };
    let speech_rate = ((rate - 1.0) * 100.0).round().clamp(-50.0, 100.0) as i32;
    let resp = net::client().post(endpoint)
        .header("X-Api-Key", key)
        .header("X-Api-Resource-Id", resource)
        .header("X-Api-Request-Id", uuid::Uuid::new_v4().to_string())
        .json(&json!({
            "req_params": {
                "text": text,
                "speaker": voice,
                "audio_params": { "format": "pcm", "sample_rate": DOUBAO_RATE, "speech_rate": speech_rate }
            }
        }))
        .timeout(Duration::from_secs(120))
        .send().await.map_err(|_| anyhow!("连不上豆包语音服务"))?;
    if !resp.status().is_success() {
        return Err(anyhow!("豆包语音合成返回 HTTP {}（检查 Key 与音色）", resp.status()));
    }
    let body = resp.text().await?;
    let mut pcm = Vec::new();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).map_err(|_| anyhow!("豆包返回了无法解析的音频"))?;
        let code = v["code"].as_i64().unwrap_or(0);
        if code != 0 && code != 20_000_000 {
            return Err(anyhow!("豆包拒绝了合成请求（{code}）：{}", v["message"].as_str().unwrap_or("检查 Key、模型和音色")));
        }
        if let Some(d) = v["data"].as_str() {
            base64::engine::general_purpose::STANDARD.decode_vec(d, &mut pcm).map_err(|_| anyhow!("音频数据损坏"))?;
        }
    }
    if pcm.len() < 2 { return Err(anyhow!("豆包没有返回音频")); }
    let samples: Vec<f32> = pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    Ok(cloud::wav_bytes(&samples, DOUBAO_RATE))
}

async fn mimo_tts(req: &TtsRequest, text: &str, rate: f64) -> Result<Vec<u8>> {
    let key = require_key(req)?;
    let base = req.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty())
        .unwrap_or("https://api.xiaomimimo.com/v1").trim_end_matches('/');
    let url = if base.ends_with("/chat/completions") { base.to_string() } else { format!("{base}/chat/completions") };
    let model = req.model.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("mimo-v2.5-tts");
    let voice = if req.voice.trim().is_empty() { "mimo_default" } else { req.voice.trim() };
    // Natural-language pace direction sent to MiMo (not user-facing UI text).
    let pace = if (rate - 1.0).abs() <= 0.05 {
        "正常语速，自然清晰朗读。".to_string() // i18n-ignore
    } else {
        format!("语速调整为正常的 {rate:.2} 倍左右，保持清晰自然朗读。") // i18n-ignore
    };
    let resp = net::client().post(&url)
        .header("api-key", &key)
        .json(&json!({
            "model": model,
            "messages": [
                { "role": "user", "content": pace },
                { "role": "assistant", "content": text }
            ],
            "audio": { "format": "wav", "voice": voice }
        }))
        .timeout(Duration::from_secs(120))
        .send().await.map_err(net::describe)?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() { return Err(http_err(status.as_u16(), &body)); }
    let v: Value = serde_json::from_str(&body).map_err(|_| anyhow!("小米 MiMo 返回了无法解析的响应"))?;
    let b64 = v["choices"][0]["message"]["audio"]["data"].as_str()
        .ok_or_else(|| anyhow!("小米 MiMo 没有返回音频数据"))?;
    base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| anyhow!("音频数据损坏"))
}

const SILICONFLOW_PRESETS: &[&str] = &["alex", "anna", "bella", "benjamin", "charles", "claire", "david", "diana"];

async fn openai_tts(req: &TtsRequest, text: &str, rate: f64) -> Result<(Vec<u8>, &'static str)> {
    let key = require_key(req)?;
    let base = req.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("请先在设置里填写朗读服务的接口地址"))?
        .trim_end_matches('/');
    let url = if base.ends_with("/audio/speech") { base.to_string() } else { format!("{base}/audio/speech") };
    let model = req.model.as_deref().map(str::trim).unwrap_or("");
    let raw_voice = req.voice.trim();
    let voice = if req.engine == "siliconflow"
        && !raw_voice.contains(':')
        && !model.is_empty()
        && SILICONFLOW_PRESETS.contains(&raw_voice)
    {
        format!("{model}:{raw_voice}")
    } else if raw_voice.is_empty() {
        "alloy".to_string()
    } else {
        raw_voice.to_string()
    };
    let mut payload = json!({
        "model": if model.is_empty() { "tts-1" } else { model },
        "input": text,
        "voice": voice,
        "response_format": "mp3",
        "speed": rate.clamp(0.25, 4.0)
    });
    if req.engine == "siliconflow" {
        payload["sample_rate"] = json!(32000);
    }
    let mut builder = net::client().post(&url).json(&payload).timeout(Duration::from_secs(120));
    if !key.is_empty() { builder = builder.bearer_auth(&key); }
    let resp = builder.send().await.map_err(net::describe)?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(http_err(status.as_u16(), &body));
    }
    let bytes = resp.bytes().await.map_err(net::describe)?.to_vec();
    let ext = detect_ext(&bytes, "mp3");
    Ok((bytes, ext))
}

async fn qwen_tts(req: &TtsRequest, text: &str) -> Result<(Vec<u8>, &'static str)> {
    let key = require_key(req)?;
    let base = req.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty())
        .unwrap_or("https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation");
    let model = req.model.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("qwen3-tts-flash");
    let voice = if req.voice.trim().is_empty() { "Cherry" } else { req.voice.trim() };
    let resp = net::client().post(base)
        .bearer_auth(&key)
        .json(&json!({
            "model": model,
            "input": { "text": text, "voice": voice, "language_type": "Auto" }
        }))
        .timeout(Duration::from_secs(120))
        .send().await.map_err(net::describe)?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() { return Err(http_err(status.as_u16(), &body)); }
    let v: Value = serde_json::from_str(&body).map_err(|_| anyhow!("通义语音返回了无法解析的响应"))?;
    if let Some(data) = v["output"]["audio"]["data"].as_str().filter(|s| !s.is_empty()) {
        let bytes = base64::engine::general_purpose::STANDARD.decode(data).map_err(|_| anyhow!("音频数据损坏"))?;
        return Ok((bytes, "wav"));
    }
    let url = v["output"]["audio"]["url"].as_str().filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("通义语音没有返回音频地址"))?;
    let dl = net::client().get(url).timeout(Duration::from_secs(60)).send().await.map_err(net::describe)?;
    if !dl.status().is_success() { return Err(anyhow!("下载通义语音音频失败（HTTP {}）", dl.status())); }
    let bytes = dl.bytes().await.map_err(net::describe)?.to_vec();
    let ext = detect_ext(&bytes, "wav");
    Ok((bytes, ext))
}

async fn minimax_tts(req: &TtsRequest, text: &str, rate: f64) -> Result<Vec<u8>> {
    let key = require_key(req)?;
    let base = req.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty())
        .unwrap_or("https://api.minimaxi.com/v1").trim_end_matches('/');
    let url = if base.ends_with("/t2a_v2") { base.to_string() } else { format!("{base}/t2a_v2") };
    let model = req.model.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("speech-02-hd");
    let voice = if req.voice.trim().is_empty() { "male-qn-qingse" } else { req.voice.trim() };
    let resp = net::client().post(&url)
        .bearer_auth(&key)
        .json(&json!({
            "model": model,
            "text": text,
            "stream": false,
            "output_format": "hex",
            "voice_setting": { "voice_id": voice, "speed": rate.clamp(0.5, 2.0), "vol": 1.0, "pitch": 0 },
            "audio_setting": { "sample_rate": 32000, "bitrate": 128000, "format": "mp3", "channel": 1 }
        }))
        .timeout(Duration::from_secs(120))
        .send().await.map_err(net::describe)?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() { return Err(http_err(status.as_u16(), &body)); }
    let v: Value = serde_json::from_str(&body).map_err(|_| anyhow!("MiniMax 返回了无法解析的响应"))?;
    let code = v["base_resp"]["status_code"].as_i64().unwrap_or(0);
    if code != 0 {
        let msg = v["base_resp"]["status_msg"].as_str().unwrap_or("未知错误");
        return Err(anyhow!("MiniMax 合成失败（{code}）：{msg}"));
    }
    let hex_audio = v["data"]["audio"].as_str().filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("MiniMax 没有返回音频数据"))?;
    hex::decode(hex_audio).map_err(|_| anyhow!("音频数据损坏"))
}

async fn elevenlabs_tts(req: &TtsRequest, text: &str, rate: f64) -> Result<Vec<u8>> {
    let key = require_key(req)?;
    let base = req.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty())
        .unwrap_or("https://api.elevenlabs.io/v1").trim_end_matches('/');
    let voice = if req.voice.trim().is_empty() { "JBFqnCBsd6RMkjVDRZzb" } else { req.voice.trim() };
    let model = req.model.as_deref().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("eleven_multilingual_v2");
    let url = format!("{base}/text-to-speech/{}", urlencoding::encode(voice));
    let resp = net::client().post(&url)
        .header("xi-api-key", &key)
        .header("Accept", "audio/mpeg")
        .json(&json!({
            "text": text,
            "model_id": model,
            "voice_settings": { "stability": 0.5, "similarity_boost": 0.75, "speed": rate.clamp(0.7, 1.2) }
        }))
        .timeout(Duration::from_secs(120))
        .send().await.map_err(net::describe)?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(http_err(status.as_u16(), &body));
    }
    Ok(resp.bytes().await.map_err(net::describe)?.to_vec())
}

/// DeepSeek text to speech (https://api-docs.deepseek.com/guides/tts): WAV streamed in the body. It has no speed
/// setting, so the app asks for 1× and speeds playback up instead.
async fn deepseek_tts(req: &TtsRequest, text: &str) -> Result<Vec<u8>> {
    let key = require_key(req)?;
    let base = req.base_url.as_deref().map(str::trim).filter(|s| !s.is_empty())
        .unwrap_or("https://api.deepseek.com").trim_end_matches('/');
    let url = if base.ends_with("/tools/tts") { base.to_string() } else { format!("{base}/v1/tools/tts") };
    let voice = if req.voice.trim().is_empty() { "stella" } else { req.voice.trim() };
    let resp = net::client().post(&url)
        .bearer_auth(&key)
        .json(&json!({ "text": text, "voice_id": voice, "format": "wav", "filter_markdown": true }))
        .timeout(Duration::from_secs(120))
        .send().await.map_err(net::describe)?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        if status.as_u16() == 400 && body.contains("Content Exists Risk") { return Err(anyhow!("DeepSeek 拒绝朗读这段文字（内容审核未通过）")); }
        return Err(http_err(status.as_u16(), &body));
    }
    let mut bytes = resp.bytes().await.map_err(net::describe)?.to_vec();
    fix_streamed_wav(&mut bytes);
    Ok(bytes)
}

/// A WAV streamed before its length was known says 0xFFFFFFFF for the RIFF and data sizes; write the real ones so
/// players and the duration estimate read it as an ordinary file.
fn fix_streamed_wav(b: &mut [u8]) {
    if b.len() < 12 || !b.starts_with(b"RIFF") || &b[8..12] != b"WAVE" { return; }
    let mut i = 12;
    while i + 8 <= b.len() {
        let size = u32::from_le_bytes([b[i + 4], b[i + 5], b[i + 6], b[i + 7]]);
        if &b[i..i + 4] == b"data" {
            if size == u32::MAX || size == 0 || i + 8 + size as usize > b.len() {
                let data = ((b.len() - i - 8) & !1) as u32;
                b[i + 4..i + 8].copy_from_slice(&data.to_le_bytes());
                let riff = (b.len() - 8) as u32;
                b[4..8].copy_from_slice(&riff.to_le_bytes());
            }
            return;
        }
        i += 8 + size as usize + (size as usize & 1);
    }
}

// ---- System TTS (macOS say / Windows SAPI / Linux espeak-ng) -------------------------------------

async fn system_tts(voice: &str, text: &str, rate: f64) -> Result<Vec<u8>> {
    let voice = voice.trim().to_string();
    let text = text.to_string();
    tokio::task::spawn_blocking(move || system_tts_sync(&voice, &text, rate))
        .await.map_err(|e| anyhow!("系统语音任务中断：{e}"))?
}

#[cfg(target_os = "macos")]
const NOVELTY_VOICES: &[&str] = &[
    "Albert", "Bad News", "Bahh", "Bells", "Boing", "Bubbles", "Cellos", "Good News",
    "Jester", "Junior", "Kathy", "Organ", "Ralph", "Superstar", "Trinoids", "Whisper", "Wobble", "Zarvox",
];

#[cfg(target_os = "macos")]
fn pick_macos_default_voice(text: &str) -> Option<String> {
    let has_cjk = text.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c));
    let voices = list_system_voices();
    let matches_lang = |v: &SystemVoice| {
        if has_cjk { v.lang.starts_with("zh") } else { v.lang.starts_with("en") }
    };
    // Prefer Enhanced/Premium voices first, then Tingting / Samantha.
    for needle in ["Premium", "Enhanced", if has_cjk { "Tingting" } else { "Samantha" }] {
        if let Some(v) = voices.iter().find(|v| matches_lang(v) && v.name.contains(needle)) {
            return Some(v.id.clone());
        }
    }
    voices.into_iter().find(matches_lang).map(|v| v.id)
}

#[cfg(target_os = "macos")]
fn system_tts_sync(voice: &str, text: &str, rate: f64) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let tmp = std::env::temp_dir().join(format!("subcast-say-{}.wav", uuid::Uuid::new_v4().simple()));
    let wpm = (180.0 * rate.clamp(0.5, 2.0)).round() as u32;
    let resolved = if !voice.is_empty() { Some(voice.to_string()) } else { pick_macos_default_voice(text) };
    let mut cmd = Command::new("/usr/bin/say");
    if let Some(ref v) = resolved { cmd.args(["-v", v]); }
    cmd.args(["-r", &wpm.to_string(), "-o"]).arg(&tmp)
        .args(["--data-format=LEI16@24000"])
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|_| anyhow!("无法启动 macOS 系统语音"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(anyhow!("系统语音合成失败：{}", if msg.is_empty() { "请更换系统音色".into() } else { msg }));
    }
    let bytes = std::fs::read(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(bytes)
}

#[cfg(windows)]
fn system_tts_sync(voice: &str, text: &str, rate: f64) -> Result<Vec<u8>> {
    use std::os::windows::process::CommandExt;
    use std::process::Command;
    let tmp = std::env::temp_dir().join(format!("subcast-sapi-{}.wav", uuid::Uuid::new_v4().simple()));
    let txt_file = std::env::temp_dir().join(format!("subcast-sapi-{}.txt", uuid::Uuid::new_v4().simple()));
    std::fs::write(&txt_file, text)?;
    let sapi_rate = ((rate - 1.0) * 6.0).round().clamp(-10.0, 10.0) as i32;
    let script = format!(
        "Add-Type -AssemblyName System.Speech; \
         $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
         $s.Rate = {sapi_rate}; \
         if ($env:SC_VOICE) {{ try {{ $s.SelectVoice($env:SC_VOICE) }} catch {{}} }}; \
         $s.SetOutputToWaveFile($env:SC_OUT); \
         $t = [System.IO.File]::ReadAllText($env:SC_TXT, [System.Text.Encoding]::UTF8); \
         $s.Speak($t); $s.Dispose()"
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .env("SC_VOICE", voice)
        .env("SC_OUT", &tmp)
        .env("SC_TXT", &txt_file)
        .creation_flags(0x08000000)
        .output();
    let _ = std::fs::remove_file(&txt_file);
    let out = out.map_err(|_| anyhow!("无法启动 Windows 系统语音"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("系统语音合成失败，请切换 Edge 或其他引擎"));
    }
    let bytes = std::fs::read(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(bytes)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn system_tts_sync(voice: &str, text: &str, rate: f64) -> Result<Vec<u8>> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let tmp = std::env::temp_dir().join(format!("subcast-espeak-{}.wav", uuid::Uuid::new_v4().simple()));
    let wpm = (160.0 * rate.clamp(0.5, 2.0)).round() as u32;
    let bin = if Command::new("espeak-ng").arg("--version").output().is_ok() { "espeak-ng" } else { "espeak" };
    let mut cmd = Command::new(bin);
    cmd.args(["-v", if voice.is_empty() { "cmn" } else { voice }, "-s", &wpm.to_string(), "-w"]).arg(&tmp)
        .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|_| anyhow!("未找到系统语音工具（espeak-ng），建议使用 Edge 在线语音"))?;
    if let Some(mut stdin) = child.stdin.take() { let _ = stdin.write_all(text.as_bytes()); }
    let out = child.wait_with_output()?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&tmp);
        return Err(anyhow!("系统语音合成失败，建议使用 Edge 在线语音"));
    }
    let bytes = std::fs::read(&tmp)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(bytes)
}

/// Lists available OS voices (Chinese and English first) for the System TTS picker.
pub fn list_system_voices() -> Vec<SystemVoice> {
    #[cfg(target_os = "macos")]
    {
        let Ok(out) = std::process::Command::new("/usr/bin/say").args(["-v", "?"]).output() else { return vec![] };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut voices = Vec::new();
        for line in text.lines() {
            let Some((left, _)) = line.split_once('#') else { continue };
            let trimmed = left.trim_end();
            let Some(pos) = trimmed.rfind(|c: char| c.is_whitespace()) else { continue };
            let lang = trimmed[pos..].trim();
            let name = trimmed[..pos].trim();
            if name.is_empty() || lang.is_empty() || NOVELTY_VOICES.iter().any(|n| name == *n) { continue; }
            if lang.starts_with("zh") || lang.starts_with("en") || lang.starts_with("ja") {
                voices.push(SystemVoice { id: name.to_string(), name: name.to_string(), lang: lang.to_string() });
            }
        }
        let rank = |v: &SystemVoice| {
            let lang_rank = if v.lang.starts_with("zh_CN") { 0 } else if v.lang.starts_with("zh") { 1 } else if v.lang.starts_with("en") { 2 } else { 3 };
            let quality_rank = if v.name.contains("Premium") { 0 } else if v.name.contains("Enhanced") { 1 } else if v.name.contains("Tingting") || v.name.contains("Samantha") || v.name.contains("Meijia") || v.name.contains("Sinji") || v.name.contains("Daniel") { 2 } else { 3 };
            (lang_rank, quality_rank, v.name.clone())
        };
        voices.sort_by_key(rank);
        voices.dedup_by(|a, b| a.id == b.id);
        voices
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let script = "Add-Type -AssemblyName System.Speech; \
            (New-Object System.Speech.Synthesis.SpeechSynthesizer).GetInstalledVoices() | \
            ForEach-Object { \"$($_.VoiceInfo.Name)`t$($_.VoiceInfo.Culture.Name)\" }";
        let Ok(out) = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(0x08000000)
            .output() else { return vec![] };
        let mut voices: Vec<SystemVoice> = String::from_utf8_lossy(&out.stdout).lines().filter_map(|l| {
            let (name, lang) = l.split_once('\t')?;
            let (name, lang) = (name.trim(), lang.trim());
            (!name.is_empty()).then(|| SystemVoice { id: name.to_string(), name: name.to_string(), lang: lang.to_string() })
        }).collect();
        voices.sort_by_key(|v| (if v.lang.starts_with("zh") { 0 } else if v.lang.starts_with("en") { 1 } else { 2 }, v.name.clone()));
        voices
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        vec![
            SystemVoice { id: "cmn".into(), name: "Mandarin (cmn)".into(), lang: "zh-CN".into() },
            SystemVoice { id: "en".into(), name: "English (en)".into(), lang: "en-US".into() },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_duration_matches_samples() {
        let samples = vec![0.0f32; 24_000 * 3 / 2]; // 1.5 s at 24 kHz
        let wav = cloud::wav_bytes(&samples, 24_000);
        let secs = audio_duration(&wav, "wav");
        assert!((secs - 1.5).abs() < 0.01, "got {secs}");
    }

    #[test]
    fn cache_key_varies_by_engine_voice_and_rate() {
        let base = TtsRequest {
            engine: "edge".into(),
            protocol: "edge".into(),
            base_url: None,
            model: None,
            voice: "zh-CN-YunxiNeural".into(),
            resource: None,
            text: "你好".into(),
            rate: Some(1.0),
            needs_key: Some(false),
        };
        let k1 = cache_key(&base, "你好", 1.0);
        let k2 = cache_key(&base, "你好", 1.25);
        let mut other = base.clone();
        other.voice = "zh-CN-XiaoxiaoNeural".into();
        let k3 = cache_key(&other, "你好", 1.0);
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
        assert_eq!(k1.len(), 24);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_lists_system_voices() {
        let voices = list_system_voices();
        assert!(!voices.is_empty());
    }
}

#[cfg(test)]
mod deepseek_wav_tests {
    #[test]
    fn streamed_wav_gets_real_sizes() {
        let mut b = b"RIFF\xff\xff\xff\xffWAVEfmt \x10\0\0\0\x01\0\x01\0\xc0\x5d\0\0\x80\xbb\0\0\x02\0\x10\0data\xff\xff\xff\xff".to_vec();
        b.extend_from_slice(&[0u8; 100]);
        super::fix_streamed_wav(&mut b);
        assert_eq!(u32::from_le_bytes([b[4], b[5], b[6], b[7]]) as usize, b.len() - 8);
        assert_eq!(u32::from_le_bytes([b[40], b[41], b[42], b[43]]), 100);
        let before = b.clone();
        super::fix_streamed_wav(&mut b);
        assert_eq!(b, before);
    }
}
