// Read-along: reads an article aloud a sentence at a time and says which sentence and which characters are being
// spoken, so the page can highlight them, jump to any sentence and pick up where the reader left off.
// After qiaomu-audio's reading controller: one sentence synthesised ahead, finished sentences kept for jumping back.
import { create } from "zustand";
import { useStore } from "../store";
import { currentTtsService, hasWebSpeechVoices, speakSystemAsync, stopHooks, stopSpeaking, synthesizeText } from "./tts";
import { estimateMarks, spokenChars, type Mark } from "./sentences";
import { t } from "./i18n";

export type ReadState = "idle" | "loading" | "playing" | "paused" | "ended" | "error";

interface ReadAlong {
  /** What is being read (e.g. "deep:<episode id>"); null when nothing is. */
  owner: string | null;
  sentences: string[];
  index: number;
  state: ReadState;
  /** Characters of the current sentence already spoken (estimated unless the engine reports words). */
  spoken: number;
  error: string | null;
}

export const useReadAlong = create<ReadAlong>(() => ({ owner: null, sentences: [], index: 0, state: "idle", spoken: 0, error: null }));
const set = (p: Partial<ReadAlong>) => useReadAlong.setState(p);

interface Synth { url: string; rate: number }
let gen = 0;
let audio: HTMLAudioElement | null = null;
let raf = 0;
/** The speed the current audio was made at. */
let madeAt = 1;
const cache = new Map<string, Promise<Synth>>();

const progressKey = (owner: string) => `subcast-read:${owner}`;
/** Where the reader stopped last time, if inside the article. */
export function resumeIndex(owner: string, total: number): number {
  try { const i = Number(localStorage.getItem(progressKey(owner))); return i > 0 && i < total ? i : 0; } catch { return 0; }
}
function remember(owner: string, index: number | null) {
  try { if (index == null) localStorage.removeItem(progressKey(owner)); else localStorage.setItem(progressKey(owner), String(index)); } catch { /* private mode */ }
}

const usesWebSpeech = () => currentTtsService(useStore.getState().settings).id === "system" && hasWebSpeechVoices();
const rateNow = () => useStore.getState().settings.ttsRate || 1;
/** Playback-rate correction for audio made at `made` when 语速 changed since (or the service has no speed). */
const rateFix = (made: number) => Math.min(2, Math.max(0.5, rateNow() / made));

function synth(text: string): Promise<Synth> {
  const s = useStore.getState().settings;
  const svc = currentTtsService(s);
  const key = `${svc.id}|${svc.voice}|${svc.model ?? ""}|${text}`;
  let p = cache.get(key);
  if (!p) {
    p = synthesizeText(text, { rate: rateNow() }).then((r) => ({ url: r.url, rate: r.rate }));
    p.catch(() => cache.delete(key));
    cache.set(key, p);
    if (cache.size > 200) cache.delete(cache.keys().next().value!);
  }
  return p;
}

/** Stops sound without changing the state (the caller sets it). */
function halt() {
  gen++;
  cancelAnimationFrame(raf);
  if (audio) { audio.onended = null; audio.onerror = null; audio.pause(); audio = null; }
  if (usesWebSpeech() || window.speechSynthesis?.speaking) window.speechSynthesis?.cancel();
}

async function playFrom(i: number) {
  const { owner, sentences } = useReadAlong.getState();
  if (!owner) return;
  halt();
  const g = gen;
  if (i >= sentences.length) { set({ state: "ended", index: sentences.length - 1, spoken: 0 }); remember(owner, null); return; }
  set({ index: i, spoken: 0, state: "loading", error: null });
  remember(owner, i);
  const text = sentences[i];
  const next = () => { if (g === gen) void playFrom(i + 1); };
  const fail = (e: unknown) => { if (g === gen) set({ state: "error", error: String((e as Error)?.message ?? e) }); };
  try {
    if (usesWebSpeech()) {
      set({ state: "playing" });
      const svc = currentTtsService(useStore.getState().settings);
      await speakSystemAsync(text, { voice: svc.voice, rate: Math.min(2, Math.max(0.5, rateNow())), isCanceled: () => g !== gen, onBoundary: (n) => { if (g === gen) set({ spoken: n }); } });
      next();
      return;
    }
    const res = await synth(text);
    if (g !== gen) return;
    // The next sentence is made while this one plays.
    if (i + 1 < sentences.length) synth(sentences[i + 1]).catch(() => {});
    const el = new Audio(res.url);
    audio = el;
    el.playbackRate = rateFix(res.rate);
    madeAt = res.rate;
    let marks: Mark[] = [];
    el.onloadedmetadata = () => { marks = estimateMarks(text, el.duration); };
    el.onended = () => { set({ spoken: text.length }); next(); };
    el.onerror = () => fail(new Error(t("音频播放失败")));
    const tick = () => {
      if (g !== gen) return;
      if (marks.length) { const n = spokenChars(marks, el.currentTime); if (n !== useReadAlong.getState().spoken) set({ spoken: n }); }
      raf = requestAnimationFrame(tick);
    };
    await el.play();
    if (g !== gen) { el.pause(); return; }
    set({ state: "playing" });
    raf = requestAnimationFrame(tick);
  } catch (e) { fail(e); }
}
/** Starts reading `sentences` for `owner` at sentence `index`. */
export function startReading(owner: string, sentences: string[], index = 0) {
  stopSpeaking(); // anything else reading aloud (and an earlier read-along) stops
  const { player, playing } = useStore.getState();
  if (playing) player?.toggle();
  set({ owner, sentences, index, state: "loading", spoken: 0, error: null });
  void playFrom(index);
}

/** Reads from sentence `i` (clicking a sentence, previous/next). */
export function seekReading(i: number) {
  const { owner, sentences } = useReadAlong.getState();
  if (!owner) return;
  const { player, playing } = useStore.getState();
  if (playing) player?.toggle();
  void playFrom(Math.max(0, Math.min(sentences.length - 1, i)));
}

export function toggleReading() {
  const { state, index } = useReadAlong.getState();
  if (state === "playing" || state === "loading") {
    // Web Speech can't pause reliably in WebKit: pausing ends the sentence and resuming starts it again.
    if (audio && state === "playing") { audio.pause(); cancelAnimationFrame(raf); set({ state: "paused" }); return; }
    halt();
    set({ state: "paused", spoken: 0 });
    return;
  }
  if (state === "paused" && audio) {
    const { player, playing } = useStore.getState();
    if (playing) player?.toggle();
    const el = audio, g = gen;
    void el.play().then(() => {
      if (g !== gen) return;
      set({ state: "playing" });
      resumeTicker(el, g);
    }).catch((e) => set({ state: "error", error: String(e?.message ?? e) }));
    return;
  }
  seekReading(state === "ended" ? 0 : index);
}

function resumeTicker(el: HTMLAudioElement, g: number) {
  const { sentences, index } = useReadAlong.getState();
  const marks = estimateMarks(sentences[index] ?? "", el.duration);
  cancelAnimationFrame(raf);
  const tick = () => {
    if (g !== gen || el.paused) return;
    const n = spokenChars(marks, el.currentTime);
    if (n !== useReadAlong.getState().spoken) set({ spoken: n });
    raf = requestAnimationFrame(tick);
  };
  raf = requestAnimationFrame(tick);
}

export function stopReading() {
  halt();
  set({ owner: null, sentences: [], index: 0, state: "idle", spoken: 0, error: null });
}

// Something else started speaking (another 朗读 button, a word lookup): this stops.
stopHooks.add(() => { if (useReadAlong.getState().owner) stopReading(); });

// The episode starting to play pauses the reading; changing 语速 applies at once.
useStore.subscribe((s, prev) => {
  if (s.playing && !prev.playing && useReadAlong.getState().state === "playing") toggleReading();
  if (audio && s.settings.ttsRate !== prev.settings.ttsRate) audio.playbackRate = rateFix(madeAt);
});
