//! Cloud speech services ported from Capwise: transcription (Doubao 极速版 and OpenAI-compatible
//! `/audio/transcriptions` services) and Doubao text-to-speech. Keys live in the keychain as `asr.<id>` / `tts.doubao`.
use crate::model::Segment;
use crate::net;
use anyhow::{anyhow, Result};
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const RATE: u32 = 16_000;

#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct CloudAsr {
    pub id: String,
    /// "doubao" | "openai"
    pub protocol: String,
    pub base_url: String,
    pub model: String,
    #[serde(default)]
    pub chunk_secs: Option<f64>,
    #[serde(default)]
    pub needs_key: Option<bool>,
}

pub fn wav_bytes(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples { out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()); }
    out
}

fn client() -> reqwest::Client { net::client() }

async fn recognize_chunk(cfg: &CloudAsr, key: &str, wav: Vec<u8>, offset: f64, len: f64, language: &str, ctx: &crate::asr::AsrContext) -> Result<Vec<Segment>> {
    let base = cfg.base_url.trim().trim_end_matches('/');
    let mut last = String::new();
    for attempt in 0..3 {
        let res = if cfg.protocol == "deepseek" {
            // DeepSeek ASR (https://api-docs.deepseek.com/guides/asr): one WAV of ≤120 s / 8 MiB per request.
            let part = reqwest::multipart::Part::bytes(wav.clone()).file_name("input.wav").mime_str("audio/wav")?;
            client().post(format!("{base}/v1/tools/asr")).bearer_auth(key).multipart(reqwest::multipart::Form::new().part("file", part))
                .timeout(std::time::Duration::from_secs(180)).send().await
        } else if cfg.protocol == "doubao" {
            let audio = base64::engine::general_purpose::STANDARD.encode(&wav);
            client().post(format!("{base}/auc/bigmodel/recognize/flash"))
                .header("X-Api-Key", key)
                .header("X-Api-Resource-Id", "volc.bigasr.auc_turbo")
                .header("X-Api-Request-Id", uuid::Uuid::new_v4().to_string())
                .header("X-Api-Sequence", "-1")
                .json(&{
                    let mut body = json!({ "user": { "uid": "subcast" }, "audio": { "data": audio }, "request": { "model_name": if cfg.model.is_empty() { "bigmodel" } else { cfg.model.as_str() }, "enable_itn": true, "enable_punc": true, "show_utterances": true } });
                    // The show notes and glossary terms, as Doubao's context (soft guidance for names and terms).
                    if !ctx.doubao.is_empty() { body["request"]["corpus"] = json!({ "context": ctx.doubao }); }
                    body
                })
                .timeout(std::time::Duration::from_secs(180)).send().await
        } else {
            let part = reqwest::multipart::Part::bytes(wav.clone()).file_name("chunk.wav").mime_str("audio/wav")?;
            let mut form = reqwest::multipart::Form::new().text("model", cfg.model.clone());
            // SiliconFlow documents only model and file for this endpoint, returning text without timestamps.
            if cfg.id != "siliconflow" {
                form = form.text("response_format", "verbose_json").text("timestamp_granularities[]", "segment");
                if language != "auto" { form = form.text("language", language.to_string()); }
            }
            // Only services seen to use it (OpenAI, Groq); others ignore it or reject extra text.
            if !ctx.prompt.is_empty() && (base.contains("openai.com") || base.contains("groq.com")) { form = form.text("prompt", ctx.prompt.clone()); }
            let b = client().post(format!("{base}/audio/transcriptions")).multipart(form.part("file", part)).timeout(std::time::Duration::from_secs(180));
            (if key.is_empty() { b } else { b.bearer_auth(key) }).send().await
        };
        match res {
            Err(err) => { last = err.to_string(); }
            Ok(r) => {
                let status = r.status();
                let code = r.headers().get("X-Api-Status-Code").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                let msg = r.headers().get("X-Api-Message").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                let body = r.text().await.unwrap_or_default();
                if !status.is_success() {
                    let detail: String = body.chars().take(200).collect();
                    if status.as_u16() == 401 || status.as_u16() == 403 { return Err(anyhow!("识别服务拒绝了请求：Key 无效或没有权限（{detail}）")); }
                    if status.is_client_error() && status.as_u16() != 429 { return Err(anyhow!("识别服务出错（{status}）：{detail}")); }
                    last = format!("{status} {detail}");
                } else if cfg.protocol == "deepseek" {
                    let v: Value = serde_json::from_str(&body).map_err(|_| anyhow!("DeepSeek 返回了无法解析的结果"))?;
                    if let Some(err) = v.get("error") { return Err(anyhow!("DeepSeek 识别失败：{}", err["message"].as_str().unwrap_or(&err.to_string()))); }
                    let text = v["text"].as_str().unwrap_or("").trim().to_string();
                    // Text only: spread over the piece, sentence by sentence.
                    return Ok(if text.is_empty() { vec![] } else { crate::transcript::split_timed(&text, offset, offset + len) });
                } else if cfg.protocol == "doubao" {
                    if code == "20000003" { return Ok(vec![]); }
                    if !code.is_empty() && code != "20000000" {
                        if code.starts_with("55") { last = format!("{code} {msg}"); } else { return Err(anyhow!("豆包识别失败（{code}）：{msg}")); }
                    } else {
                        let v: Value = serde_json::from_str(&body).map_err(|_| anyhow!("豆包返回了无法解析的结果"))?;
                        let utts = v["result"]["utterances"].as_array().cloned().unwrap_or_default();
                        return Ok(utts.iter().filter_map(|u| {
                            let text = u["text"].as_str()?.trim().to_string();
                            (!text.is_empty()).then(|| Segment { start: offset + u["start_time"].as_f64().unwrap_or(0.0) / 1000.0, end: offset + u["end_time"].as_f64().unwrap_or(0.0) / 1000.0, text })
                        }).collect());
                    }
                } else {
                    let v: Value = serde_json::from_str(&body).map_err(|_| anyhow!("识别服务返回了无法解析的结果"))?;
                    let segs = v["segments"].as_array().cloned().unwrap_or_default();
                    if !segs.is_empty() {
                        return Ok(segs.iter().filter_map(|s| {
                            let text = s["text"].as_str()?.trim().to_string();
                            let st = s["start"].as_f64().unwrap_or(0.0);
                            (!text.is_empty()).then(|| Segment { start: offset + st, end: offset + s["end"].as_f64().unwrap_or(st).max(st), text })
                        }).collect());
                    }
                    let text = v["text"].as_str().unwrap_or("").trim().to_string();
                    return Ok(if text.is_empty() { vec![] } else { crate::transcript::split_timed(&text, offset, offset + len) });
                }
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(800 * (attempt + 1))).await;
    }
    Err(anyhow!("连接识别服务失败：{last}"))
}

/// Recognises 16 kHz mono samples in chunks, a few at a time, reporting progress 0–1.
pub async fn transcribe(cfg: &CloudAsr, samples: Vec<f32>, language: &str, ctx: &crate::asr::AsrContext, cancel: Arc<AtomicBool>, on_progress: impl Fn(f64)) -> Result<Vec<Segment>> {
    // DeepSeek recognition uses the same key as DeepSeek for Q&A when it has none of its own.
    let key = crate::llm::get_key(&format!("asr.{}", cfg.id)).filter(|k| !k.is_empty())
        .or_else(|| (cfg.protocol == "deepseek").then(|| crate::llm::get_key("deepseek")).flatten()).unwrap_or_default();
    if cfg.needs_key.unwrap_or(true) && key.is_empty() { return Err(anyhow!("请先在设置里填写这个识别服务的 API Key")); }
    let chunk = (cfg.chunk_secs.unwrap_or(if cfg.protocol == "doubao" { 300.0 } else { 20.0 }) * RATE as f64) as usize;
    // Borrowed slices, not copies: a long episode's samples are already hundreds of MB.
    let chunks: Vec<(usize, &[f32])> = samples.chunks(chunk.max(RATE as usize)).enumerate().collect();
    let total = chunks.len().max(1);
    let done = std::sync::atomic::AtomicUsize::new(0);
    let parallel = match cfg.protocol.as_str() { "doubao" => 2, "deepseek" => 4, _ => 4 };
    let slots = tokio::sync::Semaphore::new(parallel);
    let parts: Vec<Vec<Segment>> = futures_util::future::try_join_all(chunks.into_iter().map(|(i, c)| {
        let (key, cfg, cancel, done, on_progress, slots) = (key.clone(), cfg.clone(), cancel.clone(), &done, &on_progress, &slots);
        let (language, ctx) = (language.to_string(), ctx.clone());
        async move {
            let _slot = slots.acquire().await;
            if cancel.load(Ordering::Relaxed) { return Err(anyhow!("已取消")); }
            let offset = (i * chunk) as f64 / RATE as f64;
            let len = c.len() as f64 / RATE as f64;
            // A cancel stops the request in flight too, instead of waiting out its timeout and retries.
            let watch = async { while !cancel.load(Ordering::Relaxed) { tokio::time::sleep(std::time::Duration::from_millis(300)).await; } };
            let r = tokio::select! {
                r = recognize_chunk(&cfg, &key, wav_bytes(c, RATE), offset, len, &language, &ctx) => r,
                _ = watch => Err(anyhow!("已取消")),
            };
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            on_progress(n as f64 / total as f64);
            r
        }
    // The first error (a wrong key, say) stops the rest instead of sending every remaining chunk.
    })).await?;
    Ok(parts.into_iter().flatten().collect())
}

// ---- Doubao TTS (seed-tts-2.0) ------------------------------------------------------------------

const TTS_ENDPOINT: &str = "https://openspeech.bytedance.com/api/v3/tts/unidirectional";
const TTS_RATE: u32 = 24_000;

/// Synthesises up to 1,000 characters; returns a WAV file.
pub async fn tts(voice: &str, resource: &str, text: &str) -> Result<Vec<u8>> {
    let key = crate::llm::get_key("tts.doubao").or_else(|| crate::llm::get_key("asr.doubao")).filter(|k| !k.is_empty())
        .ok_or_else(|| anyhow!("请先在设置里填写豆包语音的 API Key"))?;
    let text = text.trim();
    if text.is_empty() || text.chars().count() > 1000 { return Err(anyhow!("每次朗读 1–1000 个字")); }
    let resp = client().post(TTS_ENDPOINT)
        .header("X-Api-Key", key)
        .header("X-Api-Resource-Id", if resource.is_empty() { "seed-tts-2.0" } else { resource })
        .header("X-Api-Request-Id", uuid::Uuid::new_v4().to_string())
        .json(&json!({ "req_params": { "text": text, "speaker": voice, "audio_params": { "format": "pcm", "sample_rate": TTS_RATE } } }))
        .timeout(std::time::Duration::from_secs(180))
        .send().await.map_err(|_| anyhow!("连不上豆包语音服务"))?;
    if !resp.status().is_success() { return Err(anyhow!("豆包语音合成返回 HTTP {}（检查 Key 与音色）", resp.status())); }
    let body = resp.text().await?;
    let mut pcm = vec![];
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value = serde_json::from_str(line).map_err(|_| anyhow!("豆包返回了无法解析的音频"))?;
        let code = v["code"].as_i64().unwrap_or(0);
        if code != 0 && code != 20_000_000 { return Err(anyhow!("豆包拒绝了合成请求（{code}）：{}", v["message"].as_str().unwrap_or("检查 Key、模型和音色"))); }
        if let Some(d) = v["data"].as_str() { base64::engine::general_purpose::STANDARD.decode_vec(d, &mut pcm).map_err(|_| anyhow!("音频数据损坏"))?; }
    }
    if pcm.len() < 2 { return Err(anyhow!("豆包没有返回音频")); }
    let samples: Vec<f32> = pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    Ok(wav_bytes(&samples, TTS_RATE))
}

// ---- model lists ---------------------------------------------------------------------------------

/// The model ids a provider offers, for the model picker.
pub async fn list_models(p: &crate::llm::Provider) -> Result<Vec<String>> {
    let key = crate::llm::get_key(&p.id).unwrap_or_default();
    let base = p.base_url.trim().trim_end_matches('/').trim_end_matches("/chat/completions").trim_end_matches("/messages").to_string();
    let req = if p.protocol == "anthropic" {
        let b = if base.is_empty() { "https://api.anthropic.com".to_string() } else { base };
        let b = if b.ends_with("/v1") { b } else { format!("{b}/v1") };
        client().get(format!("{b}/models?limit=100")).header("x-api-key", &key).header("anthropic-version", "2023-06-01")
    } else {
        let r = client().get(format!("{base}/models"));
        if key.is_empty() { r } else { r.bearer_auth(&key) }
    };
    let resp = req.timeout(std::time::Duration::from_secs(20)).send().await?;
    let status = resp.status();
    let v: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() { return Err(anyhow!("获取模型列表失败（{status}）")); }
    let list = v["data"].as_array().or_else(|| v["models"].as_array()).cloned().unwrap_or_default();
    let mut ids: Vec<String> = list.iter().filter_map(|m| m["id"].as_str().or_else(|| m["name"].as_str()).map(String::from)).collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

#[cfg(test)]
mod asr_request_tests {
    use super::*;
    use std::sync::mpsc;

    // Exercise the real multipart request and response parser without a cloud account or real audio.
    async fn run_request(provider: &str) -> (Result<Vec<Segment>>, String) {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", server.server_addr());
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut request = server.recv_timeout(std::time::Duration::from_secs(15)).unwrap().expect("request");
            assert_eq!(request.url(), "/audio/transcriptions");
            let mut body = Vec::new();
            request.as_reader().read_to_end(&mut body).unwrap();
            let body = String::from_utf8_lossy(&body).into_owned();
            let verbose = body.contains("name=\"response_format\"");
            let timestamps = body.contains("name=\"timestamp_granularities[]\"");
            let language = body.contains("name=\"language\"");
            let is_siliconflow = body.contains("Qwen/Qwen3-ASR-1.7B");
            let response = if is_siliconflow && (verbose || timestamps || language) {
                tiny_http::Response::from_string("").with_status_code(400)
            } else if is_siliconflow {
                tiny_http::Response::from_string(r#"{"text":"Synthetic speech sample."}"#)
            } else {
                tiny_http::Response::from_string(r#"{"segments":[{"text":"Timed control.","start":0.25,"end":1.5}]}"#)
            };
            request.respond(response).unwrap();
            tx.send(body).unwrap();
        });
        let cfg = CloudAsr {
            id: provider.to_string(), protocol: "openai".to_string(), base_url,
            model: if provider == "siliconflow" { "Qwen/Qwen3-ASR-1.7B" } else { "whisper-1" }.to_string(),
            chunk_secs: None, needs_key: Some(false),
        };
        let result = recognize_chunk(&cfg, "", wav_bytes(&[0.0; 160], RATE), 5.0, 2.0, "en", &crate::asr::AsrContext::default()).await;
        let body = rx.recv_timeout(std::time::Duration::from_secs(15)).unwrap();
        worker.join().unwrap();
        (result, body)
    }

    #[tokio::test]
    async fn siliconflow_text_only_response() {
        let (result, body) = run_request("siliconflow").await;
        let segments = result.expect("SiliconFlow-style endpoint accepts the request");
        assert!(!body.contains("name=\"response_format\""));
        assert!(!body.contains("name=\"timestamp_granularities[]\""));
        assert!(!body.contains("name=\"language\""));
        assert!(body.contains("RIFF"));
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Synthetic speech sample.");
        assert_eq!((segments[0].start, segments[0].end), (5.0, 7.0));
    }

    #[tokio::test]
    async fn openai_keeps_timestamp_request_and_response() {
        let (result, body) = run_request("openai").await;
        let segments = result.unwrap();
        assert!(body.contains("name=\"response_format\""));
        assert!(body.contains("verbose_json"));
        assert!(body.contains("name=\"timestamp_granularities[]\""));
        assert!(body.contains("name=\"language\""));
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].text, "Timed control.");
        assert_eq!((segments[0].start, segments[0].end), (5.25, 6.5));
    }
}
