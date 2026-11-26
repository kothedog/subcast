import { useShallow } from "zustand/react/shallow";
import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { openUrl } from "@tauri-apps/plugin-opener";
import { Check, ExternalLink, Loader2, Maximize2, Minimize2, Pause, Play, RotateCcw, RotateCw, Volume1, Volume2, VolumeX } from "lucide-react";
import { followVolume, gainOf, setLevel, toggleMute, useVolume } from "../lib/volume";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, browserLocked, isBotCheck, needsBrowserAccess, signInExpired, type Episode } from "../lib/api";
import { useYouTubeSignIn } from "./YouTubeAccount";
import { BrowserAccess } from "./BrowserAccess";
import { clock } from "../lib/format";
import { loadYouTube } from "../lib/youtube";
import { offerPlayer, useStore, withdrawPlayer } from "../store";
import { useClipTools } from "../lib/clipTools";
import { subtitleModeFor, subtitleModePatch } from "../lib/settings";
import { IS_MAC, t, tr } from "../lib/i18n";
import { audioSrc, noteCached, useOffline } from "../lib/offline";
import { alignCurrent, toContent, toPlay } from "../lib/timemap";
import { useJobs } from "../lib/jobs";
import { convertFileSrc } from "@tauri-apps/api/core";
import { playNext } from "../lib/library";
import { markEnded, notePlaybackProgress } from "../lib/watched";
import { useZh } from "../lib/zh";
import { SeekHighlights } from "./HighlightViews";

const SAVE_EVERY = 10;
/** Starts an episode as soon as its player exists: right away when it does, or when it appears (a YouTube
 *  player takes a moment to load). Replaces "wait 400 ms and hope". */
export function playSoon(id: string) {
  const s = useStore.getState();
  waiting(true);
  if (s.current?.id === id && s.player) { s.player.play(); return; }
  s.set({ playWhenReady: id });
}
useStore.subscribe((s, prev) => {
  if (!s.player || s.player === prev.player) return;
  const p = s.player;
  // A seek asked for before the player existed (a chapter link in the notes) starts playback from there.
  if (s.seekWhenReady && s.seekWhenReady.id === s.current?.id) {
    const { at } = s.seekWhenReady;
    const play = s.playWhenReady === s.current?.id;
    useStore.setState({ seekWhenReady: null, playWhenReady: null });
    window.setTimeout(() => { p.seek(at); if (play) p.play(); }, 0);
  } else if (s.playWhenReady && s.current?.id === s.playWhenReady) {
    useStore.setState({ playWhenReady: null });
    window.setTimeout(() => p.play(), 0);
  }
});

// Videos whose owners forbid embedding, remembered so they go straight to the direct stream next time
// instead of waiting for the embedded player to load and refuse.
const BLOCKED_KEY = "subcast-yt-blocked";
function blockedIds(): string[] { try { return JSON.parse(localStorage.getItem(BLOCKED_KEY) ?? "[]"); } catch { return []; } }
export const wasBlocked = (videoId?: string) => !!videoId && blockedIds().includes(videoId);
function markBlocked(videoId?: string) {
  if (!videoId || wasBlocked(videoId)) return;
  try { localStorage.setItem(BLOCKED_KEY, JSON.stringify([...blockedIds(), videoId].slice(-500))); } catch { /* storage unavailable */ }
}

// Videos the listener switched to the original audio (YouTube's embedded player picks an AI dub by region or
// language and has no audio menu here): played through the direct stream, which keeps only the original track.
const ORIGINAL_KEY = "subcast-yt-original";
function originalIds(): string[] { try { return JSON.parse(localStorage.getItem(ORIGINAL_KEY) ?? "[]"); } catch { return []; } }
export const wantsOriginalAudio = (videoId?: string) => !!videoId && originalIds().includes(videoId);
/** Asks the video on screen to switch to its original audio (SubtitleTune's "换成原声"). */
export function switchToOriginalAudio(videoId?: string) {
  if (!videoId) return;
  if (!wantsOriginalAudio(videoId)) {
    try { localStorage.setItem(ORIGINAL_KEY, JSON.stringify([...originalIds(), videoId].slice(-500))); } catch { /* storage unavailable */ }
  }
  window.dispatchEvent(new CustomEvent("subcast-original-audio", { detail: videoId }));
}

/** Playback asked for, sound not there yet: the controls show it, and say more if it takes long. */
export const waiting = (on: boolean) => { const s = useStore.getState(); if (on ? !s.loadingSince : s.loadingSince) s.set({ loadingSince: on ? Date.now() : null }); };
/** The play/pause button: while playback is still loading it cancels the request (it shows a pause on hover),
 *  even before a player exists (the direct stream is still being fetched). */
export function pressPlay() {
  const s = useStore.getState();
  if (s.loadingSince && !s.player) { s.set({ loadingSince: null, playWhenReady: null, playing: false }); return; }
  s.player?.toggle();
}
/** The current episode is loading and has been for at least `after` ms (re-renders when that moment comes). */
/** A click on a video (its poster or picture): this episode's player toggles; before it exists (still loading, or
 *  another episode holds the controls) the episode is loaded and plays as soon as its player is ready. */
function toggleVideo(ep: Episode) {
  const s = useStore.getState();
  if (s.player && s.player.episodeId === ep.id) { s.player.toggle(); return; }
  s.load(ep);
  playSoon(ep.id);
}
export function useLoadingFor(id: string | undefined, after = 0): boolean {
  const since = useStore((s) => (id && s.current?.id === id ? s.loadingSince ?? null : null));
  const [, tick] = useState(0);
  useEffect(() => {
    if (!since || !after) return;
    const left = since + after - Date.now();
    if (left <= 0) return;
    const timer = window.setTimeout(() => tick((n) => n + 1), left + 50);
    return () => window.clearTimeout(timer);
  }, [since, after]);
  return !!since && Date.now() - since >= after;
}
/** Spinner that turns into a pause sign on hover: loading, and a click cancels it. */
export function LoadingIcon({ size }: { size: number }) {
  return <span className="loading-icon"><Loader2 size={size} className="spin" /><Pause size={size} fill="currentColor" className="loading-cancel" /></span>;
}
let lastSaved = 0;
// Listening time: wall-clock time while playing, at most 2 s per tick (a seek or a sleeping laptop adds
// nothing), flushed every 20 s and on pause.
let heard = { id: "", show: "", secs: 0, at: 0, translated: false };
function flushHeard() {
  if (heard.secs >= 1) void api.logListen(heard.id, heard.show, heard.secs, heard.translated).catch(() => {});
  heard.secs = 0;
}
export function noteListening(ep: Episode, playing: boolean) {
  const now = performance.now();
  if (heard.id !== ep.id) { flushHeard(); heard = { id: ep.id, show: ep.showId, secs: 0, at: now, translated: false }; }
  if (playing && heard.at) heard.secs += Math.min(2, (now - heard.at) / 1000);
  heard.at = playing ? now : 0;
  const subMode = subtitleModeFor(useStore.getState().settings, ep);
  heard.translated ||= subMode !== "original" && subMode !== "off";
  if (heard.secs >= 20 || (!playing && heard.secs > 0)) flushHeard();
}

/** The current episode's time map, when its subtitles were made from another cut of the audio. */
const mapOf = (id: string) => { const a = useStore.getState().align; return a?.id === id ? a.map : null; };

/** `raw` is the audio element's clock; the store keeps subtitle time (src/lib/timemap.ts). Returns where the stretch
 *  only this audio has ends, while inside one. */
function track(ep: Episode, raw: number, length?: number): number | undefined {
  const s = useStore.getState();
  if (s.current?.id !== ep.id) return;
  noteListening(ep, s.playing);
  const { at: time, gapEnd } = toContent(mapOf(ep.id), raw);
  s.set({ time, rawTime: raw, gapEnd: gapEnd ?? null, ...(length ? { length } : {}) });
  if (Math.abs(time - lastSaved) > SAVE_EVERY) {
    lastSaved = time;
    void api.setProgress(ep.id, time);
    notePlaybackProgress(ep, time, length || ep.duration);
  }
  return gapEnd;
}

/** Local copies that failed to play (deleted from the cache, or not readable): the original address is used. */
const brokenLocal = new Set<string>();
/** Where to play an episode from, skipping a local copy that already failed. */
function playable(ep: Episode): string | undefined {
  const s = audioSrc(ep);
  return s && brokenLocal.has(s) ? ep.audioUrl : s;
}
/** What failed, for the note the listener sees (MediaError codes). */
function mediaProblem(code?: number): string {
  if (code === 2) return t("网络中断");
  if (code === 3) return t("音频无法解码");
  return t("地址打不开或格式不支持");
}

/** The audio element lives at the app root so playback continues while browsing. */
export function AudioEngine() {
  const current = useStore((s) => s.current);
  const rate = useStore((s) => s.settings.playbackRate);
  const ref = useRef<HTMLAudioElement>(null);
  const audio = current && !current.videoId ? current : null;
  const saved = useOffline((s) => (audio ? (s.saved[audio.id] ?? s.cached[audio.id])?.path : undefined));
  // The address is fixed per episode so a download finishing mid-episode does not cut in; it takes over at the next pause.
  const [chosen, setChosen] = useState<{ id: string; src?: string } | null>(null);
  const src = audio ? (chosen?.id === audio.id ? chosen.src : playable(audio)) : undefined;
  const setSrc = (s?: string) => audio && setChosen({ id: audio.id, src: s });
  useEffect(() => { if (audio && chosen?.id !== audio.id) setSrc(src); }, [audio?.id]);
  useEffect(() => { if (audio && saved && ref.current?.paused !== false) setSrc(playable(audio)); }, [saved]);
  /** What a failed load has tried for this episode: one retry of the address, then SubCast's own route to it. */
  const tries = useRef({ id: "", retried: false, routed: false });
  // Subtitles made from another cut of the episode (other ads, a re-edit): line them up with what plays.
  const hasSubtitles = useJobs((s) => !!(audio && s.transcripts[audio.id]?.segments.length));
  useEffect(() => { if (audio && hasSubtitles) void alignCurrent(); }, [audio?.id, hasSubtitles]);
  // The map fits a downloaded copy: play that one now, from the same line.
  const alignPath = useStore((s) => (audio && s.align?.id === audio.id ? s.align.path : null));
  const resume = useRef(false);
  /** Subtitle time to carry over to the new file (its first time updates say 0 before it has loaded). */
  const carry = useRef<number | null>(null);
  useEffect(() => {
    if (!audio || !alignPath) return;
    const file = convertFileSrc(alignPath);
    if (file === src || brokenLocal.has(file)) return;
    if (!useOffline.getState().saved[audio.id]) noteCached(audio.id, alignPath, useStore.getState().align?.localBytes ?? 0);
    resume.current = !!ref.current && !ref.current.paused;
    carry.current = useStore.getState().time;
    setSrc(file);
  }, [alignPath]);

  useEffect(() => {
    const el = ref.current;
    if (!el || !audio) return;
    const handle = {
      seek: (t: number) => { el.currentTime = toPlay(mapOf(audio.id), t); el.play().catch(() => {}); },
      seekRaw: (t: number) => { el.currentTime = t; el.play().catch(() => {}); },
      // A load that failed leaves the element dead until it loads again.
      toggle: () => { if (!el.paused) { el.pause(); return; } if (el.error) el.load(); void el.play().catch(() => {}); },
      play: () => { if (el.error) el.load(); void el.play().catch(() => {}); },
      setRate: (r: number) => { el.playbackRate = r; },
      episodeId: audio.id,
    };
    offerPlayer(handle);
    const offVolume = followVolume(el);
    return () => { offVolume(); withdrawPlayer(handle); };
  }, [audio?.id]);

  useEffect(() => { if (ref.current) ref.current.playbackRate = rate; }, [rate, audio?.id]);

  if (!audio || !src) return null;
  return (
    <audio
      ref={ref}
      src={src}
      // It is the current episode: start buffering now, so pressing play doesn't start from nothing.
      preload="auto"
      onLoadedMetadata={(e) => {
        const el = e.currentTarget;
        el.playbackRate = rate;
        // It loaded: a later drop (the network mid-episode) gets its own quick retry.
        tries.current.retried = false;
        const s = useStore.getState();
        const start = toPlay(mapOf(audio.id), carry.current ?? (s.current?.id === audio.id ? s.time : audio.progress));
        carry.current = null;
        if (start > 5 && start < el.duration - 10) el.currentTime = start;
        track(audio, el.currentTime, el.duration);
        if (resume.current) { resume.current = false; el.play().catch(() => {}); }
      }}
      onTimeUpdate={(e) => {
        const el = e.currentTarget;
        // A new address has nothing loaded yet: its 0 is not where we are.
        if (el.readyState === 0 || carry.current != null) return;
        const gapEnd = track(audio, el.currentTime);
        if (gapEnd && useStore.getState().settings.skipAds && !el.paused) el.currentTime = gapEnd;
      }}
      onPlay={() => { useStore.getState().set({ playing: true }); if (ref.current && ref.current.readyState < 3) waiting(true); }}
      onWaiting={() => waiting(true)}
      onPlaying={() => waiting(false)}
      onPause={(e) => {
        waiting(false);
        useStore.getState().set({ playing: false });
        // Progress is kept in subtitle time, like everything outside the player.
        const at = toContent(mapOf(audio.id), e.currentTarget.currentTime).at;
        void api.setProgress(audio.id, at);
        notePlaybackProgress(audio, at, e.currentTarget.duration || audio.duration);
        if (saved && src !== playable(audio) && !tries.current.routed) setSrc(playable(audio));
      }}
      onEnded={(e) => { markEnded(audio, toContent(mapOf(audio.id), e.currentTarget.duration || e.currentTarget.currentTime).at); playNext(); }}
      onError={(e) => {
        const el = e.currentTarget;
        const remote = audio.audioUrl;
        if (tries.current.id !== audio.id) tries.current = { id: audio.id, retried: false, routed: false };
        const st = useStore.getState();
        /** Carries on from where it was, playing again if it was. */
        const again = (next?: string) => {
          carry.current = st.current?.id === audio.id ? st.time : audio.progress;
          resume.current = st.playing;
          if (next === src) el.load(); else setSrc(next);
        };
        console.warn("SubCast audio failed", el.error?.code, el.error?.message, src);
        // A local copy that won't open (trimmed from the cache, or not allowed): the original has the same bytes.
        if (src && remote && src !== remote && !src.startsWith("http://127.0.0.1:")) {
          brokenLocal.add(src);
          again(remote);
          return;
        }
        if (!remote) { waiting(false); st.notify(t("音频加载失败：{why}", { why: mediaProblem(el.error?.code) })); return; }
        // Hosts sometimes refuse one request (busy, rate-limited): once more after a moment.
        if (!tries.current.retried) {
          tries.current.retried = true;
          window.setTimeout(() => { if (useStore.getState().current?.id === audio.id) again(src); }, 1200);
          return;
        }
        // The web view can't load it (a proxy the host refuses, plain http, odd answers): SubCast fetches it.
        if (!tries.current.routed) {
          tries.current.routed = true;
          api.audioStream(remote).then(
            (via) => { if (useStore.getState().current?.id === audio.id) again(via); },
            () => { waiting(false); st.notify(t("音频加载失败：{why}", { why: mediaProblem(el.error?.code) })); },
          );
          return;
        }
        waiting(false);
        st.set({ playing: false });
        // Pressing play again starts over (the element reloads: see the handle).
        tries.current = { id: "", retried: false, routed: false };
        st.notify(t("音频加载失败：{why}。可以在 设置 › 通用 › 网络诊断 检查网络或代理", { why: mediaProblem(el.error?.code) }));
      }}
    />
  );
}

type StreamMode = "native-hls" | "proxy-hls" | "mp4";
const nativeHls = () => document.createElement("video").canPlayType("application/vnd.apple.mpegurl") !== "";
/** How long to wait for a video address (yt-dlp, its bot-check retries, the proxy) before trying something else.
 *  The backend has its own, longer limit; this keeps the player from spinning for minutes. */
const STREAM_WAIT = 40_000;
/** How long a direct stream may take to show its first frame's details (its length) before it counts as stuck. */
const STALL_WAIT = 20_000;
/** Play loading this long gets a "loading slowly" note with the ways out (audio, YouTube). */
const SLOW_AFTER = 8_000;
/** An embedded player asked to play that hasn't started after this long counts as not working here. */
const EMBED_START_WAIT = 20_000;
function withTimeout<T>(p: Promise<T>, ms: number): Promise<T> {
  return new Promise((resolve, reject) => {
    const timer = window.setTimeout(() => reject(new Error(t("获取视频地址超时，请检查网络或代理"))), ms);
    p.then((v) => { window.clearTimeout(timer); resolve(v); }, (e) => { window.clearTimeout(timer); reject(e); });
  });
}

/** Our subtitles replace YouTube's: turning the caption module off alone is not enough when the viewer has captions on. */
function hideYouTubeCaptions(player: any) {
  try { player.setOption?.("captions", "track", {}); } catch { /* module not loaded yet */ }
  try { player.unloadModule?.("captions"); player.unloadModule?.("cc"); } catch { /* not loaded */ }
}

export const RATES = [0.5, 0.75, 1, 1.25, 1.5, 1.75, 2, 2.5, 3];

/** YouTube without YouTube's chrome: the embed runs with its controls off, cropped so the title bar and logo fall
 *  outside the frame, under a shield that keeps hover UI from appearing. Our own controls and subtitles drive it.
 *  Videos whose owners forbid embedding fall back to the HLS stream yt-dlp finds, played in a plain <video>. */
export function YouTubePlayer({ ep, overlay, onUnplayable }: { ep: Episode; overlay?: React.ReactNode; onUnplayable?: () => void }) {
  const [native, setNativeState] = useState<{ url: string; kind: "hls" | "mp4" } | null>(null);
  /** The direct stream to play. Its length, when yt-dlp gave one, fills a bar that has none yet (feeds without
   *  a length showed "--:--" and couldn't seek until the video loaded). */
  const setNative = (stream: { url: string; kind: "hls" | "mp4"; duration?: number | null } | null) => {
    setNativeState(stream && { url: stream.url, kind: stream.kind });
    const s = useStore.getState();
    if (stream?.duration && s.current?.id === ep.id && !s.length) s.set({ length: stream.duration });
  };
  const [error, setError] = useState<string | null>(null);
  /** What the switch to the direct stream is waiting for; null when not switching. */
  const [fallingBack, setFallingBack] = useState<string | null>(null);
  const box = useRef<HTMLDivElement>(null);
  /** The WebView itself can't reach YouTube (the embed's script never loaded): the stream then has to come
   *  through the app's loopback proxy, which uses SubCast's own network settings. */
  const viaApp = useRef(false);
  /** Streams still to try when the one on screen fails: through the proxy, then the MP4. */
  const plan = useRef<StreamMode[]>([]);
  /** The stream on screen, and whether it ever loaded (then a failure is more likely an expired address). */
  const playingMode = useRef<StreamMode | null>(null);
  const loadedOnce = useRef(false);
  /** A fresh address was already fetched for an expired one: once is enough, or a flapping network loops forever. */
  const refreshed = useRef(false);
  /** Going straight to the direct stream: the owner forbids embedding, the original audio was asked for, or
   *  nothing could play and the poster waits for a retry. */
  const [direct, setDirect] = useState(() => wasBlocked(ep.videoId) || wantsOriginalAudio(ep.videoId));
  /** The fallback running now; a second request (the embed can report the same failure twice) waits for it. */
  const resolving = useRef(false);
  const mine = () => useStore.getState().current?.id === ep.id;
  /** Nothing plays the video: hand over to the audio when there is one, else say what to do. */
  const giveUp = (why: string, detail: string) => {
    // The audio takes over with the "play was pressed" state left as it is: it reads it, and starts by itself.
    if (onUnplayable) { onUnplayable(); return; }
    // No audio to fall back on: the poster comes back (its play button tries again) under a clear error.
    if (mine()) { waiting(false); useStore.getState().set({ playing: false }); }
    setNative(null); setDirect(true);
    setError(t("{reason}。{detail}", { reason: why, detail }));
  };
  const fallback = async (why: string, noRoute = false) => {
    if (resolving.current) return;
    resolving.current = true;
    viaApp.current ||= noRoute;
    refreshed.current = false;
    setError(null);
    setFallingBack(t("{reason}，正在改用直连播放…", { reason: why }));
    const off = listen<{ videoId: string; stage: string; progress: number }>("yt-stream", ({ payload: p }) => {
      if (p.videoId !== ep.videoId) return;
      setFallingBack(p.stage === "tool"
        ? t("首次使用，正在下载 YouTube 播放组件 {pct}%", { pct: Math.round(p.progress * 100) })
        : t("{reason}，正在获取视频地址…", { reason: why }));
    });
    // HLS where the WebView plays it (macOS); elsewhere, or when the WebView has no route to YouTube, HLS through
    // the app's loopback proxy (hls.js where needed), for HD.
    const mode: StreamMode = viaApp.current || !nativeHls() ? "proxy-hls" : "native-hls";
    // Play was pressed (or it was playing): the direct stream picks up where the embed left off.
    const wanted = () => { const s = useStore.getState(); return s.current?.id === ep.id && (!!s.loadingSince || s.playWhenReady === ep.id || s.playing); };
    try {
      const stream = await withTimeout(api.youtubeStream(ep.videoId!, mode), STREAM_WAIT);
      plan.current = mode === "native-hls" ? ["proxy-hls", "mp4"] : mode === "proxy-hls" ? ["mp4"] : [];
      playingMode.current = mode; loadedOnce.current = false;
      if (wanted()) useStore.getState().set({ playWhenReady: ep.id });
      setNative(stream); setError(null);
    }
    catch (e) {
      const msg = typeof e === "string" ? e : String((e as any)?.message ?? e);
      // YouTube wants a signed-in browser: ask once, here, instead of failing or silently falling back to audio.
      if (isBotCheck(msg)) { if (mine()) waiting(false); setBotCheck(why); setBotWhy(browserLocked(msg) ? "locked" : signInExpired(msg) ? "expired" : "ask"); return; }
      if (needsBrowserAccess(msg)) { if (mine()) waiting(false); setDenied(why); return; }
      giveUp(why, tr(msg));
    }
    finally { resolving.current = false; setFallingBack(null); void off.then((f) => f()); }
  };
  const retry = () => { setError(null); void fallback(t("重新获取视频")); };
  // The stream on screen failed or never started (expired address, no route from the WebView, HLS unsupported):
  // everything through the app's proxy, then the MP4; then the audio, or a clear error.
  const fetching = useRef(false);
  const failed = () => {
    // hls.js, the <video> element and the stall check can all report the same failure.
    if (fetching.current) return;
    // It played and then failed: the address has probably expired, so the same kind gets a fresh one first.
    if (loadedOnce.current && playingMode.current && !refreshed.current) { plan.current.unshift(playingMode.current); refreshed.current = true; }
    loadedOnce.current = false;
    const mode = plan.current.shift();
    if (!mode) return giveUp(t("视频无法播放"), t("可能是网络或代理不通，检查代理设置后重试，或在 YouTube 打开"));
    fetching.current = true;
    // It was playing or about to: the new address carries on by itself.
    const s = useStore.getState();
    const resume = mine() && (s.playing || !!s.loadingSince || s.playWhenReady === ep.id);
    if (mine()) waiting(true);
    setFallingBack(t("视频没能播放，正在换一种方式…"));
    void withTimeout(api.youtubeStream(ep.videoId!, mode, true), STREAM_WAIT).then(
      (stream) => { fetching.current = false; setFallingBack(null); playingMode.current = mode; if (resume) useStore.getState().set({ playWhenReady: ep.id }); setNative(stream); },
      // This kind can't be had either: on to the next one (or the audio, or the error).
      () => { fetching.current = false; setFallingBack(null); failed(); });
  };
  const [botCheck, setBotCheck] = useState<string | null>(null);
  /** Why the check is being shown: never signed in, the browser's sign-in is unreadable, or SubCast's expired. */
  const [botWhy, setBotWhy] = useState<"ask" | "locked" | "expired">("ask");
  const signIn = useYouTubeSignIn(() => { const why = botCheck ?? ""; setBotCheck(null); void fallback(why); });
  /** macOS refused access to the browser's data; holds the reason to retry with. */
  const [denied, setDenied] = useState<string | null>(null);
  const [browser, setBrowser] = useState(useStore.getState().settings.ytCookiesBrowser || "chrome");
  const retryWithBrowser = async () => {
    const why = botCheck ?? "";
    setBotCheck(null);
    await useStore.getState().saveSettings({ ytCookiesBrowser: browser });
    void fallback(why);
  };
  useEffect(() => { setNative(null); setError(null); setDenied(null); plan.current = []; }, [ep.id]);
  // Known to refuse embedding: skip the embedded player and fetch the direct stream right away.
  // A saved video plays from disk: no YouTube player to load, and it works offline.
  const [localBroken, setLocalBroken] = useState(false);
  useEffect(() => setLocalBroken(false), [ep.id]);
  const saved = useOffline((s) => s.saved[ep.id]?.path);
  const localVideo = !localBroken && saved && /\.(mp4|m4v|mov|webm)$/i.test(saved) ? saved : undefined;
  useEffect(() => { if (direct && !localVideo) void fallback(wasBlocked(ep.videoId) ? t("作者限制了嵌入播放") : t("使用原声")); }, []);
  useEffect(() => {
    const on = (e: Event) => {
      if ((e as CustomEvent<string>).detail !== ep.videoId || native || localVideo) return;
      setDirect(true);
      void fallback(t("切换到原声"));
    };
    window.addEventListener("subcast-original-audio", on);
    return () => window.removeEventListener("subcast-original-audio", on);
  }, [ep.videoId, native, localVideo]);
  // Have yt-dlp ready before it is needed: a first fallback would otherwise start with a 35 MB download.
  useEffect(() => { void api.prepareYoutube().catch(() => {}); }, []);
  const watchOnYouTube = () => {
    const s = Math.floor(useStore.getState().current?.id === ep.id ? useStore.getState().time : ep.progress ?? 0);
    void openUrl(`https://www.youtube.com/watch?v=${ep.videoId}${s > 5 ? `&t=${s}s` : ""}`);
  };
  const openBtn = <button className="link-btn video-open" onClick={watchOnYouTube}><ExternalLink size={13} /> {t("在 YouTube 打开")}</button>;
  // Play has been loading a while (and nothing else explains it yet): say so, with the ways out.
  const slow = useLoadingFor(ep.id, SLOW_AFTER);
  // Waiting on the direct stream need not be the only way out when the episode has its audio.
  const audioBtn = onUnplayable ? <button className="link-btn video-open" onClick={onUnplayable}>{t("改听音频")}</button> : null;
  const fullscreen = () => toggleTheater();
  const theater = useStore((s) => s.theater);
  // Leaving the episode leaves full screen too.
  useEffect(() => () => { if (useStore.getState().theater) toggleTheater(false); }, []);
  // In full screen the controls show while the mouse moves or keys are pressed and fade after 2.5 s of stillness.
  const [idle, setIdle] = useState(false);
  const titleZh = useZh(ep.title);
  useEffect(() => {
    if (!theater) return;
    let timer = window.setTimeout(() => setIdle(true), 2500);
    const wake = () => { setIdle(false); window.clearTimeout(timer); timer = window.setTimeout(() => setIdle(true), 2500); };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") { e.preventDefault(); toggleTheater(false); return; }
      wake();
    };
    window.addEventListener("mousemove", wake);
    window.addEventListener("keydown", onKey);
    return () => { window.clearTimeout(timer); window.removeEventListener("mousemove", wake); window.removeEventListener("keydown", onKey); };
  }, [theater]);
  return (
    <div className={`video ${theater ? "theater" : ""} ${theater && idle ? "idle" : ""}`} ref={box}>
      {localVideo ? <NativeVideo ep={ep} src={convertFileSrc(localVideo)} kind="mp4" onFullscreen={fullscreen} onFail={() => setLocalBroken(true)} />
        : native ? <NativeVideo ep={ep} src={native.url} kind={native.kind} onFullscreen={fullscreen} onFail={failed} onLoaded={() => { loadedOnce.current = true; }} />
        : direct ? <PlayHint ep={ep} show onClick={() => { useStore.getState().load(ep); playSoon(ep.id); if (error) retry(); }} />
        : <EmbedVideo ep={ep} onFullscreen={fullscreen} onBlocked={(why, noRoute) => void fallback(why, noRoute)} onError={setError} />}
      {fallingBack ? <div className="video-note"><span className="spin-dot" /> {fallingBack} {audioBtn} {openBtn}</div>
        : slow && !error && !botCheck && denied == null && <div className="video-note"><span className="spin-dot" /> {t("视频加载较慢…")} {audioBtn} {openBtn}</div>}
      {botCheck && (
        <div className="video-error bot-check">
          <p>{t("YouTube 要求验证「不是机器人」才能播放这个视频。")}</p>
          <p className="small">{botWhy === "locked" ? t("{name} 把登录信息加密了，SubCast 读不到。在 SubCast 里登录一次 YouTube 就行，以后不用再借用浏览器。", { name: browser[0].toUpperCase() + browser.slice(1) })
            : botWhy === "expired" ? t("之前在 SubCast 里登录的 YouTube 已失效，重新登录一次再试。")
            : t("在 SubCast 里登录一次 YouTube，以后遇到验证会自动用这个登录。")}</p>
          <div className="row-actions">
            <button className="btn primary sm" disabled={signIn.waiting} onClick={() => void signIn.start()}>{signIn.waiting ? t("在弹出的窗口里登录…") : t("登录 YouTube")}</button>
            {openBtn}
          </div>
          {botWhy !== "locked" && (
            <div className="row-actions small">
              <span>{t("或者借用浏览器的登录：")}</span>
              <select value={browser} onChange={(e) => setBrowser(e.target.value)} aria-label={t("浏览器")}>
                {(IS_MAC ? ["chrome", "safari", "edge", "firefox", "brave"] : ["chrome", "edge", "firefox", "brave"]).map((b) => <option key={b} value={b}>{b[0].toUpperCase() + b.slice(1)}</option>)}
              </select>
              <button className="btn ghost sm" onClick={() => void retryWithBrowser()}>{t("借用登录再试")}</button>
            </div>
          )}
        </div>
      )}
      {denied != null && (
        <div className="video-error bot-check">
          <BrowserAccess browser={browser} onRetry={() => { const why = denied; setDenied(null); void fallback(why); }} />
        </div>
      )}
      {error && (
        <div className="video-error">
          <p>{error}</p>
          <div className="row-actions">
            <button className="btn primary sm" onClick={retry}>{t("重试")}</button>
            <button className="link-btn video-open" onClick={() => useStore.getState().openSettings("network")}>{t("网络诊断")}</button>
            {openBtn}
          </div>
        </div>
      )}
      {overlay}
      {!theater && <button className="video-fs" title={t("全屏（F）")} aria-label={t("全屏")} onClick={(e) => { e.stopPropagation(); fullscreen(); }}><Maximize2 size={16} /></button>}
      {theater && (
        <div className="theater-bar" onClick={(e) => e.stopPropagation()}>
          <p className="theater-title clamp1">{titleZh ?? ep.title}</p>
          <Controls compact />
          <button className="icon-btn theater-exit" title={t("退出全屏（Esc）")} aria-label={t("退出全屏")} onClick={() => toggleTheater(false)}><Minimize2 size={16} /></button>
        </div>
      )}
    </div>
  );
}

/** Full screen for the video: the window goes full screen and the video fills it, with its subtitles. A layout
 *  mode rather than the element Fullscreen API, which the macOS WebView doesn't enable. */
export function toggleTheater(on?: boolean) {
  const s = useStore.getState();
  const next = on ?? !s.theater;
  if (next && !document.querySelector(".video")) return false;
  s.set({ theater: next });
  void getCurrentWindow().setFullscreen(next).catch(() => {});
}

/** The poster over a video that hasn't started. While play is loading its button spins (a pause on hover: a click
 *  cancels), so it doesn't invite pressing play again. */
function PlayHint({ ep, show, onClick }: { ep: Episode; show: boolean; onClick: () => void }) {
  const loading = useLoadingFor(ep.id);
  return (
    <div className={`video-poster ${show ? "on" : ""}`} onClick={loading ? pressPlay : onClick} title={loading ? t("正在加载…（点击取消）") : undefined}>
      <img src={`https://i.ytimg.com/vi/${ep.videoId}/maxresdefault.jpg`} onError={(e) => { const i = e.currentTarget; if (!i.dataset.f) { i.dataset.f = "1"; i.src = `https://i.ytimg.com/vi/${ep.videoId}/hqdefault.jpg`; } }} alt="" />
      <span className={`big-play ${loading ? "loading" : ""}`}>{loading ? <LoadingIcon size={28} /> : <Play size={28} fill="currentColor" />}</span>
    </div>
  );
}

function EmbedVideo({ ep, onFullscreen, onBlocked, onError }: { ep: Episode; onFullscreen: () => void; onBlocked: (why: string, noRoute?: boolean) => void; onError: (m: string) => void }) {
  const host = useRef<HTMLDivElement>(null);
  const rate = useStore((s) => s.settings.playbackRate);
  const playerRef = useRef<any>(null);
  const [state, setState] = useState(-1);

  useEffect(() => {
    let player: any, timer: number | undefined, disposed = false, handle: any, offVolume: (() => void) | undefined;
    /** Set while a play request waits for the player to start; it gives up and falls back if it never does. */
    let startCheck: number | undefined;
    /** The video has started at least once: before that its clock says 0, which is not where the listener is. */
    let started = false;
    const mine = () => useStore.getState().current?.id === ep.id;
    const requestPlay = () => {
      waiting(true);
      player.playVideo();
      window.clearTimeout(startCheck);
      // Some setups load the embed but never play it (no route to YouTube's video servers, a blocked iframe):
      // a play button that only spins is worse than the direct stream.
      startCheck = window.setTimeout(() => {
        const st = player.getPlayerState?.();
        if (disposed || st === 1 || st === 2 || !mine() || !useStore.getState().loadingSince) return;
        // It loaded but can't play: YouTube's video servers are out of the WebView's reach, so the direct stream
        // should come through the app too.
        onBlocked(t("YouTube 播放器没有响应"), true);
      }, EMBED_START_WAIT);
    };
    loadYouTube().then((YT) => {
      if (disposed || !host.current) return;
      const mount = document.createElement("div");
      host.current.appendChild(mount);
      player = new YT.Player(mount, {
        videoId: ep.videoId,
        width: "100%",
        height: "100%",
        host: "https://www.youtube.com",
        playerVars: { controls: 0, disablekb: 1, fs: 0, playsinline: 1, rel: 0, modestbranding: 1, cc_load_policy: 0, iv_load_policy: 3, start: Math.floor(ep.progress > 5 ? ep.progress : 0), origin: window.location.origin },
        events: {
          onReady: () => {
            playerRef.current = player;
            hideYouTubeCaptions(player);
            player.setPlaybackRate?.(useStore.getState().settings.playbackRate);
            handle = {
              episodeId: ep.id,
              seek: (t: number) => { player.seekTo(t, true); if (player.getPlayerState?.() !== 1) requestPlay(); },
              // Buffering or still starting counts as playing: pressing the button then must pause, not ask
              // to play again (which made pause look dead while the video loaded or an ad ran).
              toggle: () => {
                const st = player.getPlayerState?.();
                if (st === 1 || st === 3 || useStore.getState().loadingSince) {
                  window.clearTimeout(startCheck);
                  player.pauseVideo();
                  waiting(false);
                  useStore.getState().set({ playing: false });
                } else requestPlay();
              },
              play: () => requestPlay(),
              setRate: (r: number) => player.setPlaybackRate(r),
              // YouTube's embedded player goes no faster than 2×.
              maxRate: 2,
            };
            offerPlayer(handle, { length: player.getDuration?.() || ep.duration || 0 });
            const applyVolume = () => { const v = useVolume.getState(); player.setVolume?.(Math.round(gainOf(v) * 100)); if (v.muted) player.mute?.(); else player.unMute?.(); };
            applyVolume();
            offVolume = useVolume.subscribe(applyVolume);
            timer = window.setInterval(() => {
              const t = player.getCurrentTime?.();
              if (typeof t === "number" && started) track(ep, t, player.getDuration?.());
            }, 250);
          },
          onStateChange: (e: any) => {
            setState(e.data);
            if (e.data === 1 || e.data === 2 || e.data === 3) started = true;
            if (e.data === 1 || e.data === 2) window.clearTimeout(startCheck);
            // Another episode is current (this page was opened while it plays): leave its state alone.
            if (!mine()) return;
            // 3 = buffering; -1/5 after a play request = still starting. 1 = playing.
            if (e.data === 3) waiting(true); else if (e.data === 1 || e.data === 2 || e.data === 0) waiting(false);
            useStore.getState().set({ playing: e.data === 1 });
            if (e.data === 1) hideYouTubeCaptions(player);
            if (e.data === 2) { const t = player.getCurrentTime(); void api.setProgress(ep.id, t); notePlaybackProgress(ep, t, player.getDuration?.() ?? ep.duration); }
            if (e.data === 0) { markEnded(ep, player.getDuration?.() ?? player.getCurrentTime()); playNext(); }
          },
          onError: (e: any) => {
            // 101/150: the owner disallows embedding. 152/153 are player/referrer problems on this side, not the owner's choice.
            console.warn("YouTube player error", e.data, ep.videoId);
            if (e.data === 101 || e.data === 150) markBlocked(ep.videoId);
            if (![100, 101, 150, 152, 153].includes(e.data) && mine()) waiting(false);
            if ([100, 101, 150, 152, 153].includes(e.data)) onBlocked(e.data === 100 ? t("视频不存在或已设为私享")
              : e.data === 101 || e.data === 150 ? t("作者限制了嵌入播放")
              : t("YouTube 播放器加载失败（错误 {code}）", { code: e.data }));
            else onError(t("视频无法播放（错误 {code}）", { code: e.data }));
          },
        },
      });
    // The embedded player can't load (often no route to youtube.com): the direct stream may still work.
    }).catch((e) => { if (!disposed) onBlocked(tr(String(e.message ?? e)), true); });
    return () => {
      disposed = true;
      window.clearTimeout(startCheck);
      offVolume?.();
      window.clearInterval(timer);
      try { if (started) void api.setProgress(ep.id, player?.getCurrentTime?.() ?? 0); player?.destroy?.(); } catch { /* already gone */ }
      if (handle) withdrawPlayer(handle);
    };
  }, [ep.id]);

  useEffect(() => { playerRef.current?.setPlaybackRate?.(rate); }, [rate]);
  const toggle = () => toggleVideo(ep);
  return (
    <>
      <div className="video-host crop" ref={host} />
      <div className="video-shield" onClick={toggle} onDoubleClick={onFullscreen} />
      <PlayHint ep={ep} show={state === -1 || state === 5 || state === 0} onClick={toggle} />
      {state === 2 && <span className="paused-mark" onClick={toggle}><Play size={22} fill="currentColor" /></span>}
    </>
  );
}

/** `onFail`: the stream can't be played (stale address, or HLS unsupported here); the caller gets another. */
function NativeVideo({ ep, src, kind, onFullscreen, onFail, onLoaded }: { ep: Episode; src: string; kind: "hls" | "mp4"; onFullscreen: () => void; onFail?: () => void; onLoaded?: () => void }) {
  const ref = useRef<HTMLVideoElement>(null);
  const rate = useStore((s) => s.settings.playbackRate);
  const [started, setStarted] = useState(false);
  const [paused, setPaused] = useState(false);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    let hls: any, dead = false;
    setStarted(false);
    if (kind === "mp4" || el.canPlayType("application/vnd.apple.mpegurl")) el.src = src;
    else void import("hls.js").then(({ default: Hls }) => {
      if (dead) return;
      if (!Hls.isSupported()) { onFail?.(); return; }
      hls = new Hls();
      hls.on(Hls.Events.ERROR, (_: unknown, d: { fatal?: boolean }) => { if (d.fatal) { hls.destroy(); hls = null; onFail?.(); } });
      hls.loadSource(src);
      hls.attachMedia(el);
    });
    const handle = {
      seek: (t: number) => { el.currentTime = t; void el.play().catch(() => {}); },
      toggle: () => (el.paused ? void el.play().catch(() => {}) : el.pause()),
      play: () => void el.play().catch(() => {}),
      setRate: (r: number) => { el.playbackRate = r; },
      episodeId: ep.id,
    };
    offerPlayer(handle);
    const offVolume = followVolume(el);
    // A stream the WebView can't reach doesn't always fail: it can just never load (length stays "--:--").
    const stall = window.setTimeout(() => { if (!dead && el.readyState === 0) onFail?.(); }, STALL_WAIT);
    return () => {
      dead = true;
      window.clearTimeout(stall);
      offVolume();
      hls?.destroy?.();
      if (el.currentTime > 0) void api.setProgress(ep.id, el.currentTime);
      withdrawPlayer(handle);
    };
  }, [src, ep.id]);
  useEffect(() => { if (ref.current) ref.current.playbackRate = rate; }, [rate]);
  return (
    <>
      <video ref={ref} className="video-native" playsInline preload="auto"
        // A stale or blocked address, before or during playback: the caller fetches a fresh one (once).
        onError={() => onFail?.()}
        onLoadedMetadata={(e) => {
          const el = e.currentTarget, s = useStore.getState();
          el.playbackRate = rate;
          // Where it actually is now (after the embed played a while, or mid-play on a fresh address), not where the page opened.
          const at = s.current?.id === ep.id ? s.time : ep.progress;
          if (at > 5 && at < el.duration - 10) el.currentTime = at;
          track(ep, el.currentTime, el.duration);
          onLoaded?.();
        }}
        onTimeUpdate={(e) => track(ep, e.currentTarget.currentTime)}
        onPlay={(e) => { setStarted(true); setPaused(false); useStore.getState().set({ playing: true }); if (e.currentTarget.readyState < 3) waiting(true); }}
        onWaiting={() => waiting(true)}
        onPlaying={() => { setPaused(false); waiting(false); }}
        onPause={(e) => { setPaused(true); waiting(false); useStore.getState().set({ playing: false }); void api.setProgress(ep.id, e.currentTarget.currentTime); notePlaybackProgress(ep, e.currentTarget.currentTime, e.currentTarget.duration || ep.duration); }}
        onEnded={(e) => { setPaused(false); markEnded(ep, e.currentTarget.duration); playNext(); }} />
      <div className="video-shield" onClick={() => toggleVideo(ep)} onDoubleClick={onFullscreen} />
      <PlayHint ep={ep} show={!started} onClick={() => toggleVideo(ep)} />
      {started && paused && <span className="paused-mark" onClick={() => toggleVideo(ep)}><Play size={22} fill="currentColor" /></span>}
    </>
  );
}

export function Controls({ compact = false, video = false }: { compact?: boolean; video?: boolean }) {
  const clipTools = useClipTools();
  // Whole seconds: the clock and bar move once a second instead of re-rendering on every time update.
  // The bar and clock follow the player's own clock (`rawTime`), which can differ from subtitle time.
  const { currentId, time, length, playing, player, playbackRate, skipBack, skipForward, loadingSince } = useStore(useShallow((s) => ({
    currentId: s.current?.id,
    time: Math.floor(s.rawTime ?? s.time),
    length: s.length,
    playing: s.playing,
    player: s.player,
    playbackRate: s.settings.playbackRate,
    skipBack: s.settings.skipBack ?? 15,
    skipForward: s.settings.skipForward ?? 30,
    loadingSince: s.loadingSince,
  })));
  const sub = () => useStore.getState().time;
  const seekBar = (at: number) => (player?.seekRaw ? player.seekRaw(at) : player?.seek(at));
  const [drag, setDrag] = useState<number | null>(null);
  const [hover, setHover] = useState<{ time: number; pct: number } | null>(null);
  const seekWrap = useRef<HTMLDivElement>(null);

  const shown = drag ?? time;
  const tip = drag != null ? { time: drag, pct: length ? (drag / length) * 100 : 0 } : hover;
  const updateHover = (clientX: number) => {
    const el = seekWrap.current;
    if (!el || !length) return;
    const rect = el.getBoundingClientRect();
    const ratio = Math.min(1, Math.max(0, (clientX - rect.left) / Math.max(1, rect.width)));
    setHover({ time: ratio * length, pct: ratio * 100 });
  };
  return (
    <div className={`controls ${compact ? "compact" : ""}`}>
      <button className="icon-btn skip-btn" title={t("后退 {n} 秒（←）", { n: skipBack })} aria-label={t("后退 {n} 秒", { n: skipBack })} onClick={() => player?.seek(Math.max(0, sub() - skipBack))} disabled={!player}>
        <RotateCcw size={18} />
        <span className="skip-num">{skipBack}</span>
      </button>
      <button className={`play-btn ${loadingSince ? "loading" : ""}`} title={loadingSince ? t("正在加载…（点击取消）") : playing ? t("暂停（空格）") : t("播放（空格）")} onClick={pressPlay} disabled={!player && !loadingSince} aria-busy={!!loadingSince}>
        {loadingSince ? <LoadingIcon size={20} /> : playing ? <Pause size={20} fill="currentColor" /> : <Play size={20} fill="currentColor" />}
      </button>
      <button className="icon-btn skip-btn" title={t("前进 {n} 秒（→）", { n: skipForward })} aria-label={t("前进 {n} 秒", { n: skipForward })} onClick={() => player?.seek(sub() + skipForward)} disabled={!player}>
        <RotateCw size={18} />
        <span className="skip-num">{skipForward}</span>
      </button>
      <span className="time">{clock(shown)}</span>
      <div
        className="seek-wrap"
        ref={seekWrap}
        onMouseMove={(e) => updateHover(e.clientX)}
        onMouseLeave={() => setHover(null)}
      >
        {!compact && clipTools && <SeekHighlights episodeId={currentId} length={length} onSeek={(sec) => player?.seek(sec)} />}
        {tip && length > 0 && (
          <span className="seek-tip" style={{ left: `clamp(24px, ${tip.pct}%, calc(100% - 24px))` }} aria-hidden>
            {clock(tip.time)}
          </span>
        )}
        <input
          className="seek"
          type="range"
          min={0}
          max={Math.max(length, 1)}
          step={1}
          value={Math.min(shown, length || shown)}
          onChange={(e) => setDrag(Number(e.target.value))}
          onPointerUp={() => { if (drag != null) { seekBar(drag); setDrag(null); } }}
          onPointerCancel={() => setDrag(null)}
          onBlur={() => setDrag(null)}
          onKeyUp={() => { if (drag != null) { seekBar(drag); setDrag(null); } }}
          style={{ "--fill": `${length ? (shown / length) * 100 : 0}%` } as React.CSSProperties}
          aria-label={t("播放进度")}
        />
      </div>
      <span className="time">{length ? clock(length) : "--:--"}</span>
      <VolumeControl />
      <RateMenu rate={playbackRate} max={player?.maxRate} />
      {!compact && video && <button className="icon-btn fs-btn" title={t("全屏（F）")} aria-label={t("全屏")} onClick={() => toggleTheater(true)}><Maximize2 size={17} /></button>}
    </div>
  );
}

/** The speed, picked from a list that opens upwards (the controls sit at the bottom). */
function RateMenu({ rate, max }: { rate: number; max?: number }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); setOpen(false); } };
    document.addEventListener("pointerdown", away);
    window.addEventListener("keydown", esc, true);
    return () => { document.removeEventListener("pointerdown", away); window.removeEventListener("keydown", esc, true); };
  }, [open]);
  const shown = max && rate > max ? max : rate;
  return (
    <div className="rate-menu" ref={box}>
      <button className={`rate-btn${shown >= 2 ? " fast" : ""}`} onClick={() => setOpen(!open)} title={t("播放速度")} aria-haspopup="menu" aria-expanded={open}>{shown >= 2 && <span className="rate-bars" aria-hidden><i /><i /><i /></span>}{shown}×</button>
      {open && (
        <div className="rate-pop" role="menu" aria-label={t("播放速度")}>
          {[...RATES].reverse().map((r) => {
            const off = !!max && r > max;
            return (
              <button key={r} role="menuitemradio" aria-checked={r === shown} disabled={off} title={off ? t("YouTube 视频最快 2 倍速，切到「仅音频」可以更快") : undefined}
                className={r === shown ? "on" : ""} onClick={() => { setRate(r); setOpen(false); }}>
                <Check size={13} className="rate-check" />{r === 1 ? t("正常") : `${r}×`}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}

function setRate(r: number) {
  const { saveSettings, player } = useStore.getState();
  player?.setRate(r);
  void saveSettings({ playbackRate: r });
}

/** Speaker button (click mutes) with a floating slider above it on hover or keyboard focus so the seek bar never shifts. */
function VolumeControl() {
  const { level, muted } = useVolume(useShallow((s) => ({ level: s.level, muted: s.muted })));
  const shown = muted ? 0 : level;
  const Icon = shown === 0 ? VolumeX : shown < 0.5 ? Volume1 : Volume2;
  const pct = Math.round(shown * 100);
  return (
    <div className="volume" onWheel={(e) => { if (e.deltaY) setLevel(shown - Math.sign(e.deltaY) * 0.05); }}>
      <button className="icon-btn" onClick={() => toggleMute()} title={muted ? t("取消静音（M）") : t("静音（M）")} aria-label={muted ? t("取消静音") : t("静音")}><Icon size={17} /></button>
      <div className="volume-pop">
        <input className="seek volume-slider" type="range" min={0} max={1} step={0.01} value={shown}
          onChange={(e) => setLevel(Number(e.target.value))}
          style={{ "--fill": `${pct}%` } as React.CSSProperties}
          aria-label={t("音量")} aria-valuetext={`${pct}%`} title={`${t("音量")} ${pct}%`} />
        <span className="num small muted volume-pct">{pct}%</span>
      </div>
    </div>
  );
}

export function cycleMode() {
  const { settings, saveSettings, current, notify } = useStore.getState();
  const order = ["bilingual", "translation", "original", "off"] as const;
  const cur = subtitleModeFor(settings, current);
  const next = order[(order.indexOf(cur) + 1) % order.length];
  void saveSettings(subtitleModePatch(settings, current, next));
  const label = next === "bilingual" ? t("双语") : next === "translation" ? t("中文") : next === "original" ? t("原文") : t("已关闭");
  notify(t("字幕：{mode}", { mode: label }));
}

/** Steps the speed up or down one notch (for shortcuts). */
export function stepRate(dir: 1 | -1) {
  const { settings, saveSettings, player, notify } = useStore.getState();
  const cur = settings.playbackRate;
  const max = player?.maxRate ?? RATES[RATES.length - 1];
  const r = Math.min(max, dir > 0 ? (RATES.find((x) => x > cur + 0.001) ?? cur) : ([...RATES].reverse().find((x) => x < cur - 0.001) ?? cur));
  player?.setRate(r);
  void saveSettings({ playbackRate: r });
  notify(`${r}×`);
}


