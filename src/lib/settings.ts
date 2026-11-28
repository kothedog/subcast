import { api, type CloudAsr, type Provider, type TtsRequest } from "./api";
import { t, tk, type LangSetting } from "./i18n";

export type TtsProtocol = TtsRequest["protocol"];
export interface SyncSettings {
  icloudEnabled: boolean;
  icloudDir: string;
  webdavEnabled: boolean;
  webdavUrl: string;
  webdavPath: string;
  webdavUsername: string;
  autoSync: boolean;
  syncProgress: boolean;
  syncContent: boolean;
}
export interface TtsVoice { id: string; name: string }
export interface TtsService {
  id: string;
  name: string;
  note: string;
  group: "local" | "cloud";
  protocol: TtsProtocol;
  baseUrl: string;
  model: string;
  models: string[];
  voice: string;
  voices: TtsVoice[];
  resource?: string;
  keyUrl?: string;
  needsKey: boolean;
  /** Shown as placeholder on the Key field when an AI/ASR key can be reused. */
  sharedKeyHint?: string;
}

export interface Settings {
  providers: Record<string, Provider>;
  chatProvider: string;
  translateProvider: string;
  /** auto: the user's model when it is set up, else Google; google: always Google; llm: always the model. */
  translateMode: "auto" | "google" | "llm";
  onboarded: boolean;
  proxy: string;
  asrModel: string;
  asrLanguage: string;
  mirrorFirst: boolean;
  autoTranslate: boolean;
  /** fast: one pass. best: a second pass that makes each line read naturally (about twice the model cost). */
  translateQuality: "fast" | "best";
  /** Give recognition and translation the episode's title and show notes (names and terms come out right). */
  useShowNotes: boolean;
  /** The listener picked the models themselves: local agents are never switched in automatically after that. */
  modelChosen?: boolean;
  /** When YouTube asks to confirm "not a bot", download with this browser's YouTube sign-in ("" = never). */
  ytCookiesBrowser?: string;
  /** Shortcuts the listener changed (Settings › 快捷键); the rest use the defaults in lib/shortcuts.ts. */
  /** Reading: subtitle size step, font, video subtitle look, Q&A text size, the next line on audio, skips, auto-next. */
  subtitleSize?: "s" | "m" | "l" | "xl";
  subtitleFont?: "system" | "serif" | "kai" | "sans";
  subtitleLook?: "box" | "outline";
  /** Subtitle size as a multiplier, set from the video's "Aa" (0.7–2); wins over `subtitleSize` when set. */
  subtitleScale?: number;
  /** The original-language line's size multiplier (0.7–2); unset = follows `subtitleScale`. */
  subtitleOrigScale?: number;
  /** How far video subtitles are moved up from their usual place, in % of the picture's height (dragged). */
  subtitleLift?: number;
  /** Old two-step size; `chatScale` replaces it once the slider moves. */
  chatSize?: "m" | "l";
  /** AI answers and guides text size, 0.85–1.5 (1 = 14px). */
  chatScale?: number;
  audioNextLine?: boolean;
  skipBack?: number;
  skipForward?: number;
  autoNext?: boolean;
  /** Show highlights and clip-making (unset: shown). */
  clipTools?: boolean;
  /** The one-time offer to connect AI agents to SubCast was answered (or done from settings). */
  agentsOffered?: boolean;
  /** Skip what only the playing audio has when the subtitles were made from another cut (ads, mostly). */
  skipAds?: boolean;
  shortcuts?: Partial<Record<import("./shortcuts").ActionId, string[]>>;
  /** The local agent SubCast switched in on its own (shown as such in settings). */
  autoPicked?: string;
  subtitleMode: SubtitleMode;
  /** Per-show (or standalone episode) subtitle display preference; overrides `subtitleMode` when set. */
  showSubtitleModes?: Record<string, SubtitleMode>;
  playbackRate: number;
  /** "local", or the id of a cloud transcription service. */
  asrEngine: string;
  cloudAsr: Record<string, CloudAsr>;
  /** Active TTS engine id ("edge", "local", "system", "doubao", "mimo", "qwen", "openai", "siliconflow", "minimax", "elevenlabs", "custom"). */
  ttsEngine: string;
  ttsServices: Record<string, TtsService>;
  /** Speech rate multiplier for TTS synthesis (0.75–1.5, default 1). */
  ttsRate: number;
  /** Legacy Doubao fields kept synced for older versions. */
  ttsVoice: string;
  ttsResource: string;
  /** The chat model picked in the chat box; "" means the chat provider's own model. */
  chatModel: string;
  /** A standing instruction added to every question. */
  chatPrompt: string;
  chatPromptOn: boolean;
  /** null keeps the built-in list, so it can improve with updates until the user edits it. */
  quickPrompts: QuickPrompt[] | null;
  /** Interface language; "auto" follows the system. */
  uiLang: LangSetting;
  /** Show the "subscribe on YouTube" links for shows that have an official channel. */
  youtubePromo: boolean;
  /** Show subscribed shows under their Chinese names (curated or machine-translated) in a Chinese interface. */
  translateShowNames: boolean;
  /** User-defined custom display names per show id. */
  showAliases?: Record<string, string>;
  /** Download new versions in the background; they install on the next restart. */
  autoUpdate: boolean;
  /** The Discover banner for our own show was closed by the user. */
  homeBannerHidden: boolean;
  /** Where highlight clips are exported (empty: Downloads). */
  clipExportDir?: string;
  /** List the 乔木精读 shows not yet followed under the sidebar's subscriptions. */
  sideCurated: boolean;
  /** English learning mode (设置 › 学英语). Off by default; everything below only applies when it is on. */
  learnOn: boolean;
  /** The listener's level: words above it are marked in English subtitles. */
  learnLevel: LearnLevel;
  learnMark: boolean;
  learnCard: boolean;
  /** Ask the default model what a word means in its sentence. */
  learnAi: boolean;
  learnSide: boolean;
  /** New words introduced per day in review. */
  learnDaily: number;
  learnRemind: boolean;
  /** Key, wrong-letter and right-word sounds in spelling review. */
  learnSounds: boolean;
  /** Pause playback while a word card is open (and resume when it closes). */
  learnPauseCard: boolean;
  /** 跟读: hear the listener with the microphone and move on once the sentence was said (else timed pauses). */
  shadowScore?: boolean;
  /** The one-time "想顺便学英语吗？" tip was answered. */
  learnAsked: boolean;
  /** Upload transcripts we make to the shared library (anonymous; reading shared ones is always on). */
  shareTranscripts: boolean;
  /** Cross-device sync via iCloud Drive and/or WebDAV. */
  sync: SyncSettings;
}

export type LearnLevel = "cet4" | "cet6" | "ky" | "ielts";

export interface QuickPrompt { id: string; title: string; body: string; enabled: boolean }

export const BUILTIN_PROMPTS: QuickPrompt[] = ([
  ["brief", tk("三分钟看懂这期"), tk("用三分钟能读完的篇幅讲清这期节目：主要聊了什么、得出了什么结论、为什么值得关注。最后推荐 3 个最值得听的片段，附时间戳。")],
  ["insights", tk("最有价值的观点"), tk("列出这期最有信息量的 5–8 个观点，按重要性排序。每条写：一句加粗的观点、嘉宾给出的理由、原话时间戳。")],
  ["guest", tk("嘉宾是谁"), tk("嘉宾是谁？结合节目内容介绍他的身份、经历和代表作，说明他为什么有资格谈这个话题。节目里没提到、属于常识补充的内容请标出来。")],
  ["debate", tk("反直觉和有争议的地方"), tk("这期有哪些反直觉或有争议的观点？逐条说明：通常大家怎么看、嘉宾怎么说、他的理由站不站得住、可能漏掉了什么。")],
  ["numbers", tk("数据和预测"), tk("把节目里出现的具体数字、时间表和对未来的预测整理成表格，列：内容、谁说的、时间戳、可信度和理由。")],
  ["resources", tk("提到的书和资源"), tk("整理节目里提到的书、论文、公司、产品、工具和人物，按类别列出，每项一句话说明，并附时间戳。")],
  ["action", tk("我能用上什么"), tk("站在普通从业者的角度，这期有哪些马上能用到工作或生活里的方法和建议？写成具体可做的动作，每条附出处时间戳。")],
  ["terms", tk("术语和黑话"), tk("列出节目里出现的专业术语、缩写和圈内说法，给出英文原文、通俗的中文解释和出现的时间戳。")],
  ["followup", tk("值得追问的问题"), tk("如果我能采访这位嘉宾，最值得追问的 3 个问题是什么？说明为什么要问，以及节目里哪里没讲透。")],
  ["share", tk("写一段分享"), tk("把这期的精华写成一段适合发朋友圈的分享，200 字以内，口语化，突出一个最有冲击力的观点。")],
]).map(([id, title, body]) => ({ id: `builtin-${id}`, title, body, enabled: true }));

export const PROMPT_HINTS = [tk("先给结论，再展开"), tk("简洁，不说套话"), tk("多用列表和表格"), tk("术语保留英文原文"), tk("讲给外行听，多打比方")];

export const quickPrompts = (s: Pick<Settings, "quickPrompts">) => s.quickPrompts ?? BUILTIN_PROMPTS.map((p) => ({ ...p, title: t(p.title), body: t(p.body) }));

// The model catalogue, ported from Capwise. Model names are suggestions; every field stays editable because providers
// rename models often, and 「获取模型列表」 reads the live list from the provider.
type G = Provider["group"];
const o = (id: string, name: string, group: G, baseUrl: string, keyUrl: string | undefined, models: string[] = [], needsKey = group !== "local"): Provider =>
  ({ id, name, group, protocol: "openai", baseUrl, model: models[0] ?? "", needsKey, keyUrl, models });

export const PRESETS: Provider[] = [
  o("deepseek", "DeepSeek", "provider", "https://api.deepseek.com/v1", "https://platform.deepseek.com/api_keys", ["deepseek-flash", "deepseek-v4-pro"]),
  o("siliconflow", tk("硅基流动"), "relay", "https://api.siliconflow.cn/v1", "https://cloud.siliconflow.cn/account/ak", ["deepseek-ai/deepseek-v4-flash", "deepseek-ai/deepseek-v4-pro"]),
  o("tokendance", "TokenDance", "relay", "https://tokendance.space/gateway/v1", "https://tokendance.space/keys", ["deepseek-v4-flash", "qwen3.8-flash", "kimi-k3"]),
  o("openrouter", "OpenRouter", "relay", "https://openrouter.ai/api/v1", "https://openrouter.ai/settings/keys", ["anthropic/claude-sonnet-5.5"]),
  o("opencode-go", "OpenCode Go", "relay", "https://opencode.ai/zen/go/v1", "https://opencode.ai/auth", ["deepseek-v4.1-flash", "glm-5.3-flash", "kimi-k3", "longcat-2.0", "mimo-v2.6-flash"]),
  { id: "anthropic", name: "Claude（Anthropic）", group: "provider", protocol: "anthropic", baseUrl: "https://api.anthropic.com", model: "claude-opus-5-5", needsKey: true, keyUrl: "https://console.anthropic.com/settings/keys", models: ["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"] },
  o("openai", "OpenAI", "provider", "https://api.openai.com/v1", "https://platform.openai.com/api-keys", ["gpt-5.6-luna", "gpt-5.6-terra", "gpt-5.6-sol"]),
  o("google-gemini", "Google Gemini", "provider", "https://generativelanguage.googleapis.com/v1beta/openai", "https://aistudio.google.com/app/apikey", ["gemini-3.5-flash", "gemini-3.1-flash-lite", "gemini-3.1-pro-preview"]),
  o("moonshot", tk("Kimi（月之暗面）"), "provider", "https://api.moonshot.cn/v1", "https://platform.moonshot.cn/console/api-keys", ["kimi-k3", "kimi-k2.6"]),
  o("zhipu", tk("智谱 GLM"), "provider", "https://open.bigmodel.cn/api/paas/v4", "https://open.bigmodel.cn/usercenter/apikeys"),
  o("qwen-cn", tk("通义千问（百炼）"), "provider", "https://dashscope.aliyuncs.com/compatible-mode/v1", "https://bailian.console.aliyun.com/?apiKey=1"),
  o("doubao", tk("豆包（火山方舟）"), "provider", "https://ark.cn-beijing.volces.com/api/v3", "https://ai.volcengine.com/console/apikey"),
  o("minimax-cn", "MiniMax", "provider", "https://api.minimaxi.com/v1", "https://platform.minimaxi.com/user-center/basic-information/interface-key"),
  o("stepfun", tk("阶跃星辰"), "provider", "https://api.stepfun.com/v1", "https://platform.stepfun.com/interface-key"),
  o("qianfan", tk("百度千帆"), "provider", "https://qianfan.baidubce.com/v2", "https://console.bce.baidu.com/iam/#/iam/apikey/list"),
  o("hunyuan", tk("腾讯混元"), "provider", "https://api.hunyuan.cloud.tencent.com/v1", "https://console.cloud.tencent.com/hunyuan/api-key"),
  o("mimo", tk("小米 MiMo"), "provider", "https://api.xiaomimimo.com/v1", "https://platform.xiaomimimo.com/"),
  o("xai", "xAI", "provider", "https://api.x.ai/v1", "https://console.x.ai", ["grok-4.5", "grok-4.3"]),
  o("mistral", "Mistral", "provider", "https://api.mistral.ai/v1", "https://console.mistral.ai/api-keys"),
  o("groq", "Groq", "provider", "https://api.groq.com/openai/v1", "https://console.groq.com/keys"),
  o("perplexity", "Perplexity", "provider", "https://api.perplexity.ai", "https://www.perplexity.ai/settings/api", ["sonar", "sonar-pro"]),
  o("nvidia", "NVIDIA", "provider", "https://integrate.api.nvidia.com/v1", "https://build.nvidia.com/settings/api-keys"),
  o("together", "Together AI", "provider", "https://api.together.xyz/v1", "https://api.together.xyz/settings/api-keys"),
  o("fireworks", "Fireworks AI", "provider", "https://api.fireworks.ai/inference/v1", "https://fireworks.ai/account/api-keys"),
  o("cerebras", "Cerebras", "provider", "https://api.cerebras.ai/v1", "https://cloud.cerebras.ai/"),
  // Bilibili's Index-Translate: a free translation model, OpenAI-compatible, no key needed.
  o("bilibili", tk("B 站翻译（免费）"), "provider", "https://index-translate.bilibili.com/v1", "https://index-translate.bilibili.com/site/home.html", ["Index-Translate-35B-A3B"], false),
  o("ollama", "Ollama", "local", "http://127.0.0.1:11434/v1", undefined, ["qwen3.5", "gemma4", "gpt-oss:20b"]),
  o("lmstudio", "LM Studio", "local", "http://127.0.0.1:1234/v1", undefined),
  o("magpie", "Magpie", "local", "http://127.0.0.1:3425/v1", "https://github.com/yetone/magpie"),
  o("custom", tk("其他（OpenAI 兼容）"), "provider", "", undefined, [], true),
];

export const GROUPS: { id: NonNullable<G>; name: string; note: string }[] = [
  { id: "relay", name: tk("一个 Key 用多家模型"), note: tk("中转平台") },
  { id: "provider", name: tk("模型厂商"), note: tk("官方接口") },
  { id: "local", name: tk("本机运行"), note: tk("不需要 Key") },
];

/** The cloud services shown up front (Settings and the episode's pickers); the rest sit behind 「更多」. */
export const CLOUD_ASR_MAIN = ["doubao", "siliconflow", "groq"];

/** Cloud transcription services, ported from Capwise. */
export const ASR_SERVICES: CloudAsr[] = [
  { id: "doubao", name: tk("豆包语音（极速版）"), note: tk("中文最准，5 分钟一段，速度快"), protocol: "doubao", baseUrl: "https://openspeech.bytedance.com/api/v3", model: "bigmodel", models: ["bigmodel"], keyUrl: "https://console.volcengine.com/speech/new/experience/asr", needsKey: true, chunkSecs: 300 },
  // DeepSeek's ASR takes ≤120 s per request and returns text only; 60 s pieces keep the timing close.
  { id: "deepseek", name: "DeepSeek", note: tk("和问答共用同一个 DeepSeek Key"), protocol: "deepseek", baseUrl: "https://api.deepseek.com", model: "asr", models: ["asr"], keyUrl: "https://platform.deepseek.com/api_keys", needsKey: true, chunkSecs: 60 },
  { id: "siliconflow", name: tk("硅基流动"), note: tk("Qwen3-ASR、SenseVoice 等，便宜"), protocol: "openai", baseUrl: "https://api.siliconflow.cn/v1", model: "Qwen/Qwen3-ASR-1.7B", models: ["Qwen/Qwen3-ASR-1.7B", "FunAudioLLM/SenseVoiceSmall", "XingChenAGI/XingChenASR-V3.2"], keyUrl: "https://cloud.siliconflow.cn/account/ak", needsKey: true, chunkSecs: 20 },
  { id: "groq", name: "Groq", note: tk("Whisper large-v3，极快"), protocol: "openai", baseUrl: "https://api.groq.com/openai/v1", model: "whisper-large-v3-turbo", models: ["whisper-large-v3-turbo", "whisper-large-v3"], keyUrl: "https://console.groq.com/keys", needsKey: true, chunkSecs: 600 },
  { id: "openai", name: "OpenAI", note: "Whisper / gpt-4o-transcribe", protocol: "openai", baseUrl: "https://api.openai.com/v1", model: "gpt-4o-mini-transcribe", models: ["whisper-1", "gpt-4o-mini-transcribe", "gpt-4o-transcribe"], keyUrl: "https://platform.openai.com/api-keys", needsKey: true, chunkSecs: 600 },
  { id: "glm", name: tk("智谱 GLM-ASR"), note: tk("中文识别"), protocol: "openai", baseUrl: "https://open.bigmodel.cn/api/paas/v4", model: "glm-asr-2512", models: ["glm-asr-2512"], keyUrl: "https://open.bigmodel.cn/usercenter/proj-mgmt/apikeys", needsKey: true, chunkSecs: 25 },
  { id: "stepfun", name: tk("阶跃星辰"), note: "Step ASR", protocol: "openai", baseUrl: "https://api.stepfun.com/v1", model: "stepaudio-2.5-asr", models: ["stepaudio-2.5-asr", "step-asr"], keyUrl: "https://platform.stepfun.com/interface-key", needsKey: true, chunkSecs: 20 },
  { id: "local-server", name: tk("本机识别服务"), note: tk("OpenAI 兼容的本地服务"), protocol: "openai", baseUrl: "http://127.0.0.1:8765/v1", model: "Qwen/Qwen3-ASR-0.6B", models: ["Qwen/Qwen3-ASR-0.6B", "whisper-1"], needsKey: false, chunkSecs: 60 },
];

/** Doubao seed-tts-2.0 voices (from Capwise). */
export const VOICES: [string, string][] = [
  ["zh_female_vv_uranus_bigtts", "Vivi 2.0"],
  ["zh_male_liufei_uranus_bigtts", tk("刘飞 2.0")],
  ["zh_female_xiaohe_uranus_bigtts", tk("小何 2.0")],
  ["zh_male_m191_uranus_bigtts", tk("云舟 2.0")],
  ["zh_male_taocheng_uranus_bigtts", tk("小天 2.0")],
  ["zh_male_shenyeboke_uranus_bigtts", tk("深夜播客 2.0")],
  ["zh_male_youyoujunzi_uranus_bigtts", tk("悠悠君子 2.0")],
  ["zh_female_shuangkuaisisi_uranus_bigtts", tk("爽快思思 2.0")],
  ["zh_female_cancan_uranus_bigtts", tk("知性灿灿 2.0")],
  ["en_male_tim_uranus_bigtts", "Tim"],
  ["en_female_dacey_uranus_bigtts", "Dacey"],
];

/** Cloud TTS services shown upfront; the rest sit behind 「更多」. */
export const TTS_CLOUD_MAIN = ["doubao", "deepseek", "mimo", "qwen", "openai", "edge"];

const v = (list: [string, string][]): TtsVoice[] => list.map(([id, name]) => ({ id, name }));

export const TTS_SERVICES: TtsService[] = [
  {
    id: "system", name: tk("系统语音"), note: tk("优先使用本机增强语音，完全离线、无需 Key"), group: "local",
    protocol: "system", baseUrl: "", model: "", models: [], needsKey: false,
    voice: "", voices: [],
  },
  {
    id: "local", name: tk("本地语音"), note: tk("在本机离线合成，文字不会离开你的电脑"), group: "local",
    protocol: "local", baseUrl: "", model: "melo", models: ["melo", "kokoro"], needsKey: false,
    voice: "zf_001",
    voices: v([
      ["zf_001", tk("知夏（女声）")],
      ["zf_002", tk("清禾（女声）")],
      ["zf_003", tk("晴岚（女声）")],
      ["zf_004", tk("沐言（女声）")],
      ["zm_009", tk("言川（男声）")],
      ["zm_010", tk("远山（男声）")],
      ["zm_011", tk("景和（男声）")],
      ["zm_012", tk("明朗（男声）")],
    ]),
  },
  {
    id: "doubao", name: tk("豆包语音"), note: tk("中文自然度高，支持复刻音色和原生变速"), group: "cloud",
    protocol: "doubao", baseUrl: "https://openspeech.bytedance.com/api/v3/tts/unidirectional",
    model: "seed-tts-2.0", models: ["seed-tts-2.0"], resource: "seed-tts-2.0",
    keyUrl: "https://console.volcengine.com/speech/new/setting/apikeys?projectName=default",
    needsKey: true, sharedKeyHint: tk("填过豆包语音识别的可以留空"),
    voice: "zh_female_vv_uranus_bigtts",
    voices: v(VOICES),
  },
  {
    id: "deepseek", name: "DeepSeek", note: tk("两种音色，中英文都能读，与 AI 模型共用 Key"), group: "cloud",
    protocol: "deepseek", baseUrl: "https://api.deepseek.com", model: "", models: [],
    keyUrl: "https://platform.deepseek.com/api_keys",
    needsKey: true, sharedKeyHint: tk("填过「AI 模型」里的 DeepSeek 可以留空"),
    voice: "stella",
    voices: v([
      ["stella", tk("Stella（女声）")],
      ["tide", tk("Tide（男声）")],
    ]),
  },
  {
    id: "mimo", name: tk("小米 MiMo"), note: tk("自然播客语调，与 AI 模型共用 Key"), group: "cloud",
    protocol: "mimo", baseUrl: "https://api.xiaomimimo.com/v1",
    model: "mimo-v2.5-tts", models: ["mimo-v2.5-tts"],
    keyUrl: "https://platform.xiaomimimo.com/",
    needsKey: true, sharedKeyHint: tk("填过「AI 模型」里的小米 MiMo 可以留空"),
    voice: "mimo_default",
    // mimo-v2.5-tts's built-in voices (the old v2 ids default_zh / default_en are rejected with HTTP 400).
    voices: v([
      ["mimo_default", tk("默认")],
      ["冰糖", tk("冰糖")], // i18n-ignore (API voice id)
      ["茉莉", tk("茉莉")], // i18n-ignore (API voice id)
      ["苏打", tk("苏打")], // i18n-ignore (API voice id)
      ["白桦", tk("白桦")], // i18n-ignore (API voice id)
      ["Mia", "Mia"],
      ["Chloe", "Chloe"],
      ["Milo", "Milo"],
      ["Dean", "Dean"],
    ]),
  },
  {
    id: "qwen", name: tk("通义 Qwen-TTS"), note: tk("阿里云百炼，音质清晰，与通义千问共用 Key"), group: "cloud",
    protocol: "qwen", baseUrl: "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation",
    model: "qwen3-tts-flash", models: ["qwen3-tts-flash", "qwen-tts"],
    keyUrl: "https://bailian.console.aliyun.com/?apiKey=1",
    needsKey: true, sharedKeyHint: tk("填过「AI 模型」里的通义千问可以留空"),
    voice: "Cherry",
    voices: v([
      ["Cherry", tk("芊悦 Cherry")],
      ["Ethan", tk("晨煦 Ethan")],
      ["Chelsie", tk("千雪 Chelsie")],
      ["Serena", tk("苏瑶 Serena")],
      ["Dylan", tk("晓东 Dylan")],
      ["Jada", tk("阿珍 Jada")],
      ["Sunny", tk("晴儿 Sunny")],
    ]),
  },
  {
    id: "openai", name: "OpenAI", note: tk("官方语音接口，与 OpenAI 模型共用 Key"), group: "cloud",
    protocol: "openai", baseUrl: "https://api.openai.com/v1",
    model: "gpt-4o-mini-tts", models: ["gpt-4o-mini-tts", "tts-1", "tts-1-hd"],
    keyUrl: "https://platform.openai.com/api-keys",
    needsKey: true, sharedKeyHint: tk("填过「AI 模型」里的 OpenAI 可以留空"),
    voice: "alloy",
    voices: v([
      ["alloy", "Alloy"], ["ash", "Ash"], ["coral", "Coral"], ["echo", "Echo"],
      ["nova", "Nova"], ["onyx", "Onyx"], ["sage", "Sage"], ["shimmer", "Shimmer"],
    ]),
  },
  {
    id: "edge", name: tk("Edge 在线语音"), note: tk("无需 API Key，高峰期可能限流"), group: "cloud",
    protocol: "edge", baseUrl: "", model: "", models: [], needsKey: false,
    voice: "zh-CN-YunxiNeural",
    voices: v([
      ["zh-CN-YunxiNeural", tk("云希（男声）")],
      ["zh-CN-XiaoxiaoNeural", tk("晓晓（女声）")],
      ["zh-CN-YunjianNeural", tk("云健（播音）")],
      ["zh-CN-XiaoyiNeural", tk("晓伊（温和）")],
      ["zh-CN-YunyangNeural", tk("云扬（新闻）")],
      ["zh-TW-HsiaoChenNeural", tk("晓臻（台湾）")],
      ["zh-HK-HiuMaanNeural", tk("晓曼（粤语）")],
      ["en-US-AndrewMultilingualNeural", "Andrew"],
      ["en-US-EmmaMultilingualNeural", "Emma"],
    ]),
  },
  {
    id: "siliconflow", name: tk("硅基流动"), note: tk("CosyVoice2 / MOSS，价格低，与硅基流动共用 Key"), group: "cloud",
    protocol: "openai", baseUrl: "https://api.siliconflow.cn/v1",
    model: "FunAudioLLM/CosyVoice2-0.5B", models: ["FunAudioLLM/CosyVoice2-0.5B", "fnlp/MOSS-TTSD-v0.5"],
    keyUrl: "https://cloud.siliconflow.cn/account/ak",
    needsKey: true, sharedKeyHint: tk("填过「AI 模型」里的硅基流动可以留空"),
    voice: "anna",
    voices: v([
      ["anna", tk("Anna（沉稳女声）")],
      ["alex", tk("Alex（沉稳男声）")],
      ["bella", tk("Bella（激情女声）")],
      ["benjamin", tk("Benjamin（低沉男声）")],
      ["claire", tk("Claire（温柔女声）")],
      ["charles", tk("Charles（磁性男声）")],
      ["diana", tk("Diana（欢快女声）")],
      ["david", tk("David（欢快男声）")],
    ]),
  },
  {
    id: "minimax", name: "MiniMax", note: tk("Speech 02 / 2.6，韵律自然，与 MiniMax 共用 Key"), group: "cloud",
    protocol: "minimax", baseUrl: "https://api.minimaxi.com/v1",
    model: "speech-02-hd", models: ["speech-02-hd", "speech-02-turbo", "speech-2.6-hd"],
    keyUrl: "https://platform.minimaxi.com/user-center/basic-information/interface-key",
    needsKey: true, sharedKeyHint: tk("填过「AI 模型」里的 MiniMax 可以留空"),
    voice: "male-qn-qingse",
    voices: v([
      ["male-qn-qingse", tk("青涩青年")],
      ["female-shaonv", tk("少女音色")],
      ["male-qn-jingying", tk("精英青年")],
      ["female-yujie", tk("御姐音色")],
      ["presenter_male", tk("男性主持人")],
      ["presenter_female", tk("女性主持人")],
      ["audiobook_male_1", tk("有声书男声")],
      ["audiobook_female_1", tk("有声书女声")],
    ]),
  },
  {
    id: "elevenlabs", name: "ElevenLabs", note: tk("多语种拟真语音，英文表现突出"), group: "cloud",
    protocol: "elevenlabs", baseUrl: "https://api.elevenlabs.io/v1",
    model: "eleven_multilingual_v2", models: ["eleven_multilingual_v2", "eleven_flash_v2_5", "eleven_turbo_v2_5"],
    keyUrl: "https://elevenlabs.io/app/settings/api-keys",
    needsKey: true,
    voice: "JBFqnCBsd6RMkjVDRZzb",
    voices: v([
      ["JBFqnCBsd6RMkjVDRZzb", "George"],
      ["21m00Tcm4TlvDq8ikWAM", "Rachel"],
      ["EXAVITQu4vr4xnSDxMaL", "Sarah"],
      ["pNInz6obpgDQGcFmaJgB", "Adam"],
      ["Xb7hH8MSUJpSbSDYk0k2", "Alice"],
    ]),
  },
  {
    id: "custom", name: tk("其他（OpenAI 兼容）"), note: tk("兼容 /audio/speech 的云端或本机服务"), group: "cloud",
    protocol: "openai", baseUrl: "http://127.0.0.1:8880/v1",
    model: "tts-1", models: ["tts-1"],
    needsKey: false,
    voice: "alloy",
    voices: [],
  },
];

export const DEFAULT_SYNC: SyncSettings = {
  icloudEnabled: false,
  icloudDir: "",
  webdavEnabled: false,
  webdavUrl: "",
  webdavPath: "/SubCast",
  webdavUsername: "",
  autoSync: true,
  syncProgress: true,
  syncContent: true,
};

export const DEFAULTS: Settings = {
  providers: Object.fromEntries(PRESETS.map((p) => [p.id, p])),
  chatProvider: "deepseek",
  translateProvider: "deepseek",
  translateMode: "auto",
  onboarded: false,
  proxy: "",
  asrModel: "qwen3-asr",
  asrLanguage: "auto",
  mirrorFirst: true,
  autoTranslate: true,
  translateQuality: "fast",
  useShowNotes: true,
  subtitleMode: "bilingual",
  playbackRate: 1,
  asrEngine: "local",
  cloudAsr: Object.fromEntries(ASR_SERVICES.map((a) => [a.id, a])),
  ttsEngine: "system",
  ttsServices: Object.fromEntries(TTS_SERVICES.map((s) => [s.id, s])),
  ttsRate: 1,
  ttsVoice: "zh_female_vv_uranus_bigtts",
  ttsResource: "seed-tts-2.0",
  chatModel: "",
  chatPrompt: "",
  chatPromptOn: true,
  quickPrompts: null,
  uiLang: "auto",
  youtubePromo: true,
  translateShowNames: false,
  autoUpdate: true,
  homeBannerHidden: false,
  sideCurated: true,
  learnOn: false,
  learnLevel: "cet6",
  learnMark: true,
  learnCard: true,
  learnAi: true,
  learnSide: true,
  learnDaily: 20,
  learnRemind: false,
  learnSounds: true,
  learnPauseCard: false,
  learnAsked: false,
  shareTranscripts: true,
  sync: { ...DEFAULT_SYNC },
};

export async function loadSettings(): Promise<Settings> {
  const raw = (await api.getSettings()) as Partial<Settings> | null;
  if (!raw) return structuredClone(DEFAULTS);
  // Keep presets' metadata (key links, suggestions) current while keeping what the user edited.
  const providers: Record<string, Provider> = {};
  for (const p of PRESETS) providers[p.id] = { ...p, ...(raw.providers?.[p.id] ?? {}), keyUrl: p.keyUrl, models: p.models, needsKey: p.needsKey, name: p.name, group: p.group };
  const cloudAsr: Record<string, CloudAsr> = {};
  for (const a of ASR_SERVICES) cloudAsr[a.id] = { ...a, ...(raw.cloudAsr?.[a.id] ?? {}), name: a.name, note: a.note, models: a.models, keyUrl: a.keyUrl, needsKey: a.needsKey, protocol: a.protocol };
  const ttsServices: Record<string, TtsService> = {};
  for (const s of TTS_SERVICES) {
    ttsServices[s.id] = {
      ...s,
      ...(raw.ttsServices?.[s.id] ?? {}),
      name: s.name, note: s.note, group: s.group, protocol: s.protocol,
      models: s.models, voices: s.voices, keyUrl: s.keyUrl, needsKey: s.needsKey, sharedKeyHint: s.sharedKeyHint,
    };
  }
  // Carry legacy Doubao voice/resource forward, and keep existing Doubao users on Doubao when they already set a key.
  if (!raw.ttsServices?.doubao) {
    if (raw.ttsVoice) ttsServices.doubao.voice = raw.ttsVoice;
    if (raw.ttsResource) ttsServices.doubao.resource = raw.ttsResource;
  }
  // MiMo v2.5 dropped the v2 voice ids; a saved default_zh / default_en would fail with HTTP 400.
  const mimo = ttsServices.mimo;
  if (mimo && !/^mimo-v2-tts$/.test(mimo.model.trim()) && /^default_(zh|en)$/.test(mimo.voice)) ttsServices.mimo = { ...mimo, voice: "mimo_default" };
  let ttsEngine = raw.ttsEngine;
  if (!ttsEngine) {
    // Default Chinese & word reading to the OS Enhanced voice (zero rate-limiting, no key); users can still pick cloud engines or Edge manually.
    ttsEngine = "system";
  }
  if (!ttsServices[ttsEngine]) ttsEngine = "system";
  const sync: SyncSettings = { ...DEFAULT_SYNC, ...(raw.sync ?? {}) };
  return migrate({ ...DEFAULTS, ...raw, providers, cloudAsr, ttsEngine, ttsServices, sync });
}

export type SubtitleMode = "bilingual" | "original" | "translation" | "off";

export function subtitleModeKey(ep?: { id: string; showId?: string | null } | null): string | null {
  if (!ep) return null;
  return ep.showId && ep.showId !== "single" ? ep.showId : ep.id;
}

/** Resolves the effective subtitle display mode for a show/episode, falling back to the global default. */
export function subtitleModeFor(settings: Settings, ep?: { id: string; showId?: string | null } | null): SubtitleMode {
  const key = subtitleModeKey(ep);
  return (key && settings.showSubtitleModes?.[key]) || settings.subtitleMode;
}

/** Builds a Settings patch that persists a per-show subtitle mode (or global default when `ep` is absent). */
export function subtitleModePatch(settings: Settings, ep: { id: string; showId?: string | null } | null | undefined, mode: SubtitleMode): Partial<Settings> {
  const key = subtitleModeKey(ep);
  if (!key) return { subtitleMode: mode };
  return { showSubtitleModes: { ...(settings.showSubtitleModes ?? {}), [key]: mode } };
}

/** Builds a Settings patch that sets or clears (when blank) a custom display name for a subscribed show. */
export function showAliasPatch(settings: Settings, showId: string, rawName: string): Partial<Settings> {
  const next = { ...(settings.showAliases ?? {}) };
  const clean = rawName.trim();
  if (clean) next[showId] = clean;
  else delete next[showId];
  return { showAliases: next };
}

/** 0.2.6 removed several switches; carry what each one meant into what remains. Idempotent. */
export function migrate(s: Settings): Settings {
  const out = { ...s };
  // "Always use the model" is now just the default; "always Google" stays as translation assigned to Google.
  if (out.translateMode === "llm") out.translateMode = "auto";
  // Translation is always on; someone who turned it off wanted the original only.
  if (out.autoTranslate === false) { out.subtitleMode = "original"; out.autoTranslate = true; }
  out.useShowNotes = true;
  out.asrLanguage = "auto";
  return out;
}
