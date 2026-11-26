//! Settings defaults, dot-path inspection/mutation, and provider/ASR/TTS resolution aligned with `src/lib/settings.ts`.
use crate::{agents, cloud, ctx::CoreCtx, llm, ops, speech};
use anyhow::{anyhow, Result};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub group: &'static str,
    pub protocol: &'static str,
    pub base_url: &'static str,
    pub model: &'static str,
    pub needs_key: bool,
    pub models: &'static [&'static str],
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AsrServicePreset {
    pub id: &'static str,
    pub name: &'static str,
    pub protocol: &'static str,
    pub base_url: &'static str,
    pub model: &'static str,
    pub needs_key: bool,
    pub chunk_secs: f64,
    pub models: &'static [&'static str],
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TtsServicePreset {
    pub id: &'static str,
    pub name: &'static str,
    pub group: &'static str,
    pub protocol: &'static str,
    pub base_url: &'static str,
    pub model: &'static str,
    pub voice: &'static str,
    pub resource: Option<&'static str>,
    pub needs_key: bool,
}

// i18n-ignore-start (mirrored from src/lib/settings.ts presets)
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset { id: "deepseek", name: "DeepSeek", group: "provider", protocol: "openai", base_url: "https://api.deepseek.com/v1", model: "deepseek-flash", needs_key: true, models: &["deepseek-flash", "deepseek-v4-pro"] },
    ProviderPreset { id: "siliconflow", name: "硅基流动", group: "relay", protocol: "openai", base_url: "https://api.siliconflow.cn/v1", model: "deepseek-ai/deepseek-v4-flash", needs_key: true, models: &["deepseek-ai/deepseek-v4-flash", "deepseek-ai/deepseek-v4-pro"] },
    ProviderPreset { id: "tokendance", name: "TokenDance", group: "relay", protocol: "openai", base_url: "https://tokendance.space/gateway/v1", model: "deepseek-v4-flash", needs_key: true, models: &["deepseek-v4-flash", "qwen3.8-flash", "kimi-k3"] },
    ProviderPreset { id: "openrouter", name: "OpenRouter", group: "relay", protocol: "openai", base_url: "https://openrouter.ai/api/v1", model: "anthropic/claude-sonnet-5.5", needs_key: true, models: &["anthropic/claude-sonnet-5.5"] },
    ProviderPreset { id: "opencode-go", name: "OpenCode Go", group: "relay", protocol: "openai", base_url: "https://opencode.ai/zen/go/v1", model: "deepseek-v4.1-flash", needs_key: true, models: &["deepseek-v4.1-flash", "glm-5.3-flash", "kimi-k3", "longcat-2.0", "mimo-v2.6-flash"] },
    ProviderPreset { id: "anthropic", name: "Claude（Anthropic）", group: "provider", protocol: "anthropic", base_url: "https://api.anthropic.com", model: "claude-opus-5-5", needs_key: true, models: &["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"] },
    ProviderPreset { id: "openai", name: "OpenAI", group: "provider", protocol: "openai", base_url: "https://api.openai.com/v1", model: "gpt-5.6-luna", needs_key: true, models: &["gpt-5.6-luna", "gpt-5.6-terra", "gpt-5.6-sol"] },
    ProviderPreset { id: "google-gemini", name: "Google Gemini", group: "provider", protocol: "openai", base_url: "https://generativelanguage.googleapis.com/v1beta/openai", model: "gemini-3.5-flash", needs_key: true, models: &["gemini-3.5-flash", "gemini-3.1-flash-lite", "gemini-3.1-pro-preview"] },
    ProviderPreset { id: "moonshot", name: "Kimi（月之暗面）", group: "provider", protocol: "openai", base_url: "https://api.moonshot.cn/v1", model: "kimi-k3", needs_key: true, models: &["kimi-k3", "kimi-k2.6"] },
    ProviderPreset { id: "zhipu", name: "智谱 GLM", group: "provider", protocol: "openai", base_url: "https://open.bigmodel.cn/api/paas/v4", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "qwen-cn", name: "通义千问（百炼）", group: "provider", protocol: "openai", base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "doubao", name: "豆包（火山方舟）", group: "provider", protocol: "openai", base_url: "https://ark.cn-beijing.volces.com/api/v3", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "minimax-cn", name: "MiniMax", group: "provider", protocol: "openai", base_url: "https://api.minimaxi.com/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "stepfun", name: "阶跃星辰", group: "provider", protocol: "openai", base_url: "https://api.stepfun.com/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "qianfan", name: "百度千帆", group: "provider", protocol: "openai", base_url: "https://qianfan.baidubce.com/v2", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "hunyuan", name: "腾讯混元", group: "provider", protocol: "openai", base_url: "https://api.hunyuan.cloud.tencent.com/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "mimo", name: "小米 MiMo", group: "provider", protocol: "openai", base_url: "https://api.xiaomimimo.com/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "xai", name: "xAI", group: "provider", protocol: "openai", base_url: "https://api.x.ai/v1", model: "grok-4.5", needs_key: true, models: &["grok-4.5", "grok-4.3"] },
    ProviderPreset { id: "mistral", name: "Mistral", group: "provider", protocol: "openai", base_url: "https://api.mistral.ai/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "groq", name: "Groq", group: "provider", protocol: "openai", base_url: "https://api.groq.com/openai/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "perplexity", name: "Perplexity", group: "provider", protocol: "openai", base_url: "https://api.perplexity.ai", model: "sonar", needs_key: true, models: &["sonar", "sonar-pro"] },
    ProviderPreset { id: "nvidia", name: "NVIDIA", group: "provider", protocol: "openai", base_url: "https://integrate.api.nvidia.com/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "together", name: "Together AI", group: "provider", protocol: "openai", base_url: "https://api.together.xyz/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "fireworks", name: "Fireworks AI", group: "provider", protocol: "openai", base_url: "https://api.fireworks.ai/inference/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "cerebras", name: "Cerebras", group: "provider", protocol: "openai", base_url: "https://api.cerebras.ai/v1", model: "", needs_key: true, models: &[] },
    ProviderPreset { id: "bilibili", name: "B 站翻译（免费）", group: "provider", protocol: "openai", base_url: "https://index-translate.bilibili.com/v1", model: "Index-Translate-35B-A3B", needs_key: false, models: &["Index-Translate-35B-A3B"] },
    ProviderPreset { id: "ollama", name: "Ollama", group: "local", protocol: "openai", base_url: "http://127.0.0.1:11434/v1", model: "qwen3.5", needs_key: false, models: &["qwen3.5", "gemma4", "gpt-oss:20b"] },
    ProviderPreset { id: "lmstudio", name: "LM Studio", group: "local", protocol: "openai", base_url: "http://127.0.0.1:1234/v1", model: "", needs_key: false, models: &[] },
    ProviderPreset { id: "magpie", name: "Magpie", group: "local", protocol: "openai", base_url: "http://127.0.0.1:3425/v1", model: "", needs_key: false, models: &[] },
    ProviderPreset { id: "custom", name: "其他（OpenAI 兼容）", group: "provider", protocol: "openai", base_url: "", model: "", needs_key: true, models: &[] },
];

pub const ASR_PRESETS: &[AsrServicePreset] = &[
    AsrServicePreset { id: "doubao", name: "豆包语音（极速版）", protocol: "doubao", base_url: "https://openspeech.bytedance.com/api/v3", model: "bigmodel", needs_key: true, chunk_secs: 300.0, models: &["bigmodel"] },
    AsrServicePreset { id: "deepseek", name: "DeepSeek", protocol: "deepseek", base_url: "https://api.deepseek.com", model: "asr", needs_key: true, chunk_secs: 60.0, models: &["asr"] },
    AsrServicePreset { id: "siliconflow", name: "硅基流动", protocol: "openai", base_url: "https://api.siliconflow.cn/v1", model: "Qwen/Qwen3-ASR-1.7B", needs_key: true, chunk_secs: 20.0, models: &["Qwen/Qwen3-ASR-1.7B", "FunAudioLLM/SenseVoiceSmall", "XingChenAGI/XingChenASR-V3.2"] },
    AsrServicePreset { id: "groq", name: "Groq", protocol: "openai", base_url: "https://api.groq.com/openai/v1", model: "whisper-large-v3-turbo", needs_key: true, chunk_secs: 600.0, models: &["whisper-large-v3-turbo", "whisper-large-v3"] },
    AsrServicePreset { id: "openai", name: "OpenAI", protocol: "openai", base_url: "https://api.openai.com/v1", model: "gpt-4o-mini-transcribe", needs_key: true, chunk_secs: 600.0, models: &["whisper-1", "gpt-4o-mini-transcribe", "gpt-4o-transcribe"] },
    AsrServicePreset { id: "glm", name: "智谱 GLM-ASR", protocol: "openai", base_url: "https://open.bigmodel.cn/api/paas/v4", model: "glm-asr-2512", needs_key: true, chunk_secs: 25.0, models: &["glm-asr-2512"] },
    AsrServicePreset { id: "stepfun", name: "阶跃星辰", protocol: "openai", base_url: "https://api.stepfun.com/v1", model: "stepaudio-2.5-asr", needs_key: true, chunk_secs: 20.0, models: &["stepaudio-2.5-asr", "step-asr"] },
    AsrServicePreset { id: "local-server", name: "本机识别服务", protocol: "openai", base_url: "http://127.0.0.1:8765/v1", model: "Qwen/Qwen3-ASR-0.6B", needs_key: false, chunk_secs: 60.0, models: &["Qwen/Qwen3-ASR-0.6B", "whisper-1"] },
];

pub const TTS_PRESETS: &[TtsServicePreset] = &[
    TtsServicePreset { id: "system", name: "系统语音", group: "local", protocol: "system", base_url: "", model: "", voice: "", resource: None, needs_key: false },
    TtsServicePreset { id: "local", name: "本地语音", group: "local", protocol: "local", base_url: "", model: "melo", voice: "zf_001", resource: None, needs_key: false },
    TtsServicePreset { id: "doubao", name: "豆包语音", group: "cloud", protocol: "doubao", base_url: "https://openspeech.bytedance.com/api/v3/tts/unidirectional", model: "seed-tts-2.0", voice: "zh_female_vv_uranus_bigtts", resource: Some("seed-tts-2.0"), needs_key: true },
    TtsServicePreset { id: "deepseek", name: "DeepSeek", group: "cloud", protocol: "deepseek", base_url: "https://api.deepseek.com", model: "", voice: "stella", resource: None, needs_key: true },
    TtsServicePreset { id: "mimo", name: "小米 MiMo", group: "cloud", protocol: "mimo", base_url: "https://api.xiaomimimo.com/v1", model: "mimo-v2.5-tts", voice: "mimo_default", resource: None, needs_key: true },
    TtsServicePreset { id: "qwen", name: "通义 Qwen-TTS", group: "cloud", protocol: "qwen", base_url: "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation", model: "qwen3-tts-flash", voice: "Cherry", resource: None, needs_key: true },
    TtsServicePreset { id: "openai", name: "OpenAI", group: "cloud", protocol: "openai", base_url: "https://api.openai.com/v1", model: "gpt-4o-mini-tts", voice: "alloy", resource: None, needs_key: true },
    TtsServicePreset { id: "edge", name: "Edge 在线语音", group: "cloud", protocol: "edge", base_url: "", model: "", voice: "zh-CN-YunxiNeural", resource: None, needs_key: false },
    TtsServicePreset { id: "siliconflow", name: "硅基流动", group: "cloud", protocol: "openai", base_url: "https://api.siliconflow.cn/v1", model: "FunAudioLLM/CosyVoice2-0.5B", voice: "anna", resource: None, needs_key: true },
    TtsServicePreset { id: "minimax", name: "MiniMax", group: "cloud", protocol: "minimax", base_url: "https://api.minimaxi.com/v1", model: "speech-02-hd", voice: "male-qn-qingse", resource: None, needs_key: true },
    TtsServicePreset { id: "elevenlabs", name: "ElevenLabs", group: "cloud", protocol: "elevenlabs", base_url: "https://api.elevenlabs.io/v1", model: "eleven_multilingual_v2", voice: "JBFqnCBsd6RMkjVDRZzb", resource: None, needs_key: true },
    TtsServicePreset { id: "custom", name: "其他（OpenAI 兼容）", group: "cloud", protocol: "openai", base_url: "http://127.0.0.1:8880/v1", model: "tts-1", voice: "alloy", resource: None, needs_key: false },
];
// i18n-ignore-end

pub fn defaults_json() -> Value {
    let mut providers = serde_json::Map::new();
    for p in PROVIDER_PRESETS {
        providers.insert(p.id.into(), json!({
            "id": p.id, "name": p.name, "group": p.group, "protocol": p.protocol,
            "baseUrl": p.base_url, "model": p.model, "needsKey": p.needs_key, "models": p.models
        }));
    }
    let mut cloud_asr = serde_json::Map::new();
    for a in ASR_PRESETS {
        cloud_asr.insert(a.id.into(), json!({
            "id": a.id, "name": a.name, "protocol": a.protocol, "baseUrl": a.base_url,
            "model": a.model, "needsKey": a.needs_key, "chunkSecs": a.chunk_secs, "models": a.models
        }));
    }
    let mut tts_services = serde_json::Map::new();
    for t in TTS_PRESETS {
        tts_services.insert(t.id.into(), json!({
            "id": t.id, "name": t.name, "group": t.group, "protocol": t.protocol,
            "baseUrl": t.base_url, "model": t.model, "voice": t.voice,
            "resource": t.resource, "needsKey": t.needs_key
        }));
    }
    let mut m = serde_json::Map::new();
    m.insert("providers".into(), Value::Object(providers));
    m.insert("cloudAsr".into(), Value::Object(cloud_asr));
    m.insert("ttsServices".into(), Value::Object(tts_services));
    let kvs: [(&str, Value); 39] = [
        ("chatProvider", "deepseek".into()),
        ("translateProvider", "deepseek".into()),
        ("translateMode", "auto".into()),
        ("onboarded", false.into()),
        ("proxy", "".into()),
        ("asrModel", "qwen3-asr".into()),
        ("asrLanguage", "auto".into()),
        ("mirrorFirst", true.into()),
        ("autoTranslate", true.into()),
        ("translateQuality", "fast".into()),
        ("useShowNotes", true.into()),
        ("subtitleMode", "bilingual".into()),
        ("playbackRate", 1.0.into()),
        ("asrEngine", "local".into()),
        ("ttsEngine", "system".into()),
        ("ttsRate", 1.0.into()),
        ("ttsVoice", "zh_female_vv_uranus_bigtts".into()),
        ("ttsResource", "seed-tts-2.0".into()),
        ("chatModel", "".into()),
        ("chatPrompt", "".into()),
        ("chatPromptOn", true.into()),
        ("quickPrompts", Value::Null),
        ("uiLang", "auto".into()),
        ("youtubePromo", true.into()),
        ("translateShowNames", false.into()),
        ("autoUpdate", true.into()),
        ("homeBannerHidden", false.into()),
        ("sideCurated", true.into()),
        ("learnOn", false.into()),
        ("learnLevel", "cet6".into()),
        ("learnMark", true.into()),
        ("learnCard", true.into()),
        ("learnAi", true.into()),
        ("learnSide", true.into()),
        ("learnDaily", 20.into()),
        ("learnRemind", false.into()),
        ("learnSounds", true.into()),
        ("learnPauseCard", false.into()),
        ("learnAsked", false.into()),
    ];
    for (k, v) in kvs {
        m.insert(k.into(), v);
    }
    m.insert("shareTranscripts".into(), true.into());
    Value::Object(m)
}

fn merge_json(base: &mut Value, overlay: &Value) {
    match (base, overlay) {
        (Value::Object(b), Value::Object(o)) => {
            for (k, v) in o {
                match b.get_mut(k) {
                    Some(existing) if existing.is_object() && v.is_object() => merge_json(existing, v),
                    _ => { b.insert(k.clone(), v.clone()); }
                }
            }
        }
        (b, o) if !o.is_null() => *b = o.clone(),
        _ => {}
    }
}

pub fn load_effective(ctx: &CoreCtx) -> Result<Value> {
    let mut base = defaults_json();
    let stored = ops::get_settings(ctx)?;
    if !stored.is_null() {
        merge_json(&mut base, &stored);
    }
    Ok(base)
}

pub fn get_path<'a>(val: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = val;
    for seg in path.split('.').filter(|s| !s.is_empty()) {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

pub fn set_path(val: &mut Value, path: &str, new_val: Value) -> Result<()> {
    let segs: Vec<&str> = path.split('.').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return Err(anyhow!("empty setting path")); // i18n-ignore
    }
    let mut cur = val;
    for &seg in &segs[..segs.len() - 1] {
        if !cur.is_object() {
            *cur = json!({});
        }
        let map = cur.as_object_mut().unwrap();
        if !map.contains_key(seg) || !map[seg].is_object() {
            map.insert(seg.to_string(), json!({}));
        }
        cur = map.get_mut(seg).unwrap();
    }
    if !cur.is_object() {
        *cur = json!({});
    }
    cur.as_object_mut().unwrap().insert(segs[segs.len() - 1].to_string(), new_val);
    Ok(())
}

pub fn parse_cli_value(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed == "true" { return Value::Bool(true); }
    if trimmed == "false" { return Value::Bool(false); }
    if trimmed == "null" { return Value::Null; }
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
        || (trimmed.starts_with('"') && trimmed.ends_with('"'))
    {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            return v;
        }
    }
    if let Ok(i) = trimmed.parse::<i64>() {
        return Value::Number(i.into());
    }
    if let Ok(f) = trimmed.parse::<f64>() {
        if let Some(n) = serde_json::Number::from_f64(f) {
            return Value::Number(n);
        }
    }
    Value::String(raw.to_string())
}

#[derive(Clone, Debug)]
pub struct ResolvedProvider {
    pub provider: llm::Provider,
    pub group: String,
    pub needs_key: bool,
}

fn resolve_id_from_settings(s: &Value, id: &str, model_override: Option<&str>) -> Option<ResolvedProvider> {
    if let Some(kind_name) = id.strip_prefix("cli-") {
        if agents::Kind::parse(kind_name).is_some() {
            let p = &s["providers"][id];
            let model = model_override
                .filter(|m| !m.is_empty())
                .or_else(|| p["model"].as_str().filter(|m| !m.is_empty()))
                .unwrap_or("")
                .to_string();
            return Some(ResolvedProvider {
                provider: llm::Provider {
                    id: id.to_string(),
                    protocol: "cli".into(),
                    base_url: String::new(),
                    model,
                },
                group: "cli".into(),
                needs_key: false,
            });
        }
    }
    let p = &s["providers"][id];
    if p.is_object() {
        let model = model_override
            .filter(|m| !m.is_empty())
            .or_else(|| p["model"].as_str().filter(|m| !m.is_empty()))
            .unwrap_or("")
            .to_string();
        let protocol = p["protocol"].as_str().unwrap_or("openai").to_string();
        let base_url = p["baseUrl"].as_str().unwrap_or("").to_string();
        let group = p["group"].as_str().unwrap_or("provider").to_string();
        let needs_key = p["needsKey"].as_bool().unwrap_or(group != "local");
        if protocol == "cli" || !base_url.is_empty() || !model.is_empty() {
            return Some(ResolvedProvider {
                provider: llm::Provider { id: id.to_string(), protocol, base_url, model },
                group,
                needs_key,
            });
        }
    }
    if let Some(preset) = PROVIDER_PRESETS.iter().find(|x| x.id == id) {
        return Some(ResolvedProvider {
            provider: llm::Provider {
                id: preset.id.into(),
                protocol: preset.protocol.into(),
                base_url: preset.base_url.into(),
                model: model_override.filter(|m| !m.is_empty()).unwrap_or(preset.model).into(),
            },
            group: preset.group.into(),
            needs_key: preset.needs_key,
        });
    }
    None
}

pub fn provider_ready(r: &ResolvedProvider) -> bool {
    if r.provider.protocol == "cli" {
        let kind = agents::Kind::parse(r.provider.id.trim_start_matches("cli-"));
        return kind.and_then(agents::path_of).is_some();
    }
    !r.provider.model.is_empty()
        && !r.provider.base_url.is_empty()
        && (!r.needs_key || llm::get_key(&r.provider.id).is_some_and(|k| !k.is_empty()))
}

pub fn resolve_chat_provider(ctx: &CoreCtx, id_override: Option<&str>, model_override: Option<&str>) -> Result<ResolvedProvider> {
    let s = load_effective(ctx)?;
    let id = id_override
        .filter(|x| !x.is_empty())
        .or_else(|| s["chatProvider"].as_str())
        .unwrap_or("deepseek");
    let chat_model = if id_override.is_none() {
        model_override.filter(|x| !x.is_empty()).or_else(|| s["chatModel"].as_str().filter(|x| !x.is_empty()))
    } else {
        model_override
    };
    resolve_id_from_settings(&s, id, chat_model)
        .ok_or_else(|| anyhow!("请先在设置里连接 AI 模型"))
}

pub fn resolve_translate_provider(ctx: &CoreCtx, id_override: Option<&str>, model_override: Option<&str>) -> Option<ResolvedProvider> {
    let s = load_effective(ctx).ok()?;
    let id = id_override
        .filter(|x| !x.is_empty())
        .or_else(|| s["translateProvider"].as_str())
        .unwrap_or("deepseek");
    resolve_id_from_settings(&s, id, model_override)
}

pub fn resolve_cloud_asr(ctx: &CoreCtx, engine_id: Option<&str>, model_override: Option<&str>) -> Result<Option<cloud::CloudAsr>> {
    let s = load_effective(ctx)?;
    let engine = engine_id
        .filter(|x| !x.is_empty())
        .or_else(|| s["asrEngine"].as_str())
        .unwrap_or("local");
    if engine == "local" {
        return Ok(None);
    }
    let obj = &s["cloudAsr"][engine];
    if !obj.is_object() {
        return Err(anyhow!("未知的云端语音识别服务：{engine}")); // i18n-ignore
    }
    let mut cfg: cloud::CloudAsr = serde_json::from_value(obj.clone())?;
    if let Some(m) = model_override.filter(|m| !m.is_empty()) {
        cfg.model = m.to_string();
    }
    Ok(Some(cfg))
}

pub fn resolve_tts_request(
    ctx: &CoreCtx,
    text: &str,
    engine_override: Option<&str>,
    voice_override: Option<&str>,
    rate_override: Option<f64>,
    model_override: Option<&str>,
) -> Result<speech::TtsRequest> {
    let s = load_effective(ctx)?;
    let engine = engine_override
        .filter(|x| !x.is_empty())
        .or_else(|| s["ttsEngine"].as_str())
        .unwrap_or("system");
    let svc = &s["ttsServices"][engine];
    if !svc.is_object() {
        return Err(anyhow!("未知的朗读引擎：{engine}")); // i18n-ignore
    }
    let protocol = svc["protocol"].as_str().unwrap_or(engine).to_string();
    let voice = voice_override
        .filter(|v| !v.is_empty())
        .or_else(|| svc["voice"].as_str())
        .unwrap_or("")
        .to_string();
    let wanted_rate = rate_override.or_else(|| s["ttsRate"].as_f64()).unwrap_or(1.0);
    let rate = if protocol == "deepseek" { 1.0 } else { wanted_rate };
    Ok(speech::TtsRequest {
        engine: engine.to_string(),
        protocol,
        base_url: svc["baseUrl"].as_str().filter(|x| !x.is_empty()).map(String::from),
        model: model_override
            .filter(|x| !x.is_empty())
            .or_else(|| svc["model"].as_str().filter(|x| !x.is_empty()))
            .map(String::from),
        voice,
        resource: svc["resource"].as_str().filter(|x| !x.is_empty()).map(String::from),
        text: text.to_string(),
        rate: Some(rate),
        needs_key: svc["needsKey"].as_bool(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_and_set_path_preserves_unknown_fields() {
        let mut v = json!({ "proxy": "", "customGuiFlag": { "nested": 42 } });
        set_path(&mut v, "providers.openai.model", Value::String("gpt-5".into())).unwrap();
        assert_eq!(get_path(&v, "providers.openai.model").unwrap(), &json!("gpt-5"));
        assert_eq!(get_path(&v, "customGuiFlag.nested").unwrap(), &json!(42));
    }
}
