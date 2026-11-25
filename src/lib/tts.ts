import { convertFileSrc } from "@tauri-apps/api/core";
import { create } from "zustand";
import { api, type SystemVoice, type TtsRequest } from "./api";
import { TTS_SERVICES, type Settings, type TtsService } from "./settings";
import { useStore } from "../store";
import { t } from "./i18n";

/** Reads text aloud with the active TTS engine, a sentence-aligned chunk at a time, prefetching the next. */
let session = 0;
let audio: HTMLAudioElement | null = null;
// Keep a strong reference while speaking so WebKit doesn't GC the utterance and drop `onend`.
let activeUtterance: SpeechSynthesisUtterance | null = null;
let cancelUtterance: (() => void) | null = null;

const NOVELTY_RE = /\b(Albert|Bad News|Bahh|Bells|Boing|Bubbles|Cellos|Good News|Jester|Junior|Kathy|Organ|Ralph|Superstar|Trinoids|Whisper|Wobble|Zarvox)\b/i;

export const useTtsPreview = create<{ active: string | null }>(() => ({ active: null }));

export function currentTtsService(s: Settings): TtsService {
  return s.ttsServices[s.ttsEngine] ?? s.ttsServices.system ?? TTS_SERVICES[0];
}

export function isKeyError(m: string): boolean {
  return m.includes("Key") || m.includes("API Key");
}

function maxChunkChars(protocol: TtsService["protocol"]): number {
  switch (protocol) {
    case "qwen": return 480;
    case "doubao": return 900;
    case "edge": return 800;
    // One process per chunk on this machine: short chunks get the first words out sooner.
    case "local": case "system": return 280;
    default: return 800;
  }
}

export function chunks(text: string, max = 800): string[] {
  const parts = text.replace(/\s+\n/g, "\n").split(/(?<=[。！？!?；;\n])/);
  const out: string[] = [];
  let cur = "";
  for (const p of parts) {
    if ((cur + p).length > max && cur) { out.push(cur.trim()); cur = ""; }
    cur += p.length > max ? p.slice(0, max) : p;
  }
  if (cur.trim()) out.push(cur.trim());
  return out.filter(Boolean);
}

/** Services with no speed setting: they synthesise at 1× and playback is sped up instead. */
const NO_NATIVE_SPEED = new Set<string>(["deepseek"]);

export function buildTtsRequest(
  s: Settings,
  text: string,
  opts?: { engine?: string; voice?: string; rate?: number },
): TtsRequest {
  const svc = (opts?.engine ? s.ttsServices[opts.engine] : undefined) ?? currentTtsService(s);
  const voice = opts?.voice ?? svc.voice;
  const wanted = opts?.rate ?? s.ttsRate ?? 1;
  const rate = NO_NATIVE_SPEED.has(svc.protocol) ? 1 : wanted;
  return {
    engine: svc.id,
    protocol: svc.protocol,
    baseUrl: svc.baseUrl || undefined,
    model: svc.model || undefined,
    voice,
    resource: svc.resource || undefined,
    text,
    rate,
    needsKey: svc.needsKey,
  };
}

export async function synthesizeText(
  text: string,
  opts?: { engine?: string; voice?: string; rate?: number },
): Promise<{ url: string; path: string; seconds: number; rate: number }> {
  const s = useStore.getState().settings;
  const req = buildTtsRequest(s, text, opts);
  const res = await api.ttsSynthesize(req);
  // `rate`: the speed the audio was made at (1 for services without a speed setting); playback makes up the rest.
  return { url: convertFileSrc(res.path), path: res.path, seconds: res.seconds, rate: req.rate ?? 1 };
}

/** Called whenever reading aloud stops or something else starts speaking (the read-along player listens). */
export const stopHooks = new Set<() => void>();

export function stopSpeaking() {
  for (const h of stopHooks) h();
  session++;
  audio?.pause();
  audio = null;
  activeUtterance = null;
  cancelUtterance?.();
  cancelUtterance = null;
  window.speechSynthesis?.cancel();
  useStore.getState().set({ speaking: false });
  useTtsPreview.setState({ active: null });
}

let audioRate = 1;
/** Playback-rate correction for audio synthesized at `made` when 语速 has changed since. */
const rateFix = (made: number) => Math.min(2, Math.max(0.5, (useStore.getState().settings.ttsRate || 1) / made));
// Changing 语速 while something is being read applies at once.
useStore.subscribe((s, prev) => {
  if (audio && s.settings.ttsRate !== prev.settings.ttsRate) audio.playbackRate = rateFix(audioRate);
});

function playUrl(url: string, madeAt = useStore.getState().settings.ttsRate || 1): Promise<void> {
  const el = new Audio(url);
  audioRate = madeAt;
  el.playbackRate = rateFix(madeAt);
  audio = el;
  return new Promise<void>((resolve, reject) => {
    el.onended = () => resolve();
    el.onerror = () => reject(new Error(t("音频播放失败")));
    void el.play().catch(reject);
  });
}

export async function speak(text: string, onError?: (m: string) => void) {
  stopSpeaking();
  const id = ++session;
  const { settings, player, playing } = useStore.getState();
  if (playing) player?.toggle();
  useStore.getState().set({ speaking: true });
  const svc = currentTtsService(settings);
  const parts = chunks(text, svc.id === "system" ? 240 : maxChunkChars(svc.protocol));
  try {
    if (svc.id === "system" && hasWebSpeechVoices()) {
      for (const part of parts) {
        if (id !== session) return;
        const rate = Math.min(2, Math.max(0.5, useStore.getState().settings.ttsRate || 1));
        await speakSystemAsync(part, { voice: svc.voice, rate, isCanceled: () => id !== session });
      }
      return;
    }
    const fetchPart = (i: number) => {
      if (i >= parts.length) return Promise.resolve(null);
      const rate = useStore.getState().settings.ttsRate || 1;
      return synthesizeText(parts[i], { rate });
    };
    let next = fetchPart(0);
    for (let i = 0; i < parts.length; i++) {
      const res = await next;
      if (id !== session || !res) return;
      next = fetchPart(i + 1);
      await playUrl(res.url, res.rate);
      if (id !== session) return;
    }
  } catch (e) {
    if (id === session) onError?.(String((e as any)?.message ?? e));
  } finally {
    if (id === session) useStore.getState().set({ speaking: false });
  }
}

function sampleLine(voiceId: string): string {
  const v = voiceId.toLowerCase();
  if (v.startsWith("en") || v.includes("en-") || v.includes("en_") || v.includes("default_en") || v === "tim" || v === "dacey" || ["mia", "chloe", "milo", "dean"].includes(v)) {
    return "Hello, this is a voice sample from SubCast.";
  }
  return t("你好，这是 SubCast 朗读音色试听。");
}

/** Synthesises and plays one short sample sentence for the given engine + voice. */
export async function previewVoice(engineId: string, voiceId: string, onError?: (m: string) => void) {
  const key = `${engineId}:${voiceId}`;
  if (useTtsPreview.getState().active === key) {
    stopSpeaking();
    return;
  }
  stopSpeaking();
  const id = ++session;
  useTtsPreview.setState({ active: key });
  try {
    const rate = Math.min(2, Math.max(0.5, useStore.getState().settings.ttsRate || 1));
    if (engineId === "system" && hasWebSpeechVoices()) {
      await speakSystemAsync(sampleLine(voiceId), { voice: voiceId, rate, isCanceled: () => id !== session });
      return;
    }
    const res = await synthesizeText(sampleLine(voiceId), { engine: engineId, voice: voiceId });
    if (id !== session) return;
    await playUrl(res.url, res.rate);
  } catch (e) {
    if (id === session) onError?.(String((e as any)?.message ?? e));
  } finally {
    if (id === session) useTtsPreview.setState({ active: null });
  }
}

/** Scores Web Speech voices so macOS Premium / Neural / Enhanced voices win over robotic compact ones. */
function voiceScore(v: SpeechSynthesisVoice, langPrefix: "en" | "zh"): number {
  const lang = v.lang.toLowerCase();
  if (!lang.startsWith(langPrefix)) return -1;
  if (NOVELTY_RE.test(v.name)) return -1;
  const id = `${v.voiceURI} ${v.name}`.toLowerCase();
  let s = 0;
  if (langPrefix === "en" ? lang.startsWith("en-us") : lang.startsWith("zh-cn")) s += 20;
  if (/premium|gryphon-neural|siri\.natural/.test(id)) s += 100;
  else if (/enhanced/.test(id)) s += 80;
  if (/super-compact/.test(id)) s -= 20;
  if (langPrefix === "en" && /samantha|ava|zoe|allison|alex|daniel|karen|nora/.test(id)) s += 30;
  if (langPrefix === "zh" && /tingting|meijia|sinji/.test(id)) s += 30;
  if (v.localService) s += 5;
  return s;
}

export function hasWebSpeechVoices(): boolean {
  return typeof window !== "undefined" && !!window.speechSynthesis && window.speechSynthesis.getVoices().length > 0;
}

/** Lists installed Web Speech voices (including macOS Premium / Enhanced / Siri voices), ranked by quality. */
export function listWebSystemVoices(): SystemVoice[] {
  const raw = typeof window !== "undefined" && window.speechSynthesis ? window.speechSynthesis.getVoices() : [];
  const picked: { v: SpeechSynthesisVoice; score: number; langGroup: number }[] = [];
  for (const v of raw) {
    const lang = v.lang.toLowerCase();
    const isZh = lang.startsWith("zh");
    const isEn = lang.startsWith("en");
    const isJa = lang.startsWith("ja");
    if (!isZh && !isEn && !isJa) continue;
    if (NOVELTY_RE.test(v.name) || /super-compact/i.test(v.voiceURI)) continue;
    const score = isZh ? voiceScore(v, "zh") : isEn ? voiceScore(v, "en") : 0;
    const langGroup = lang.startsWith("zh-cn") ? 0 : isZh ? 1 : isEn ? 2 : 3;
    picked.push({ v, score, langGroup });
  }
  picked.sort((a, b) => a.langGroup - b.langGroup || b.score - a.score || a.v.name.localeCompare(b.v.name));
  const seen = new Set<string>();
  const out: SystemVoice[] = [];
  for (const { v } of picked) {
    if (seen.has(v.voiceURI)) continue;
    seen.add(v.voiceURI);
    const id = `${v.voiceURI} ${v.name}`.toLowerCase();
    const badge = /premium|gryphon-neural|siri\.natural/.test(id)
      ? t("高级")
      : /enhanced/.test(id)
        ? t("增强")
        : "";
    const label = badge && !v.name.includes(badge) ? `${v.name} · ${badge}` : v.name;
    out.push({ id: v.voiceURI, name: label, lang: v.lang });
  }
  return out;
}

function pickWebVoice(text: string, preferred?: string, fallbackPrefix?: "zh" | "en"): SpeechSynthesisVoice | null {
  const voices = window.speechSynthesis?.getVoices() ?? [];
  if (!voices.length) return null;
  if (preferred) {
    const exact = voices.find((v) => v.voiceURI === preferred || v.name === preferred);
    if (exact) return exact;
  }
  const prefix: "zh" | "en" = fallbackPrefix ?? (/[\u4e00-\u9fff]/.test(text) ? "zh" : "en");
  let best: SpeechSynthesisVoice | null = null;
  let bestScore = -1;
  for (const v of voices) {
    const sc = voiceScore(v, prefix);
    if (sc > bestScore) { bestScore = sc; best = v; }
  }
  return best;
}

/** Speaks one chunk/line using the OS Enhanced/Premium Web Speech voice, resolving when finished or canceled. */
export function speakSystemAsync(
  text: string,
  opts?: { voice?: string; rate?: number; isCanceled?: () => boolean; onBoundary?: (spokenChars: number) => void },
): Promise<void> {
  const synth = window.speechSynthesis;
  if (!synth) return Promise.resolve();
  synth.cancel();
  const trimmed = text.trim();
  if (!trimmed) return Promise.resolve();
  return new Promise<void>((resolve, reject) => {
    let settled = false;
    const finish = (err?: Error) => {
      if (settled) return;
      settled = true;
      if (activeUtterance === u) activeUtterance = null;
      if (cancelUtterance === onCancel) cancelUtterance = null;
      if (err) reject(err); else resolve();
    };
    const onCancel = () => finish();
    const u = new SpeechSynthesisUtterance(trimmed);
    const voice = pickWebVoice(trimmed, opts?.voice);
    if (voice) { u.voice = voice; u.lang = voice.lang; }
    else { u.lang = /[\u4e00-\u9fff]/.test(trimmed) ? "zh-CN" : "en-US"; }
    u.rate = Math.min(2, Math.max(0.5, opts?.rate ?? 1));
    u.onend = () => finish();
    // Each word as it starts; charIndex counts from the start of the trimmed text.
    if (opts?.onBoundary) u.onboundary = (ev) => opts.onBoundary!(text.indexOf(trimmed) + ev.charIndex + (ev.charLength || 1));
    u.onerror = (ev) => {
      if (opts?.isCanceled?.() || ev.error === "canceled" || ev.error === "interrupted") finish();
      else finish(new Error(t("系统语音播放失败")));
    };
    activeUtterance = u;
    cancelUtterance = onCancel;
    synth.speak(u);
  });
}

/** Speaks with the best installed system voice (preferring macOS Premium / Enhanced voices). */
export function speakSystemEnhanced(text: string, lang: "en-US" | "zh-CN" = "en-US", rate = 0.9): void {
  const synth = window.speechSynthesis;
  if (!synth) return;
  synth.cancel();
  const u = new SpeechSynthesisUtterance(text);
  u.lang = lang;
  u.rate = rate;
  const best = pickWebVoice(text, undefined, lang.startsWith("zh") ? "zh" : "en");
  if (best) u.voice = best;
  activeUtterance = u;
  synth.speak(u);
}

function englishVoiceFor(svc: TtsService): string {
  switch (svc.id) {
    case "edge": return svc.voice.toLowerCase().startsWith("en-") ? svc.voice : "en-US-AndrewMultilingualNeural";
    case "doubao": return svc.voice.toLowerCase().startsWith("en_") ? svc.voice : "en_male_tim_uranus_bigtts";
    case "mimo": return /^mimo-v2-tts$/.test(svc.model.trim()) ? "default_en" : ["Mia", "Chloe", "Milo", "Dean"].includes(svc.voice) ? svc.voice : "Mia";
    case "deepseek": return svc.voice || "stella"; // detects the language from the text
    default: return svc.voice;
  }
}

let wordSession = 0;
let wordAudio: HTMLAudioElement | null = null;

/** Pronounces an English word/phrase using the configured cloud TTS (cached on disk), or macOS Enhanced system voice directly (never hits rate-limited Edge). */
export async function speakWord(text: string, rate = 0.9): Promise<void> {
  const id = ++wordSession;
  wordAudio?.pause();
  wordAudio = null;
  window.speechSynthesis?.cancel();

  const svc = currentTtsService(useStore.getState().settings);
  if (svc.group !== "cloud") {
    speakSystemEnhanced(text, "en-US", rate);
    return;
  }
  try {
    const res = await synthesizeText(text, { voice: englishVoiceFor(svc), rate });
    if (id !== wordSession) return;
    const el = new Audio(res.url);
    el.playbackRate = Math.min(2, Math.max(0.5, rate / res.rate));
    wordAudio = el;
    await new Promise<void>((resolve, reject) => {
      el.onended = () => resolve();
      el.onerror = () => reject(new Error("play failed"));
      void el.play().catch(reject);
    });
  } catch {
    if (id === wordSession) speakSystemEnhanced(text, "en-US", rate);
  }
}
