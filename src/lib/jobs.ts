// Long-running work that outlives any one screen: transcription and translation of an episode.
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { api, rawErrorText, type AsrEvent, type Episode, type Provider, type Segment, type Transcript } from "./api";
import { batches, batchShape, makeEpisodeContext, translateBatch, type Batch, type EpisodeContext } from "./ai";
import { isChinese } from "./format";
import { providerFor, useStore } from "../store";
import { subtitleModeFor } from "./settings";
import { refreshSaved } from "./offline";

// Messages and errors are kept as the backend wrote them (so they can be matched) and translated with tr() where shown.
export interface AsrJob { stage: "download" | "transcribe" | "done" | "error"; progress: number; message?: string; live: Segment[] }
/** youtube: the publisher's own Chinese subtitles, paired with the original lines by the backend. */
export type TranslateEngine = "llm" | "google" | "youtube" | "shared";
export interface TranslationJob { items: (string | null)[]; running: boolean; error?: string; failed: number; engine?: TranslateEngine }

interface Jobs {
  transcripts: Record<string, Transcript | null | undefined>;
  officialChecked: Record<string, "searching" | "none" | "error" | "found">;
  officialError: Record<string, string>;
  asr: Record<string, AsrJob>;
  translations: Record<string, TranslationJob>;
}

export const useJobs = create<Jobs>(() => ({ transcripts: {}, officialChecked: {}, officialError: {}, asr: {}, translations: {} }));
const patch = <K extends keyof Jobs>(key: K, id: string, value: Jobs[K][string]) => useJobs.setState((s) => ({ [key]: { ...s[key], [id]: value } }) as Partial<Jobs>);

let listening = false;
export function startJobListeners() {
  if (listening) return;
  listening = true;
  listen<AsrEvent>("asr", ({ payload: p }) => {
    const job = useJobs.getState().asr[p.episodeId] ?? { stage: "download", progress: 0, live: [] };
    if (p.stage === "segment" && p.segment) patch("asr", p.episodeId, { ...job, live: [...job.live, p.segment] });
    else patch("asr", p.episodeId, { ...job, stage: p.stage as AsrJob["stage"], progress: p.progress, message: p.message ?? undefined });
  });
}

/** Cached transcript, else the publisher's subtitles, else the shared library: all before an episode is called
 *  "no subtitles", so nobody downloads a model for an episode someone already transcribed. */
export async function loadTranscript(ep: Episode) {
  const s = useJobs.getState();
  if (s.transcripts[ep.id] || s.officialChecked[ep.id] === "searching") return;
  const cached = await api.loadTranscript(ep.id);
  // Before 0.2.1 a Chinese video that also had English subtitles opened in English: fetch those again.
  const stale = cached?.source.startsWith("youtube") && /[\u4e00-\u9fff].*[\u4e00-\u9fff]/.test(ep.title) && !/[\u3040-\u30ff]/.test(ep.title) && !cached.language.toLowerCase().startsWith("zh");
  if (cached && (!stale || (await api.getBlob(ep.id, "track-picked")))) { patch("transcripts", ep.id, cached); return; }
  patch("officialChecked", ep.id, "searching");
  const fromShared = async () => {
    const sh = await api.sharedTranscript(ep.id).catch(() => null);
    if (sh) { patch("transcripts", ep.id, sh); patch("officialChecked", ep.id, "found"); return true; }
    return false;
  };
  if (!ep.videoId && !ep.transcriptUrl && ep.transcriptType !== "qiaomu") { if (!(await fromShared())) patch("officialChecked", ep.id, "none"); return; }
  try {
    const t = await api.fetchOfficialTranscript(ep.id);
    if (t) {
      patch("transcripts", ep.id, t); patch("officialChecked", ep.id, "found");
      // Still not Chinese (a Chinese title over an English video): settled, don't check again on every open.
      if (stale && !t.language.toLowerCase().startsWith("zh")) void api.setBlob(ep.id, "track-picked", "auto");
    } else if (cached) { patch("transcripts", ep.id, cached); patch("officialChecked", ep.id, "found"); }
    else if (!(await fromShared())) patch("officialChecked", ep.id, "none");
  } catch (e) {
    if (cached) { patch("transcripts", ep.id, cached); patch("officialChecked", ep.id, "found"); return; }
    if (await fromShared()) return;
    patch("officialError", ep.id, rawErrorText(e));
    patch("officialChecked", ep.id, "error");
  }
}

export async function generateTranscript(ep: Episode, model: string, language: string) {
  patch("asr", ep.id, { stage: "download", progress: 0, live: [] });
  try {
    const { settings } = useStore.getState();
    const cloud = settings.asrEngine !== "local" ? settings.cloudAsr[settings.asrEngine] : undefined;
    const t = await api.transcribe(ep.id, model, language, cloud);
    patch("transcripts", ep.id, t);
    patch("translations", ep.id, { items: [], running: false, failed: 0 });
    void useStore.getState().reloadShows();
    // The audio it was made from is kept to play from (its times fit it exactly).
    void refreshSaved();
  } catch (e) {
    const job = useJobs.getState().asr[ep.id];
    patch("asr", ep.id, { ...(job ?? { live: [], progress: 0 }), stage: "error", message: rawErrorText(e) });
  }
}

const controllers = new Map<string, AbortController>();
const saveTimers = new Map<string, number>();

export async function loadTranslation(id: string, total: number) {
  if (useJobs.getState().translations[id]) return;
  const raw = await api.getBlob(id, "zh");
  let items: (string | null)[] = [];
  try { items = raw ? JSON.parse(raw) : []; } catch { items = []; }
  if (items.length !== total) items = Array.from({ length: total }, (_, i) => items[i] ?? null);
  const engine = ((await api.getBlob(id, "zh-engine")) as TranslateEngine | null) ?? undefined;
  patch("translations", id, { items, running: false, failed: 0, engine });
}

export function needsTranslation(t: Transcript) {
  return !isChinese(t.language, t.segments.slice(0, 5).map((s) => s.text).join(""));
}

/** Whether the user's own model is ready for translation; otherwise free Google translation is used. */
export async function llmReady(use: "chat" | "translate"): Promise<boolean> {
  const settings = useStore.getState().settings;
  if (use === "translate" && settings.translateMode === "google") return false;
  const p = providerFor(settings, use);
  // A local agent picks its own model when none is set.
  if (p?.protocol === "cli") return true;
  return Boolean(p?.model && p.baseUrl !== "" && (!p.needsKey || (await api.hasApiKey(p.id))));
}

/** How many requests at once, by provider. Adaptive: halved on a rate limit, grown back after a run of successes. */
function concurrency(p?: Provider | null): { start: number; max: number } {
  if (!p) return { start: 3, max: 6 };
  if (p.group === "local") return { start: 1, max: 1 };
  if (p.protocol === "cli") return { start: 2, max: 2 };
  if (p.protocol === "anthropic") return { start: 2, max: 4 };
  if (/deepseek/i.test(p.id + p.baseUrl)) return { start: 4, max: 6 };
  return { start: 3, max: 6 };
}

const RATE_LIMIT = /太频繁|429|rate.?limit|overload|繁忙|busy/i;
const FATAL = /API Key|权限|不存在|连不上|网络|余额|insufficient/i;
const sleep = (ms: number, signal?: AbortSignal) => new Promise<void>((res) => { const t = window.setTimeout(res, ms); signal?.addEventListener("abort", () => { window.clearTimeout(t); res(); }); });

/** The episode's summary and glossary for translation (blob "ctx"), made once per model and notes; the user's own
 *  terms are kept when it is remade. Null when it can't be made: translation goes on without it. */
export async function episodeContext(ep: Episode, t: Transcript, provider: Provider, signal?: AbortSignal): Promise<EpisodeContext | null> {
  const notes = useStore.getState().settings.useShowNotes ? ((await api.fullNotes(ep.id).catch(() => null)) ?? ep.notes ?? "") : "";
  const basis = `${provider.model}|${ep.title}|${notes.length}|${t.segments.length}`;
  let old: EpisodeContext | null = null;
  try { const raw = await api.getBlob(ep.id, "ctx"); old = raw ? JSON.parse(raw) : null; } catch { old = null; }
  if (old?.basis === basis) return old;
  try {
    const made = await makeEpisodeContext(provider, { title: ep.title, show: ep.showTitle ?? "", notes, segments: t.segments, signal });
    if (!made) return old;
    const mine = (old?.terms ?? []).filter((x) => x.user);
    made.terms = [...mine, ...made.terms.filter((x) => !mine.some((m) => m.src.toLowerCase() === x.src.toLowerCase()))].slice(0, 30);
    made.basis = basis;
    await api.setBlob(ep.id, "ctx", JSON.stringify(made));
    return made;
  } catch { return old; }
}

export async function translateEpisode(ep: Episode, t: Transcript, force?: TranslateEngine) {
  const { settings } = useStore.getState();
  const provider = providerFor(settings, "translate");
  const engine: TranslateEngine = force ?? ((await llmReady("translate")) ? "llm" : "google");
  await loadTranslation(ep.id, t.segments.length);
  const job = useJobs.getState().translations[ep.id];
  if (job.running) return;
  const controller = new AbortController();
  controllers.set(ep.id, controller);
  const signal = controller.signal;
  // Switching engine retranslates everything so one episode never mixes two translators. The publisher's own
  // Chinese and shared translations are the exception: they stay, and only the gaps are machine-translated.
  const keep = (job.engine === "youtube" || job.engine === "shared") && force == null;
  let items = job.engine && job.engine !== engine && !keep ? t.segments.map(() => null) : [...job.items];
  const label: TranslateEngine = keep ? job.engine! : engine;
  patch("translations", ep.id, { items, running: true, failed: 0, engine: label });
  void api.setBlob(ep.id, "zh-engine", label);
  const save = () => {
    window.clearTimeout(saveTimers.get(ep.id));
    saveTimers.set(ep.id, window.setTimeout(() => void api.setBlob(ep.id, "zh", JSON.stringify(useJobs.getState().translations[ep.id].items)), 800));
  };
  const merge = (result: Map<number, string>) => {
    items = [...useJobs.getState().translations[ep.id].items];
    for (const [i, text] of result) items[i] = text;
    patch("translations", ep.id, { ...useJobs.getState().translations[ep.id], items });
    save();
  };

  // The glossary takes a model call (seconds; more with a local CLI). The lines on screen don't wait for it: the
  // first batch goes at once without it, the rest once it is there.
  const ctxReady = engine === "llm" && provider ? episodeContext(ep, t, provider, signal) : Promise.resolve(null);
  let ctx: EpisodeContext | null = null, ctxDone = engine !== "llm";
  void ctxReady.then((c) => { ctx = c; ctxDone = true; pumpNow(); });
  const opts = () => ({ title: ep.title, show: ep.showTitle ?? "", ctx, signal, polish: settings.translateQuality === "best" });
  const queue = batches(t.segments, items, engine === "llm" ? batchShape(provider) : undefined);
  const tries = new Map<number, number>();
  // The batch nearest to where the listener is (ahead of the playhead counts double), so the lines on screen
  // come first; a seek changes the order at once.
  const next = (): Batch | undefined => {
    const s = useStore.getState();
    const now = s.current?.id === ep.id ? s.time : 0;
    let best = -1, bestCost = Infinity;
    queue.forEach((b, i) => {
      const at = t.segments[b.ids[0]].start, end = t.segments[b.ids[b.ids.length - 1]].end;
      const cost = end < now ? (now - end) * 2 : Math.max(0, at - now);
      if (cost < bestCost) { bestCost = cost; best = i; }
    });
    return best < 0 ? undefined : queue.splice(best, 1)[0];
  };
  const limits = engine === "google" ? { start: 2, max: 2 } : concurrency(provider);
  let limit = limits.start, active = 0, streak = 0, limited = 0;
  let failed = 0, lastError = "";
  const one = async (b: Batch) => {
    try {
      if (engine === "google") {
        const out = await api.translateTexts(b.ids.map((i) => t.segments[i].text));
        merge(new Map(b.ids.map((id, j) => [id, out[j] ?? ""])));
      } else {
        const got = await translateBatch(provider!, t.segments, b, opts());
        merge(got);
        // Lines that didn't come back go round again on their own; after three tries a line is given up.
        const missing = b.ids.filter((id) => !got.has(id) && (tries.set(id, (tries.get(id) ?? 0) + 1).get(id) ?? 0) < 3);
        for (let i = 0; i < missing.length; i += 6) queue.push({ ids: missing.slice(i, i + 6) });
        failed += b.ids.filter((id) => !got.has(id) && (tries.get(id) ?? 0) >= 3).length;
      }
      limited = 0;
      if (++streak >= 20 && limit < limits.max) { limit++; streak = 0; }
    } catch (err) {
      if (signal.aborted) return;
      lastError = rawErrorText(err);
      // A 429 is mostly "too many at once"; six in a row without a success means the quota is gone.
      if (RATE_LIMIT.test(lastError) && limited < 6) {
        // Rate limited: fewer at once, wait (2, 4, 8… s with jitter), and the batch goes back to the front.
        limited++; streak = 0; limit = Math.max(1, Math.floor(limit / 2));
        queue.unshift(b);
        await sleep(1000 * 2 ** limited + Math.random() * 1000, signal);
        return;
      }
      if (FATAL.test(lastError) || RATE_LIMIT.test(lastError)) { controller.abort(); return; }
      // Anything else: split once into halves, then give the lines up.
      if (b.ids.length >= 4) { const h = Math.ceil(b.ids.length / 2); queue.push({ ids: b.ids.slice(0, h) }, { ids: b.ids.slice(h) }); }
      else failed += b.ids.length;
    }
  };
  let pumpNow = () => {};
  let started = 0;
  await new Promise<void>((done) => {
    const pump = () => {
      if (signal.aborted && active === 0) return done();
      // Before the glossary is ready only one batch runs (the one nearest the playhead).
      while (!signal.aborted && active < (ctxDone ? limit : 1) && queue.length && (ctxDone || started === 0)) {
        started++;
        const b = next()!;
        active++;
        void one(b).finally(() => { active--; pump(); });
      }
      if (active === 0 && (!queue.length || signal.aborted) && ctxDone) done();
      if (active === 0 && signal.aborted) done();
    };
    pumpNow = pump;
    pump();
  });
  controllers.delete(ep.id);
  const final = useJobs.getState().translations[ep.id];
  await api.setBlob(ep.id, "zh", JSON.stringify(final.items));
  patch("translations", ep.id, { ...final, running: false, failed, error: failed || signal.aborted ? lastError || undefined : undefined });
  // A finished machine translation goes to the shared library with the transcript (when sharing is on), so the
  // next listener of this episode opens it bilingual at once.
  if (!signal.aborted && (engine === "llm" || engine === "google") && !keep && final.items.every((x) => x != null)) {
    void api.shareTranslation(ep.id, engine, engine === "llm" ? provider?.model ?? "" : "").catch(() => {});
  }
}

/** Starts translating a foreign-language episode as soon as its transcript is there, whichever tab is open:
 *  the subtitles on the video need the Chinese lines too. */
export function autoTranslate(ep: Episode) {
  const t = useJobs.getState().transcripts[ep.id];
  const subMode = subtitleModeFor(useStore.getState().settings, ep);
  if (!t || subMode === "original" || !needsTranslation(t)) return;
  void loadTranslation(ep.id, t.segments.length).then(() => {
    const job = useJobs.getState().translations[ep.id];
    if (!job || job.running || job.error || job.items.every((x) => x != null)) return;
    void translateEpisode(ep, t).catch((e) => patch("translations", ep.id, { ...useJobs.getState().translations[ep.id], running: false, error: rawErrorText(e) }));
  });
}

/** Shows the video's subtitles in another of its YouTube languages. */
export async function switchYoutubeTrack(ep: Episode, language: string, auto: boolean) {
  stopTranslation(ep.id);
  const t = await api.useYoutubeTrack(ep.id, language, auto);
  useJobs.setState((s) => { const translations = { ...s.translations }; delete translations[ep.id]; return { translations, transcripts: { ...s.transcripts, [ep.id]: t } }; });
  autoTranslate(ep);
}

export function stopTranslation(id: string) {
  controllers.get(id)?.abort();
}
