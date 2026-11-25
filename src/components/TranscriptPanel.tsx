import { useShallow } from "zustand/react/shallow";
import { SkeletonLines } from "./Skeleton";
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { AudioLines, BookA, Check, ChevronDown, ChevronUp, Crosshair, Play, Scissors, Sparkles, Square, Volume2, Download, Ellipsis, Languages, ListChecks, Loader2, Mic, RefreshCw, Search, X } from "lucide-react";
import { AlignBar, anchorHere } from "./AlignBar";
import { Modal } from "./Chrome";
import { ReadAloudSettings } from "./ReadAloud";
import { RatePicker } from "./RatePicker";
import { isKeyError } from "../lib/tts";
import { startZhRead, stopZhRead, useZhRead } from "../lib/zhRead";
import { SubtitleExport } from "./SubtitleExport";
import type { EpisodeContext } from "../lib/ai";
import { CassetteArt, FlatLineArt, NightNote, TrayArt, WaveToTextArt } from "./Illustrations";
import { bytes } from "../lib/offline";
import { api, errorText, rawErrorText, type Episode, type ModelInfo, type Segment } from "../lib/api";
import { clock, sizeMb } from "../lib/format";
import { generateTranscript, llmReady, loadTranslation, needsTranslation, stopTranslation, switchYoutubeTrack, translateEpisode, useJobs } from "../lib/jobs";
import { providerFor, useStore } from "../store";
import { useClipTools } from "../lib/clipTools";
import { ASR_SERVICES, CLOUD_ASR_MAIN, subtitleModeFor, subtitleModePatch, type Settings } from "../lib/settings";
import { lang, t, tk, tr, useLang } from "../lib/i18n";
import { looksEnglish } from "../lib/dict";
import { EpisodeWords, LearnText, LearnTip, WordCard, useLearnMarks, type CardTarget, type LearnMarks } from "./Learn";
import { ShadowBar, useShadowing } from "./Shadowing";
import { loadHighlights, previewEpisodeRange, useHighlights, type HighlightClip } from "../lib/highlights";
import { HighlightsPanel, useClipWarm } from "./HighlightViews";
import { openVideoClipDialog } from "./VideoClipDialog";

const SOURCE_LABEL: Record<string, string> = { youtube: tk("YouTube 字幕"), "youtube-auto": tk("YouTube 自动字幕"), rss: tk("节目方文字稿"), whisper: tk("本机识别"), cloud: tk("云端识别"), qiaomu: tk("精读站原文 · 时间为估算"), podscribe: tk("Podscribe 文字稿") };

export function activeIndex(segments: Segment[], time: number): number {
  let lo = 0, hi = segments.length - 1, found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (segments[mid].start <= time + 0.15) { found = mid; lo = mid + 1; } else hi = mid - 1;
  }
  return found;
}

function highlight(text: string, query: string) {
  if (!query) return text;
  const i = text.toLowerCase().indexOf(query.toLowerCase());
  return i < 0 ? text : <>{text.slice(0, i)}<mark>{text.slice(i, i + query.length)}</mark>{text.slice(i + query.length)}</>;
}

const NO_SEGMENTS: Segment[] = [];
const VIRTUAL_THRESHOLD = 250;
const OVERSCAN = 24;

function computeWindow(heights: Float32Array, scrollTop: number, viewH: number) {
  const n = heights.length;
  if (!n) return { start: 0, end: 0, topPad: 0, bottomPad: 0 };
  let y = 0;
  let i = 0;
  while (i < n && y + heights[i] < scrollTop) {
    y += heights[i];
    i++;
  }
  const start = Math.max(0, i - OVERSCAN);
  let topPad = 0;
  for (let p = 0; p < start; p++) topPad += heights[p];

  let j = i;
  let visibleBottom = y;
  while (j < n && visibleBottom < scrollTop + viewH) {
    visibleBottom += heights[j];
    j++;
  }
  const end = Math.min(n, j + OVERSCAN);
  let bottomPad = 0;
  for (let p = end; p < n; p++) bottomPad += heights[p];
  return { start, end, topPad, bottomPad };
}

function rowBounds(heights: Float32Array, pos: number) {
  let top = 0;
  for (let i = 0; i < pos && i < heights.length; i++) top += heights[i];
  const h = heights[pos] ?? 60;
  return { top, bottom: top + h, height: h };
}

// A click on a line plays from it, but selecting text never touches playback: the jump waits out a possible
// double-click (which selects a word) and is dropped when the click ended a drag-selection.
let lineClick: number | undefined;
let downAt: { x: number; y: number } | null = null;
function onLinePointerDown(e: React.PointerEvent) { downAt = { x: e.clientX, y: e.clientY }; }
function onLineClick(e: React.MouseEvent, seek: () => void) {
  window.clearTimeout(lineClick);
  const dragged = downAt && Math.hypot(e.clientX - downAt.x, e.clientY - downAt.y) > 4;
  if (e.detail > 1 || dragged || window.getSelection()?.toString().trim()) return;
  lineClick = window.setTimeout(() => { if (!window.getSelection()?.toString().trim()) seek(); }, 260);
}

const Row = memo(function Row({ seg, i, k, zh, active, playingActive, mode, pending, onSeek, query, current, marks, reading, hlClass, hl, isVideo, onHlPreview, onHlClip, registerRow }: {
  seg: Segment; i: number; k: number; zh?: string | null; active: boolean; playingActive: boolean; mode: string; pending: boolean;
  onSeek: (t: number, i: number) => void; query: string; current: boolean; marks: LearnMarks | null; reading?: boolean;
  /** Highlight span styling, and the clip that starts at this line (its heading is drawn above it). */
  hlClass?: string; hl?: HighlightClip; isVideo?: boolean; onHlPreview?: (c: HighlightClip) => void; onHlClip?: (c: HighlightClip) => void;
  registerRow?: (k: number, el: HTMLDivElement | null) => void;
}) {
  const showZh = mode !== "original";
  // "中文" means Chinese only, learning mode or not; a line not translated yet shows its original meanwhile.
  const showOrig = mode !== "translation" || (!zh && !pending);
  const setRef = useCallback((el: HTMLDivElement | null) => { registerRow?.(k, el); }, [registerRow, k]);
  const line = (
    <div
      ref={registerRow && !hl ? setRef : undefined}
      className={`line ${active ? "active" : ""} ${current ? "hit" : ""} ${reading ? "zh-reading" : ""} ${hlClass ?? ""}`}
      data-i={i}
      data-active={active || undefined}
      data-reading={reading || undefined}
      data-hit={current || undefined}
      role="button"
      tabIndex={0}
      onPointerDown={onLinePointerDown}
      onClick={(e) => onLineClick(e, () => onSeek(seg.start, i))}
      onKeyDown={(e) => e.key === "Enter" && onSeek(seg.start, i)}
    >
      <span
        className="line-time"
        title={t("跳到这里播放")}
        onPointerDown={(e) => e.stopPropagation()}
        onClick={(e) => { e.stopPropagation(); onSeek(seg.start, i); }}
      >
        {playingActive && <span className="live-dot" aria-hidden />}
        {clock(seg.start)}
      </span>
      <div className="line-text">
        {showZh && (zh ? <p className="zh">{highlight(zh, query)}</p> : pending ? <span className="skeleton" /> : null)}
        {showOrig && <p className="orig">{marks && !query ? <LearnText text={seg.text} marks={marks} /> : highlight(seg.text, query)}</p>}
      </div>
    </div>
  );
  if (!hl) return line;
  // The heading lives inside the measured element, so the virtual list counts its height.
  return (
    <div ref={registerRow ? setRef : undefined} className="hl-group">
      <div className="hl-head">
        <span className="hl-score">✦ {hl.score}</span>
        <span className="hl-head-title clamp1">{hl.title}</span>
        <span className="num small muted">{Math.round(hl.end - hl.start)}s</span>
        <span className="spacer" />
        <button type="button" className="link-btn" onClick={() => onHlPreview?.(hl)}>
          <Play size={11} fill="currentColor" /> {isVideo ? t("试看") : t("试听")}
        </button>
        {onHlClip && (
          <button type="button" className="link-btn hl-clip-btn" onClick={() => onHlClip(hl)}>
            <Scissors size={11} /> {t("剪视频")}
          </button>
        )}
      </div>
      {line}
    </div>
  );
});

function useModels() {
  const [models, setModels] = useState<ModelInfo[]>([]);
  const refresh = () => api.asrModels().then(setModels);
  useEffect(() => { void refresh(); }, []);
  return { models, refresh };
}

export function useModelDownload(onDone: () => void) {
  const [downloading, setDownloading] = useState<{ id: string; received: number; total?: number } | null>(null);
  useEffect(() => {
    const un = listen<{ id: string; received: number; total?: number }>("model-download", (e) => setDownloading(e.payload));
    return () => { void un.then((f) => f()); };
  }, []);
  /** Tries the China mirror first, then the original source if that fails: no setting needed. */
  const start = async (id: string, _legacy?: boolean) => {
    setDownloading({ id, received: 0 });
    try {
      try { await api.downloadModel(id, true); }
      catch (e) { if (rawErrorText(e).includes("取消")) throw e; setDownloading({ id, received: 0 }); await api.downloadModel(id, false); } // i18n-ignore (matches the backend's cancel message)
      onDone(); return true;
    }
    catch (e) { if (!rawErrorText(e).includes("取消")) useStore.getState().notify(t("模型下载失败：{error}", { error: errorText(e) })); return false; } // i18n-ignore (matches the backend's cancel message)
    finally { setDownloading(null); }
  };
  return { downloading, start, cancel: (id: string) => api.cancelJob(`model:${id}`) };
}

/** The one line under the tabs that says how this episode's subtitles stand, with the one action that applies. */
function StatusLine({ left, text, spin, progress, action, right, title }: { left?: React.ReactNode; text: React.ReactNode; spin?: boolean; progress?: number; action?: React.ReactNode; right?: React.ReactNode; title?: string }) {
  return (
    <div className="status-line" data-source={title}>
      {left}
      <span className="status-text" key={typeof text === "string" ? text : undefined}>{spin && <Loader2 size={13} className="spin" />}{text}</span>
      {progress != null && <div className="bar slim"><div style={{ width: `${Math.max(2, progress * 100)}%` }} /></div>}
      <div className="spacer" />
      {action}
      {right}
    </div>
  );
}

export function TranscriptPanel({ ep }: { ep: Episode }) {
  const transcript = useJobs((s) => s.transcripts[ep.id]);
  const official = useJobs((s) => s.officialChecked[ep.id]);
  const officialError = useJobs((s) => s.officialError[ep.id]);
  const translation = useJobs((s) => s.translations[ep.id]);
  // Not `time`: it changes several times a second, and only the active line matters here (selected below).
  const { player, settings, saveSettings, current, openSettings } = useStore(useShallow((s) => ({ player: s.player, settings: s.settings, saveSettings: s.saveSettings, current: s.current, openSettings: s.openSettings })));
  const isCurrent = current?.id === ep.id;
  const list = useRef<HTMLDivElement>(null);
  const userScrolled = useRef(0);
  const searchInput = useRef<HTMLInputElement>(null);
  const [query, setQuery] = useState("");
  const [searching, setSearching] = useState(false);
  const [hit, setHit] = useState(0);
  const clipTools = useClipTools();
  const [showHighlightsRaw, setShowHighlights] = useState(false);
  const showHighlights = showHighlightsRaw && clipTools;
  const hlBundle = useHighlights((s) => s.bundles[ep.id]);
  const hlLoading = useHighlights((s) => !!s.loading[ep.id]);
  const [startError, setStartError] = useState<string | null>(null);
  const [canUpgrade, setCanUpgrade] = useState(false);
  const segments = transcript?.segments ?? NO_SEGMENTS;
  // Re-renders when the active line changes, not on every time update.
  const active = useStore((s) => (s.current?.id === ep.id ? activeIndex(segments, s.time) : -1));
  const isPlaying = useStore((s) => s.current?.id === ep.id && s.playing);
  // Stable, so the memoised rows don't all re-render whenever this panel does.
  const seek = useCallback((t: number) => { if (isCurrent) player?.seek(t); }, [isCurrent, player]);
  // 对齐字幕 (podcast audio playing here only): while its bar is open, a click on a line lines it up instead.
  const canAlign = isCurrent && !ep.videoId && !!ep.audioUrl;
  const [aligning, setAligning] = useState(false);
  useEffect(() => { if (!canAlign) setAligning(false); }, [canAlign]);
  const wantsZh = transcript ? needsTranslation(transcript) : false;
  const epMode = subtitleModeFor(settings, ep);
  const mode = epMode === "off" ? "off" : wantsZh ? epMode : "original";
  // "关闭" only hides the subtitles over the video; the transcript here still shows, bilingual where it can.
  const lineMode = mode === "off" ? (wantsZh ? "bilingual" : "original") : mode;
  const english = useMemo(
    () => (transcript ? looksEnglish(transcript.language, segments.slice(0, 20).map((x) => x.text).join(" ")) : false),
    [transcript, segments],
  );
  const marks = useLearnMarks(english);
  const learning = !!marks || (english && settings.learnOn);
  const [card, setCard] = useState<CardTarget | null>(null);
  const [shadow, setShadowRaw] = useState(false);
  const setShadow = useCallback((v: boolean) => { setShadowRaw(v); if (v) setAligning(false); }, []);
  const openAlign = useCallback(() => { setShadowRaw(false); setAligning(true); }, []);
  useShadowing(shadow && isCurrent, ep.id, segments, transcript?.language?.startsWith("en") ? "en" : "auto");
  useEffect(() => { if (!learning) setShadowRaw(false); }, [learning]);
  const closeCard = useCallback(() => setCard(null), []);
  // Looking a word up pauses playback (as Language Reactor and asbplayer do), and closing the card resumes it
  // when the card was what paused it.
  const pausedForCard = useRef(false);
  const cardOpen = !!card;
  useEffect(() => {
    const s = useStore.getState();
    if (cardOpen) {
      if (settings.learnPauseCard && isCurrent && s.playing) { s.player?.toggle(); pausedForCard.current = true; }
    } else if (pausedForCard.current) {
      pausedForCard.current = false;
      if (!s.playing) s.player?.toggle();
    }
  }, [cardOpen]);
  // A click on a word opens its card instead of seeking (the rest of the line still seeks).
  const onWordClick = (e: React.MouseEvent) => {
    if (!marks || !settings.learnCard) return;
    const el = (e.target as HTMLElement).closest<HTMLElement>(".lw");
    if (!el) return;
    // Selecting the word itself (a double-click, or dragging across it) also opens its card; a selection that
    // reaches beyond the word is left to the selection tools.
    const sel = window.getSelection();
    const picked = !!sel?.toString().trim();
    if (picked && !(sel!.anchorNode && el.contains(sel!.anchorNode) && sel!.focusNode && el.contains(sel!.focusNode))) return;
    const i = Number(el.closest<HTMLElement>("[data-i]")?.dataset.i);
    const seg = segments[i];
    if (!seg) return;
    e.stopPropagation();
    if (picked) sel!.removeAllRanges();
    // A plain click on the open word closes it; the second click of a double-click keeps it open.
    const toggle = !picked && e.detail < 2;
    setCard((c) => (toggle && c?.el === el ? null : c?.el === el ? c : { el, token: el.textContent ?? "", seg, zh: translation?.items[i] }));
  };

  useEffect(() => { if (transcript) void loadTranslation(ep.id, transcript.segments.length); }, [transcript, ep.id]);
  useEffect(() => { void llmReady("translate").then(setCanUpgrade); }, [settings.translateProvider, settings.translateMode, settings.providers]);


  const readIndex = useZhRead((s) => (s.epId === ep.id ? s.index : null));
  const readingHere = readIndex != null;
  const [readSetup, setReadSetup] = useState(false);
  const zhItems = translation?.items;
  // While reading Chinese, or in the Chinese-only view, a click on a line reads aloud from there instead of playing the original video.
  const readFrom = useCallback((i: number) => void startZhRead(ep.id, zhItems ?? [], i, (m) => { useStore.getState().notify(tr(m)); if (isKeyError(m)) setReadSetup(true); }), [ep.id, zhItems]);
  const readAt = useCallback((_start: number, i: number) => readFrom(i), [readFrom]);
  const readBox = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!readSetup) return;
    const h = (e: PointerEvent) => { if (!readBox.current?.contains(e.target as Node)) setReadSetup(false); };
    document.addEventListener("pointerdown", h);
    return () => document.removeEventListener("pointerdown", h);
  }, [readSetup]);
  // Leaving the episode stops reading it.
  useEffect(() => () => { if (useZhRead.getState().epId === ep.id) stopZhRead(); }, [ep.id]);
  // Highlight headings: stable callbacks, so the memoised rows don't re-render every tick.
  useClipWarm(ep, segments);
  const onHlPreview = useCallback((c: HighlightClip) => previewEpisodeRange(ep, c.start, c.end), [ep]);
  const onHlClip = useCallback((c: HighlightClip) => openVideoClipDialog({ ep, segments, zh: zhItems, clip: c }), [ep, segments, zhItems]);
  // ⌘F searches the transcript; Enter / ⇧Enter step through matches.
  useEffect(() => {
    const k = (e: KeyboardEvent) => { if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") { e.preventDefault(); setShowHighlights(false); setSearching(true); setTimeout(() => searchInput.current?.focus(), 0); } };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, []);

  useEffect(() => { setShowHighlights(false); void loadHighlights(ep.id); }, [ep.id]);

  const hlMap = useMemo(() => {
    const starts = new Map<number, HighlightClip>();
    const inside = new Set<number>();
    const ends = new Set<number>();
    for (const c of (clipTools && hlBundle?.clips) || []) {
      starts.set(c.startIdx, c);
      ends.add(c.endIdx);
      for (let i = c.startIdx; i <= c.endIdx; i++) inside.add(i);
    }
    return { starts, inside, ends };
  }, [hlBundle, clipTools]);

  const matches = useMemo(() => {
    if (!query) return null;
    const q = query.toLowerCase();
    return segments.map((s, i) => (s.text.toLowerCase().includes(q) || translation?.items[i]?.toLowerCase().includes(q) ? i : -1)).filter((i) => i >= 0);
  }, [query, segments, translation]);
  const rows = useMemo(() => matches ?? segments.map((_, i) => i), [matches, segments]);
  const virtual = rows.length > VIRTUAL_THRESHOLD;
  const activePos = useMemo(() => (active < 0 ? -1 : matches ? matches.indexOf(active) : active), [active, matches]);

  const heightsRef = useRef<Float32Array>(new Float32Array(0));
  const lastWidthRef = useRef(0);
  const [win, setWin] = useState<{ start: number; end: number; topPad: number; bottomPad: number }>({ start: 0, end: 80, topPad: 0, bottomPad: 0 });
  const [activeOffscreen, setActiveOffscreen] = useState(false);
  const followPaused = useRef(false);
  const programmaticUntil = useRef(0);
  const prevActiveRef = useRef(active);

  const checkActiveOffscreen = useCallback((box: HTMLDivElement | null, pos: number) => {
    if (!box || !isCurrent || pos < 0 || query) {
      setActiveOffscreen(false);
      return false;
    }
    if (virtual) {
      const { top, bottom } = rowBounds(heightsRef.current, pos);
      const st = box.scrollTop;
      const vh = box.clientHeight;
      const off = bottom < st + 8 || top > st + vh - 8;
      setActiveOffscreen(off);
      return off;
    }
    const el = box.querySelector<HTMLElement>("[data-active]");
    if (!el) { setActiveOffscreen(false); return false; }
    const br = box.getBoundingClientRect();
    const er = el.getBoundingClientRect();
    const off = er.bottom < br.top + 8 || er.top > br.bottom - 8;
    setActiveOffscreen(off);
    return off;
  }, [isCurrent, query, virtual]);

  const rowRefs = useRef(new Map<number, HTMLElement>());
  const rowRoRef = useRef<ResizeObserver | null>(null);
  const syncWindowRef = useRef<() => void>(() => {});
  const syncRaf = useRef(0);

  const scheduleSync = useCallback(() => {
    if (syncRaf.current) return;
    syncRaf.current = requestAnimationFrame(() => {
      syncRaf.current = 0;
      syncWindowRef.current();
    });
  }, []);

  useEffect(() => () => { if (syncRaf.current) cancelAnimationFrame(syncRaf.current); }, []);

  syncWindowRef.current = () => {
    const box = list.current;
    if (!box) return;
    if (virtual) {
      const next = computeWindow(heightsRef.current, box.scrollTop, box.clientHeight || 600);
      setWin((prev) =>
        prev.start === next.start &&
        prev.end === next.end &&
        Math.abs(prev.topPad - next.topPad) < 1 &&
        Math.abs(prev.bottomPad - next.bottomPad) < 1
          ? prev
          : next,
      );
    }
    const off = checkActiveOffscreen(box, activePos);
    if (performance.now() > programmaticUntil.current) {
      if (off) followPaused.current = true;
      else followPaused.current = false;
    }
  };

  const registerRow = useCallback((k: number, el: HTMLDivElement | null) => {
    const prev = rowRefs.current.get(k);
    if (prev && prev !== el) {
      rowRoRef.current?.unobserve(prev);
      rowRefs.current.delete(k);
    }
    if (el) {
      el.dataset.k = String(k);
      rowRefs.current.set(k, el);
      rowRoRef.current?.observe(el);
    }
  }, []);

  useEffect(() => {
    if (!virtual) {
      heightsRef.current = new Float32Array(0);
      setWin({ start: 0, end: rows.length, topPad: 0, bottomPad: 0 });
      return;
    }
    const est = lineMode === "bilingual" ? 84 : 46;
    heightsRef.current = new Float32Array(rows.length).fill(est);
    const box = list.current;
    const st = box?.scrollTop ?? 0;
    const vh = box?.clientHeight || 600;
    lastWidthRef.current = box?.clientWidth ?? 0;
    setWin(computeWindow(heightsRef.current, st, vh));
    const raf = requestAnimationFrame(() => {
      let changed = false;
      for (const [k, el] of rowRefs.current) {
        if (k < heightsRef.current.length) {
          const h = el.getBoundingClientRect().height;
          if (h > 0 && Math.abs(heightsRef.current[k] - h) > 0.5) {
            heightsRef.current[k] = h;
            changed = true;
          }
        }
      }
      if (changed) scheduleSync();
    });
    return () => cancelAnimationFrame(raf);
  }, [virtual, rows.length, ep.id, lineMode, query, settings.subtitleSize, settings.subtitleFont, scheduleSync]);

  useEffect(() => {
    if (!virtual) return;
    const ro = new ResizeObserver((entries) => {
      let changed = false;
      const box = list.current;
      for (const entry of entries) {
        const el = entry.target as HTMLElement;
        const k = Number(el.dataset.k);
        if (!Number.isFinite(k) || k >= heightsRef.current.length) continue;
        const h = entry.borderBoxSize?.[0]?.blockSize ?? entry.contentRect.height;
        if (h > 0 && Math.abs(heightsRef.current[k] - h) > 0.5) {
          const delta = h - heightsRef.current[k];
          heightsRef.current[k] = h;
          if (box && performance.now() > programmaticUntil.current && el.getBoundingClientRect().bottom <= box.getBoundingClientRect().top + 4) {
            box.scrollTop += delta;
          }
          changed = true;
        }
      }
      if (changed) scheduleSync();
    });
    rowRoRef.current = ro;
    for (const el of rowRefs.current.values()) ro.observe(el);
    return () => {
      ro.disconnect();
      rowRoRef.current = null;
    };
  }, [virtual, scheduleSync]);

  useEffect(() => {
    const box = list.current;
    if (!box) return;
    const ro = new ResizeObserver(() => {
      const w = box.clientWidth;
      if (virtual && Math.abs(w - lastWidthRef.current) > 8) {
        lastWidthRef.current = w;
        heightsRef.current.fill(lineMode === "bilingual" ? 84 : 46);
        for (const [k, el] of rowRefs.current) {
          if (k < heightsRef.current.length) {
            const h = el.getBoundingClientRect().height;
            if (h > 0) heightsRef.current[k] = h;
          }
        }
      }
      scheduleSync();
    });
    ro.observe(box);
    return () => ro.disconnect();
  }, [virtual, lineMode, scheduleSync, !!transcript, showHighlights]);

  const scrollToPos = useCallback((pos: number, behavior: ScrollBehavior) => {
    const box = list.current;
    if (!box || pos < 0 || pos >= rows.length) return;
    const targetIdx = rows[pos];
    const el = box.querySelector<HTMLElement>(`[data-i="${targetIdx}"]`);
    programmaticUntil.current = performance.now() + (behavior === "smooth" ? 650 : 140);
    box.dataset.autoScrollUntil = String(programmaticUntil.current);
    if (el) {
      // Scrolls the list only: scrollIntoView would also move the episode column when the list sits under the player.
      const top = el.getBoundingClientRect().top - box.getBoundingClientRect().top + box.scrollTop;
      box.scrollTo({ top: Math.max(0, top - box.clientHeight / 2 + el.offsetHeight / 2), behavior });
      return;
    }
    if (!virtual) return;
    const { top, height } = rowBounds(heightsRef.current, pos);
    const dest = Math.max(0, top - box.clientHeight / 2 + height / 2);
    const far = Math.abs(dest - box.scrollTop) > box.clientHeight * 2;
    box.scrollTo({ top: dest, behavior: far ? "auto" : behavior });
    if (far || behavior === "auto") {
      setWin(computeWindow(heightsRef.current, dest, box.clientHeight || 600));
    }
    requestAnimationFrame(() => requestAnimationFrame(() => {
      const mounted = list.current?.querySelector<HTMLElement>(`[data-i="${targetIdx}"]`);
      if (mounted) {
        programmaticUntil.current = performance.now() + 140;
        if (list.current) list.current.dataset.autoScrollUntil = String(programmaticUntil.current);
        mounted.scrollIntoView({ block: "center", behavior: "auto" });
      }
    }));
  }, [rows, virtual]);

  useEffect(() => {
    const jumped = Math.abs(active - prevActiveRef.current) > 3;
    prevActiveRef.current = active;
    if (jumped) {
      followPaused.current = false;
      userScrolled.current = 0;
    }
    if (active < 0 || query || !isCurrent || cardOpen) {
      setActiveOffscreen(false);
      return;
    }
    if (followPaused.current || Date.now() - userScrolled.current < 5000) {
      checkActiveOffscreen(list.current, activePos);
      return;
    }
    setActiveOffscreen(false);
    scrollToPos(activePos, jumped ? "auto" : "smooth");
  }, [active, activePos, query, isCurrent, cardOpen, scrollToPos, checkActiveOffscreen]);

  // The line being read aloud stays in view, like the one being played.
  useEffect(() => {
    if (readIndex == null || Date.now() - userScrolled.current < 5000 || query) return;
    scrollToPos(rows.indexOf(readIndex), "smooth");
  }, [readIndex, query, rows, scrollToPos]);

  useEffect(() => { setHit(0); }, [query]);
  useEffect(() => {
    if (!matches?.length) return;
    scrollToPos(hit, "auto");
  }, [hit, matches, scrollToPos]);

  const jumpToActive = () => {
    followPaused.current = false;
    userScrolled.current = 0;
    setActiveOffscreen(false);
    scrollToPos(activePos, "smooth");
  };

  if (!transcript) return <NoTranscript ep={ep} official={official} officialError={officialError} />;

  const total = segments.length;
  const done = Math.min(total, translation?.items.slice(0, total).filter((x) => x != null).length ?? 0);
  const running = !!translation?.running;
  const providerName = providerFor(settings, "translate")?.name;
  const engineName = translation?.engine === "shared" ? t("共享译文") : translation?.engine === "youtube" ? t("节目方中文字幕") : translation?.engine === "google" ? t("谷歌翻译") : t("{name}翻译", { name: providerName ? t(providerName) : t("模型") });
  const error = startError ?? translation?.error;
  const pending = wantsZh && (running || (!error && done < total));
  const start = (force?: "llm" | "google") => { setStartError(null); translateEpisode(ep, transcript, force).catch((e) => setStartError(rawErrorText(e))); };

  const modeOptions = wantsZh
    ? (["bilingual", "translation", "original", "off"] as const)
    : (["original", "off"] as const);
  const modeControl = (
    <div className="seg-control" role="radiogroup" aria-label={t("字幕显示（快捷键 C）")}>
      {modeOptions.map((m) => (
        <button key={m} role="radio" aria-checked={mode === m} className={mode === m ? "on" : ""} onClick={() => saveSettings(subtitleModePatch(settings, ep, m))}>
          {m === "bilingual" ? t("双语") : m === "translation" ? t("中文") : m === "original" ? t("原文") : t("关闭")}
        </button>
      ))}
    </div>
  );
  const searchBtn = <>
    {wantsZh && done > 0 && (mode === "translation" || readingHere) && (
      <div className="read-aloud zh-read" ref={readBox}>
        {readingHere
          ? <button className="icon-btn on" title={t("停止朗读")} aria-label={t("停止朗读")} onClick={stopZhRead}><Square size={13} fill="currentColor" /></button>
          : <button className="icon-btn" title={t("朗读中文")} aria-label={t("朗读中文")} onClick={() => readFrom(Math.max(0, active))}><Volume2 size={16} /></button>}
        <RatePicker />
        <button className="icon-btn sm" aria-label={t("朗读设置")} title={t("朗读设置")} aria-expanded={readSetup} onClick={() => setReadSetup(!readSetup)}><ChevronDown size={13} /></button>
        {readSetup && <ReadAloudSettings onClose={() => setReadSetup(false)} />}
      </div>
    )}
    {transcript.source.startsWith("youtube") && <TrackPicker ep={ep} language={transcript.language} auto={transcript.source === "youtube-auto"} />}
    {total > 0 && clipTools && (
      <button
        type="button"
        className={`hl-toggle ${showHighlights ? "on" : ""} ${hlBundle?.clips.length ? "has" : ""}`}
        title={t("高光金句片段")}
        aria-pressed={showHighlights}
        onClick={() => { setShowHighlights((v) => !v); if (!showHighlights) setSearching(false); }}
      >
        {hlLoading ? <Loader2 size={13} className="spin" /> : <Sparkles size={13} />}
        <span>{t("高光")}</span>
        {hlBundle?.clips.length ? <span className="hl-badge">{hlBundle.clips.length}</span> : null}
      </button>
    )}
    <button className="icon-btn" title={t("搜索文稿（⌘F）")} onClick={() => { setShowHighlights(false); setSearching(true); setTimeout(() => searchInput.current?.focus(), 0); }}><Search size={15} /></button>
    {total > 0 && <PanelMenu ep={ep} segments={segments} zh={translation?.items} onRetranslate={wantsZh && canUpgrade ? () => start("llm") : undefined} learn={learning ? { segments, zh: translation?.items, shadow, setShadow, canShadow: isCurrent } : undefined} onAlign={canAlign ? openAlign : undefined} />}
  </>;
  const source = SOURCE_LABEL[transcript.source] ? t(SOURCE_LABEL[transcript.source]) : transcript.source;

  let status: React.ReactNode;
  if (!wantsZh) status = <StatusLine left={<span className="source-tag">{source}</span>} text="" right={searchBtn} />;
  else if (running) status = <StatusLine left={modeControl} text={t("{engine} · 正在翻译 {done}/{total}", { engine: engineName, done, total })} spin progress={done / total} action={<button className="btn ghost sm" onClick={() => stopTranslation(ep.id)}>{t("停止")}</button>} right={searchBtn} />;
  else if (error && /设置|API Key|模型/.test(error)) status = <StatusLine left={modeControl} text={tr(error)} action={<button className="btn primary sm" onClick={() => openSettings("model")}>{t("去设置")}</button>} right={searchBtn} />;
  else if (done < total && (error || translation?.failed)) status = <StatusLine left={modeControl} text={`${t("已翻译 {done}/{total}", { done, total })}${error ? ` · ${tr(error).slice(0, 40)}` : ""}`} action={<button className="btn primary sm" onClick={() => start()}>{t("重试")}</button>} right={searchBtn} />;
  else if (done < total) status = <StatusLine left={modeControl} text={done ? t("已翻译 {done}/{total}", { done, total }) : source} action={<button className="btn primary sm" onClick={() => start()}>{done ? t("继续翻译") : t("翻译成中文")}</button>} right={searchBtn} />;
  else status = <StatusLine left={modeControl} text="" title={`${source} · ${engineName}`} action={translation?.engine === "google" && canUpgrade ? <button className="btn ghost sm" onClick={() => start("llm")}>{t("用模型重译")}</button> : undefined} right={searchBtn} />;

  const visibleStart = virtual ? win.start : 0;
  const visibleEnd = virtual ? win.end : rows.length;
  const visibleRows = virtual ? rows.slice(visibleStart, visibleEnd) : rows;
  const hasHours = (segments[segments.length - 1]?.start ?? 0) >= 3600;

  return (
    <div className="transcript">
      {status}
      {searching && (
        <div className="search-row">
          <Search size={14} />
          <input ref={searchInput} spellCheck={false} value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t("搜索中文或原文")}
            onKeyDown={(e) => {
              if (e.key === "Escape") { setQuery(""); setSearching(false); }
              if (e.key === "Enter" && matches?.length) setHit((h) => (h + (e.shiftKey ? -1 : 1) + matches.length) % matches.length);
            }} />
          {matches && (
            <>
              <span className="muted small search-hits">{matches.length ? `${hit + 1}/${matches.length}` : t("无结果")}</span>
              {matches.length > 1 && (
                <>
                  <button type="button" className="icon-btn sm" title={t("上一处（⇧↵）")} aria-label={t("上一处")} onClick={() => setHit((h) => (h - 1 + matches.length) % matches.length)}><ChevronUp size={14} /></button>
                  <button type="button" className="icon-btn sm" title={t("下一处（↵）")} aria-label={t("下一处")} onClick={() => setHit((h) => (h + 1) % matches.length)}><ChevronDown size={14} /></button>
                </>
              )}
            </>
          )}
          <button className="icon-btn sm" onClick={() => { setQuery(""); setSearching(false); }} aria-label={t("关闭搜索")}><X size={14} /></button>
        </div>
      )}
      {aligning && canAlign && <AlignBar onClose={() => setAligning(false)} />}
      {shadow && isCurrent && (
        <ShadowBar onExit={() => setShadow(false)} />
      )}
      {showHighlights && !searching ? (
        <HighlightsPanel
          ep={ep}
          segments={segments}
          zh={translation?.items}
          onJumpToTranscript={(idx, start) => {
            setShowHighlights(false);
            seek(start);
            window.setTimeout(() => scrollToPos(rows.indexOf(idx), "smooth"), 50);
          }}
        />
      ) : (
      <div
        className={`lines${marks && settings.learnCard ? " learning" : ""}${hasHours ? " has-hours" : ""}`}
        ref={list}
        onWheel={() => (userScrolled.current = Date.now())}
        onTouchStart={() => (userScrolled.current = Date.now())}
        onScroll={scheduleSync}
        onClickCapture={onWordClick}
      >
        {english && <LearnTip />}
        {virtual && win.topPad > 0 && <div style={{ height: win.topPad }} aria-hidden />}
        {visibleRows.map((i, offset) => {
          const k = visibleStart + offset;
          const hl = !matches ? hlMap.starts.get(i) : undefined;
          const hlClass = !matches && hlMap.inside.has(i)
            ? `hl-in${hl ? " hl-start" : ""}${hlMap.ends.has(i) ? " hl-end" : ""}`
            : undefined;
          return (
            <Row
              key={i}
              i={i}
              k={k}
              seg={segments[i]}
              zh={translation?.items[i]}
              active={i === active}
              playingActive={i === active && isPlaying}
              mode={lineMode}
              pending={pending}
              onSeek={aligning && canAlign ? anchorHere : (readingHere || (mode === "translation" && done > 0)) ? readAt : seek}
              query={query}
              current={!!matches && k === hit}
              marks={marks}
              reading={i === readIndex}
              hlClass={hlClass}
              hl={hl}
              isVideo={!!ep.videoId}
              onHlPreview={onHlPreview}
              onHlClip={ep.videoId ? onHlClip : undefined}
              registerRow={virtual ? registerRow : undefined}
            />
          );
        })}
        {virtual && win.bottomPad > 0 && <div style={{ height: win.bottomPad }} aria-hidden />}
        {card && <WordCard target={card} ep={ep} onClose={closeCard} />}
        {!matches && total > 0 && visibleEnd >= rows.length && (
          <div className={`lines-end ${active === total - 1 ? "reached" : ""}`}>
            <FlatLineArt />
            {active === total - 1 && <p>{t("到最后一句了，耳朵辛苦了")}</p>}
          </div>
        )}
      </div>
      )}
      {isCurrent && active >= 0 && !query && activeOffscreen && (
        <button type="button" className="jump-active" onClick={jumpToActive}>
          <Crosshair size={13} />
          <span>{t("回到当前位置")}</span>
        </button>
      )}
    </div>
  );
}

/** "…" on the subtitle bar: the rarer tools (本集术语表, retranslating with the model, exporting). */
interface LearnMenu { segments: Segment[]; zh: (string | null | undefined)[] | undefined; shadow: boolean; setShadow: (v: boolean) => void; canShadow: boolean }

function PanelMenu({ ep, segments, zh, onRetranslate, learn, onAlign }: { ep: Episode; segments: Segment[]; zh: (string | null | undefined)[] | undefined; onRetranslate?: () => void; learn?: LearnMenu; onAlign?: () => void }) {
  const [menu, setMenu] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [gloss, setGloss] = useState(false);
  const [words, setWords] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menu) return;
    const h = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setMenu(false); };
    document.addEventListener("pointerdown", h);
    return () => document.removeEventListener("pointerdown", h);
  }, [menu]);
  return (
    <div className="panel-menu" ref={box}>
      <button className="icon-btn" title={t("更多")} aria-label={t("更多")} aria-expanded={menu} onClick={() => setMenu(!menu)}><Ellipsis size={15} /></button>
      {menu && (
        <div className="panel-menu-pop" role="menu">
          {learn && <>
            <button role="menuitem" onClick={() => { setMenu(false); setWords(true); }}><ListChecks size={14} /> {t("本期生词…")}</button>
            <button role="menuitemcheckbox" aria-checked={learn.shadow} disabled={!learn.canShadow} title={learn.canShadow ? undefined : t("先播放这一集")} onClick={() => { setMenu(false); learn.setShadow(!learn.shadow); }}><Mic size={14} /> {learn.shadow ? t("退出跟读模式") : t("跟读模式")}</button>
          </>}
          {learn && onRetranslate && <div className="menu-sep" role="separator" />}
          {onRetranslate && <>
            <button role="menuitem" onClick={() => { setMenu(false); setGloss(true); }}><BookA size={14} /> {t("本集术语表…")}</button>
            <button role="menuitem" onClick={() => { setMenu(false); onRetranslate(); }}><RefreshCw size={14} /> {t("用模型重新翻译")}</button>
          </>}
          {onAlign && (learn || onRetranslate) && <div className="menu-sep" role="separator" />}
          {onAlign && <button role="menuitem" onClick={() => { setMenu(false); onAlign(); }}><AudioLines size={14} /> {t("字幕和声音对不上…")}</button>}
          {(learn || onRetranslate || onAlign) && <div className="menu-sep" role="separator" />}
          <button role="menuitem" onClick={() => { setMenu(false); setExporting(true); }}><Download size={14} /> {t("导出字幕…")}</button>
        </div>
      )}
      {exporting && <SubtitleExport ep={ep} segments={segments} zh={zh} onClose={() => setExporting(false)} />}
      {gloss && onRetranslate && <Glossary ep={ep} onRetranslate={onRetranslate} onClose={() => setGloss(false)} />}
      {words && learn && <EpisodeWords ep={ep} segments={learn.segments} zh={learn.zh} onClose={() => setWords(false)} />}
    </div>
  );
}

/** The episode's names and terms (made before translating), editable; edits are kept when it is remade. */
function Glossary({ ep, onRetranslate, onClose }: { ep: Episode; onRetranslate: () => void; onClose: () => void }) {
  const [open, setOpenRaw] = useState(false);
  const setOpen = (v: boolean) => { setOpenRaw(v); if (!v) onClose(); };
  useEffect(() => { void load(); }, []);
  const [ctx, setCtx] = useState<EpisodeContext | null>(null);
  const [rows, setRows] = useState<{ src: string; zh: string; user?: boolean }[]>([]);
  useLang((s) => s.lang);
  const load = async () => {
    let c: EpisodeContext | null = null;
    try { const raw = await api.getBlob(ep.id, "ctx"); c = raw ? JSON.parse(raw) : null; } catch { c = null; }
    setCtx(c); setRows(c?.terms ?? []); setOpen(true);
  };
  const save = async (andRetranslate: boolean) => {
    const terms = rows.map((r) => ({ src: r.src.trim(), zh: r.zh.trim(), user: true })).filter((r) => r.src && r.zh);
    const next: EpisodeContext = { summary: ctx?.summary ?? "", speakers: ctx?.speakers ?? [], terms, basis: ctx?.basis };
    await api.setBlob(ep.id, "ctx", JSON.stringify(next));
    setOpen(false);
    if (andRetranslate) onRetranslate();
  };
  return (
    <>
      {open && (
        <Modal title={t("本集术语表")} onClose={() => setOpen(false)}>
          <div className="gloss">
            <p className="muted small">{t("翻译每一句时都会照这里的译法，保证人名和术语前后一致。改动会保留。")}</p>
            {ctx?.summary && <p className="gloss-summary">{ctx.summary}</p>}
            <div className="gloss-list">
              {rows.map((r, i) => (
                <div className="gloss-row" key={i}>
                  <input value={r.src} placeholder={t("原文")} onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, src: e.target.value } : x)))} />
                  <span className="arrow">→</span>
                  <input value={r.zh} placeholder={t("译为")} onChange={(e) => setRows(rows.map((x, j) => (j === i ? { ...x, zh: e.target.value } : x)))} />
                  <button className="icon-btn sm" aria-label={t("删除")} onClick={() => setRows(rows.filter((_, j) => j !== i))}><X size={13} /></button>
                </div>
              ))}
              {!rows.length && <p className="muted small">{t("还没有术语。翻译开始时会自动整理，也可以自己添加。")}</p>}
            </div>
            <div className="gloss-foot">
              <button className="btn sm" onClick={() => setRows([...rows, { src: "", zh: "" }])}>{t("添加")}</button>
              <span className="spacer" />
              <button className="btn sm" onClick={() => void save(false)}>{t("保存")}</button>
              <button className="btn sm primary" onClick={() => void save(true)}>{t("保存并重新翻译")}</button>
            </div>
          </div>
        </Modal>
      )}
    </>
  );
}

const trackCache = new Map<string, { language: string; auto: boolean }[]>();
/** The subtitle language, when the video has captions in more than one. */
function TrackPicker({ ep, language, auto }: { ep: Episode; language: string; auto: boolean }) {
  const [tracks, setTracks] = useState(trackCache.get(ep.id) ?? null);
  const [busy, setBusy] = useState(false);
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useLang((s) => s.lang);
  useEffect(() => {
    if (!open) return;
    const h = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false); };
    const k = (e: KeyboardEvent) => { if (e.key === "Escape") setOpen(false); };
    document.addEventListener("pointerdown", h); document.addEventListener("keydown", k);
    return () => { document.removeEventListener("pointerdown", h); document.removeEventListener("keydown", k); };
  }, [open]);
  useEffect(() => {
    if (tracks) return;
    void api.youtubeTracks(ep.id).then((list) => { trackCache.set(ep.id, list); setTracks(list); }).catch(() => {});
  }, [ep.id]);
  if (!tracks || tracks.length < 2) return null;
  let names: Intl.DisplayNames | null = null;
  try { names = new Intl.DisplayNames([lang()], { type: "language" }); } catch { /* old WebView */ }
  const name = (l: string) => { try { return names?.of(l) ?? l; } catch { return l; } };
  const label = (x: { language: string; auto: boolean }) => `${name(x.language)}${x.auto ? ` · ${t("自动生成")}` : ""}`;
  const pick = async (x: { language: string; auto: boolean }) => {
    setOpen(false);
    if (x.language === language && x.auto === auto) return;
    setBusy(true);
    try { await switchYoutubeTrack(ep, x.language, x.auto); }
    catch (e) { useStore.getState().notify(t("切换字幕失败：{error}", { error: errorText(e) })); }
    finally { setBusy(false); }
  };
  return (
    <div className="panel-menu" ref={box}>
      <button className="track-pick" title={t("字幕语言")} aria-label={t("字幕语言")} aria-haspopup="menu" aria-expanded={open} disabled={busy} onClick={() => setOpen(!open)}>
        {busy ? <Loader2 size={13} className="spin" /> : <Languages size={14} />}
        <span>{label({ language, auto })}</span>
        <ChevronDown size={12} className={open ? "flip" : ""} />
      </button>
      {open && (
        <div className="panel-menu-pop" role="menu">
          {tracks.map((x) => {
            const on = x.language === language && x.auto === auto;
            return <button key={`${x.auto}:${x.language}`} role="menuitemradio" aria-checked={on} className={on ? "on" : ""} onClick={() => void pick(x)}><Check size={14} className="pick-check" />{label(x)}</button>;
          })}
        </div>
      )}
    </div>
  );
}

/** What the episode's pickers offer: Qwen3-ASR, models already on this computer, the main cloud services and
 *  whatever is in use. The rest are in Settings (the 「更多…」 entry opens it). */
const pickerModels = (models: ModelInfo[], s: Settings) => models.filter((m) => m.id === "qwen3-asr" || m.installed || (s.asrEngine === "local" && s.asrModel === m.id));
const pickerCloud = (s: Settings) => ASR_SERVICES.filter((a) => CLOUD_ASR_MAIN.includes(a.id) || s.asrEngine === a.id);

/** Episodes transcription was started for automatically this session (never twice). */
const autoStarted = new Set<string>();

function NoTranscript({ ep, official, officialError }: { ep: Episode; official?: string; officialError?: string }) {
  const asr = useJobs((s) => s.asr[ep.id]);
  const { settings, saveSettings, player, current } = useStore(useShallow((s) => ({ settings: s.settings, saveSettings: s.saveSettings, player: s.player, current: s.current })));
  const { models, refresh } = useModels();
  const { downloading, start, cancel } = useModelDownload(refresh);
  const [options, setOptions] = useState(false);
  const model = models.find((m) => m.id === settings.asrModel) ?? models[0];
  const cloud = settings.asrEngine !== "local" ? settings.cloudAsr[settings.asrEngine] : undefined;
  const [cloudKey, setCloudKey] = useState<boolean | null>(null);
  const settingsOpen = useStore((s) => s.settingsOpen);
  useEffect(() => {
    setCloudKey(null);
    // DeepSeek recognition shares the Q&A key.
    if (cloud?.needsKey && !settingsOpen) void Promise.all([api.hasApiKey(`asr.${cloud.id}`), cloud.protocol === "deepseek" ? api.hasApiKey("deepseek") : Promise.resolve(false)]).then(([a, b]) => setCloudKey(a || b));
  }, [cloud?.id, settingsOpen]);
  const missingKey = !!cloud?.needsKey && cloudKey === false;
  const pickEngine = (value: string) => {
    if (value === "more") { useStore.getState().openSettings("asr"); return; }
    const [kind, id] = value.split(":");
    void saveSettings(kind === "cloud" ? { asrEngine: id } : { asrEngine: "local", asrModel: id });
  };
  const generate = async () => {
    if (!model) return;
    if (missingKey) { useStore.getState().openSettings("asr"); return; }
    if (!cloud && !model.installed && !(await start(model.id, settings.mirrorFirst))) return;
    void generateTranscript(ep, model.id, settings.asrLanguage);
  };
  const retry = () => useJobs.setState((s) => { const a = { ...s.asr }; delete a[ep.id]; return { asr: a }; });
  // Once a way of transcribing is ready (local model downloaded, or a cloud service the listener chose with its
  // key), pressing play on an episode that has no subtitles anywhere (cache, publisher, shared library: all
  // checked by now) starts transcribing by itself. The first time still takes a click (it downloads ~1 GB).
  const playing = useStore((s) => s.playing && s.current?.id === ep.id);
  const ready = !!model && (cloud ? cloudKey === true || !cloud.needsKey : model.installed);
  useEffect(() => {
    if (playing && ready && official === "none" && !asr && !autoStarted.has(ep.id)) { autoStarted.add(ep.id); void generateTranscript(ep, model!.id, settings.asrLanguage); }
  }, [playing, ready, official, !!asr, ep.id]);
  /** Stops the running transcription and waits until it has stopped. */
  const stopRunning = async () => {
    await api.cancelJob(ep.id);
    for (let i = 0; i < 80 && useJobs.getState().asr[ep.id]?.stage !== "error" && useJobs.getState().asr[ep.id]?.stage !== "done"; i++) await new Promise((r) => setTimeout(r, 100));
    retry();
  };
  /** Mid-way, switch to another way of transcribing: the running one stops, the new one starts from the beginning. */
  const switchEngine = async (value: string) => {
    if (value === "more") { useStore.getState().openSettings("asr"); return; }
    const [kind, id] = value.split(":");
    await saveSettings(kind === "cloud" ? { asrEngine: id } : { asrEngine: "local", asrModel: id });
    await stopRunning();
    const m = kind === "cloud" ? models.find((x) => x.id === settings.asrModel) ?? models[0] : models.find((x) => x.id === id);
    if (!m) return;
    if (kind !== "cloud" && !m.installed && !(await start(m.id, settings.mirrorFirst))) return;
    void generateTranscript(ep, m.id, settings.asrLanguage);
  };

  if (official === "searching" || official === undefined) {
    return <div className="transcript"><StatusLine text={t("正在查找字幕…")} spin /><SkeletonLines /></div>;
  }
  if (downloading) {
    const pct = downloading.total ? downloading.received / downloading.total : 0;
    return (
      <GenCard icon={<TrayArt busy />} title={t("正在准备识别模型")} note={t("只需下载这一次，完成后自动生成字幕。可以先听着。")}>
        <div className="gen-progress"><div className="bar live"><div style={{ width: `${Math.max(2, pct * 100)}%` }} /></div><span>{Math.round(pct * 100)}%</span></div>
        {downloading.total ? <p className="gen-size num">{bytes(downloading.received)} / {bytes(downloading.total)}</p> : null}
        <button className="btn ghost sm" onClick={() => cancel(downloading.id)}>{t("取消")}</button>
      </GenCard>
    );
  }
  if (asr && asr.stage !== "done") {
    if (asr.stage === "error") {
      return (
        <GenCard icon={<CassetteArt mode="tangled" />} title={t("字幕没有生成成功")} note={t("可以换一个识别模型，或检查网络后再试。")}>
          {asr.message && <p className="gen-warn" title={tr(asr.message)}><span>{plainError(tr(asr.message))}</span></p>}
          <button className="btn primary gen-go" onClick={retry}>{t("重试")}</button>
        </GenCard>
      );
    }
    const label = asr.message ? tr(asr.message) : asr.stage === "download" ? t("正在获取音频") : ep.duration ? t("正在识别 · {elapsed} / {total}", { elapsed: clock(asr.progress * ep.duration), total: clock(ep.duration) }) : t("正在识别");
    return (
      <div className="transcript">
        <StatusLine text={`${label} · ${Math.round(asr.progress * 100)}%`} spin progress={asr.progress} action={
          <span className="row-actions tight">
            <select className="engine-switch-mini" aria-label={t("换一种识别方式")} title={t("换一种识别方式（从头开始）")} value="" onChange={(e) => e.target.value && void switchEngine(e.target.value)}>
              <option value="">{t("换一种方式…")}</option>
              <optgroup label={t("本机模型 · 音频不出本机")}>
                {pickerModels(models, settings).filter((m) => !(settings.asrEngine === "local" && settings.asrModel === m.id)).map((m) => <option key={m.id} value={`local:${m.id}`}>{tr(m.name)}{m.installed ? "" : ` · ${sizeMb(m.sizeMb)}`}</option>)}
              </optgroup>
              <optgroup label={t("云端服务 · 更快，需要 Key")}>
                {pickerCloud(settings).filter((a) => settings.asrEngine !== a.id).map((a) => <option key={a.id} value={`cloud:${a.id}`}>{t(a.name)}</option>)}
              </optgroup>
              <option value="more">{t("更多")}…</option>
            </select>
            <button className="btn ghost sm" onClick={() => void stopRunning()}>{t("取消")}</button>
          </span>
        } />
        <div className="lines">
          {asr.live.map((s, i) => (
            <div className="line live" key={i} role="button" tabIndex={0} onClick={() => current?.id === ep.id && player?.seek(s.start)}>
              <span className="line-time">{clock(s.start)}</span>
              <div className="line-text"><p className="orig">{s.text}{i === asr.live.length - 1 && <span className="live-caret" aria-hidden />}</p></div>
            </div>
          ))}
          {!asr.live.length && (
            <div className="asr-wait">
              <CassetteArt mode={asr.stage === "download" ? "fetch" : "listen"} />
              <p className="muted">{asr.stage === "download" ? t("先把音频取回来，马上开始听写") : settings.asrModel === "qwen3-asr" ? t("识别完成后会一次性显示全部文字") : t("识别出的文字会实时出现在这里")}</p>
            </div>
          )}
        </div>
      </div>
    );
  }
  const needsDownload = !cloud && model && !model.installed;
  const size = model ? sizeMb(model.sizeMb) : "";
  return (
    <GenCard icon={<WaveToTextArt />} title={t("这集还没有字幕")}
      note={cloud ? t("用{name}生成，音频会上传到该服务，速度更快。生成后自动翻译成中文。", { name: t(cloud.name) }) : t("用本机模型生成，音频只在这台电脑上处理。生成后自动翻译成中文。")}>
      {official === "error" && officialError && <p className="gen-warn" title={tr(officialError)}><span>{t("节目自带的字幕没读到：{error}", { error: plainError(tr(officialError)) })}</span></p>}
      <button className="btn primary gen-go" onClick={generate} disabled={!model}>{missingKey ? t("先填写 API Key") : t("生成字幕")}</button>
      {needsDownload && <p className="gen-size">{t("第一次会先下载识别模型（约 {size}），之后不用再下载", { size })}</p>}
      {needsDownload && (() => {
        // A model this computer already has (ours, or another app's) saves the download.
        const have = models.find((m) => m.installed && !m.from) ?? models.find((m) => m.installed);
        return have ? <button className="link-btn gen-alt" onClick={() => saveSettings({ asrEngine: "local", asrModel: have.id })}>{have.from ? t("改用{app}里已有的 {name}，不用下载", { app: tr(have.from), name: have.name }) : t("改用已下载的 {name}", { name: tr(have.name) })}</button> : null;
      })()}
      {missingKey && <p className="gen-size">{t("用{name}识别需要它的 API Key，填好就能开始", { name: t(cloud!.name) })}</p>}
      <button className="link-btn gen-more" aria-expanded={options} onClick={() => setOptions(!options)}>{t("识别设置")}<ChevronDown size={12} className={options ? "flip" : ""} /></button>
      {!options && <NightNote />}
      {options && (
        <div className="gen-options">
          <label><span>{t("识别方式")}</span>
            <select value={cloud ? `cloud:${cloud.id}` : `local:${model?.id}`} onChange={(e) => pickEngine(e.target.value)}>
              <optgroup label={t("本机模型 · 音频不出本机")}>
                {pickerModels(models, settings).map((m) => <option key={m.id} value={`local:${m.id}`}>{tr(m.name)}{m.installed ? "" : ` · ${sizeMb(m.sizeMb)}`}</option>)}
              </optgroup>
              <optgroup label={t("云端服务 · 更快，需要 Key")}>
                {pickerCloud(settings).map((a) => <option key={a.id} value={`cloud:${a.id}`}>{t(a.name)}</option>)}
              </optgroup>
              <option value="more">{t("更多")}…</option>
            </select>
          </label>
        </div>
      )}
    </GenCard>
  );
}

/** An error for reading: long addresses carry no meaning for the listener (the full text stays in the tooltip). */
const plainError = (m: string) => m.replace(/[：:]?\s*https?:\/\/\S+/g, "").trim() || m;

/** The centred card shown in the subtitle panel while an episode has no subtitles yet. */
function GenCard({ icon, title, note, children }: { icon: React.ReactNode; title: string; note: string; children?: React.ReactNode }) {
  return (
    <div className="transcript">
      <div className="gen-wrap">
        <div className="gen-card">
          <span className="gen-art" aria-hidden>{icon}</span>
          <h3 className="gen-title">{title}</h3>
          <p className="gen-note">{note}</p>
          {children}
        </div>
      </div>
    </div>
  );
}
