import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { Check, ChevronDown, ChevronLeft, ChevronRight, Copy, FolderOpen, Image as ImageIcon, Loader2, Pause, Play, RotateCcw, Scissors, Volume2, VolumeX, X } from "lucide-react";
import { create } from "zustand";
import { useShallow } from "zustand/react/shallow";
import { api, errorText, rawErrorText, type Episode, type RenderVideoClipResult, type Segment, type VideoClipProgress } from "../lib/api";
import { ask } from "../lib/confirm";
import { clock } from "../lib/format";
import { buildPlatformPost, polishClipCopy, saveClipCopy, type HighlightClip, type PostPlatform } from "../lib/highlights";
import { kbd, t, tk, tr, tx, useLang } from "../lib/i18n";
import { llmReady, useJobs } from "../lib/jobs";
import { showName } from "../lib/picks";
import { aiLineBreaks, aiPairBreaks, pairsToBreak, textsToBreak, type PairLines } from "../lib/clipLines";
import { buildClipCues, canvasToPngBase64, clipGeometry, fromOldLayout, renderCueBand, renderStaticOverlay, type CardBg, type CardCrop, type ClipAspect, type ClipFit, type ClipLayout, type ClipVisualConfig, type SubStyleId } from "../lib/videoClipCanvas";
import { type CoverFontId } from "../lib/webfonts";
import { renderCoverCanvas, subjectCenterX, type CoverAspect, type CoverTemplateId } from "../lib/videoCoverCanvas";
import { type SubContent } from "../lib/subexport";
import { isZh } from "../lib/zh";
import { useStore } from "../store";
import { Modal } from "./Chrome";
import { ClipRangeBar } from "./ClipRangeBar";
import { TextField } from "./TextField";
import { ToggleChip } from "./ToggleChip";
import { noteExport } from "../lib/clipHistory";
import { betterStart, openingIssue } from "../lib/clipOpening";
import { cutAt, pauseCuts, toKeptTime } from "../lib/clipPauses";
import { CLIP_PRESETS, matchPreset, type ClipPreset, type CoverMode } from "../lib/clipPresets";
import { Seg } from "./ReadingSettings";

export interface ClipModalTarget {
  ep: Episode;
  segments: Segment[];
  zh?: (string | null | undefined)[];
  clip: HighlightClip;
  /** Batch export: these highlights, one after another, with the settings chosen on the first. */
  batch?: HighlightClip[];
}

export const useClipModal = create<{ target: ClipModalTarget | null }>(() => ({ target: null }));

export function openVideoClipDialog(target: ClipModalTarget) {
  useClipModal.setState({ target });
}

export function closeVideoClipDialog() {
  useClipModal.setState({ target: null });
}

const PREF_KEY = "subcast.vclip";
type SideTab = "clip" | "cover" | "copy";

interface ClipPrefs {
  /** Before aspect/fit (read once). */
  layout?: ClipLayout;
  aspect?: ClipAspect;
  fit?: ClipFit;
  cardCrop?: CardCrop;
  speed?: number;
  subScale?: number;
  coverAccent?: string;
  coverFont?: CoverFontId;
  coverScale?: number;
  coverTracking?: number;
  brandText?: string;
  watermark?: boolean;
  cardBg?: CardBg;
  subStyle?: SubStyleId;
  subLang?: SubContent;
  showTitle?: boolean;
  showSource?: boolean;
  coverTpl?: CoverTemplateId;
  coverMode?: CoverMode;
  embedThumbnail?: boolean;
  saveCoverSidecar?: boolean;
  extraLandscapeCover?: boolean;
  removePauses?: boolean;
  loudness?: boolean;
  smartFrame?: boolean;
}

function readPrefs(): ClipPrefs {
  try { return JSON.parse(localStorage.getItem(PREF_KEY) ?? "{}"); } catch { return {}; }
}

function savePrefs(p: ClipPrefs) {
  try { localStorage.setItem(PREF_KEY, JSON.stringify(p)); } catch { /* private mode */ }
}

const SUB_STYLES: { id: SubStyleId; name: string; swatchBg: string; swatchFg: string }[] = [
  { id: "cinema", name: tk("电影描边"), swatchBg: "#1b1c20", swatchFg: "#ffffff" },
  { id: "amber", name: tk("暖金高亮"), swatchBg: "#1b1c20", swatchFg: "#ffd866" },
  { id: "pill", name: tk("暗底胶囊"), swatchBg: "#2c2e35", swatchFg: "#f5f5f5" },
];

const COVER_TEMPLATES: { id: CoverTemplateId; name: string }[] = [
  { id: "impact", name: tk("暗幕金句") },
  { id: "marker", name: tk("荧光重点") },
  { id: "split", name: tk("上图下字") },
  { id: "bold", name: tk("大字报") },
  { id: "quote", name: tk("金句卡") },
  { id: "magazine", name: tk("杂志大字") },
  { id: "cinema", name: tk("电影画报") },
  { id: "minimal", name: tk("极简") },
];

const DOWNLOADS_DIR = /^.*\/(Downloads|下载)\//; // i18n-ignore
/** Cover typefaces: the style's own, system faces, and open-licence faces loaded when picked. */
const COVER_FONTS: [CoverFontId, string][] = [
  ["", tk("字体：跟随样式")],
  ["noto-sans", tk("思源黑体")],
  ["noto-serif", tk("思源宋体")],
  ["wenkai", tk("霞鹜文楷")],
  ["sans", tk("苹方 / 系统黑体")],
  ["serif", tk("系统宋体")],
];
/** Highlight colours for the cover toolbar ("" = each style's own). */
const ACCENTS = ["", "#ffd23f", "#ff5a36", "#22c55e", "#3b82f6", "#ec4899"];
/** Speeds a clip can be exported at (the preview plays at the chosen one). */
const SPEEDS = [1, 1.25, 1.5, 2];
const NO_CUTS: [number, number][] = [];

interface PreparedState {
  /** Which stretches it holds (the dialog's `rangesKey`). */
  key: string;
  path: string;
  /** Seconds before the clip in the file. */
  lead: number;
  start: number;
  end: number;
  duration: number;
  hasBurnedSubtitles: boolean;
  width: number;
  height: number;
  frames: string[];
  bestFrame: number;
  /** Pauses in the clip (clip time). */
  silences: [number, number][];
  /** Where the speaker sits over time (clip time, 0–1 across). */
  subjectTrack: [number, number][];
}

export function VideoClipDialog() {
  const target = useClipModal((s) => s.target);
  if (!target) return null;
  const key = `${target.ep.id}:${target.clip.id}:${target.clip.startIdx}:${target.clip.endIdx}`;
  return <VideoClipDialogBody key={key} target={target} />;
}

function VideoClipDialogBody({ target }: { target: ClipModalTarget }) {
  const uiLang = useLang((s) => s.lang);
  const zhTarget = uiLang === "zh-TW" ? "zh-TW" : "zh-CN";
  const { ep, segments, zh, clip } = target;
  const { notify, openSettings } = useStore(useShallow((s) => ({ notify: s.notify, openSettings: s.openSettings })));
  const liveZh = useJobs((s) => s.translations[ep.id]?.items);
  const initialForeign = useMemo(() => {
    const s = Math.max(0, Math.min(segments.length - 1, clip.startIdx));
    const e = Math.max(s, Math.min(segments.length - 1, clip.endIdx));
    for (let i = s; i <= e; i++) {
      const text = segments[i]?.text?.trim();
      if (text && !isZh(text)) return true;
    }
    return false;
  }, [segments, clip.startIdx, clip.endIdx]);
  const prefs = useMemo(readPrefs, []);
  // Episodes opened on their own belong to the internal "single" show, whose stored name is not a show name.
  const showDisplay = useMemo(() => (ep.showTitle && ep.showId !== "single" ? showName(ep.showTitle) : ""), [ep.showTitle, ep.showId]);

  const [sideTab, setSideTab] = useState<SideTab>("clip");
  const [pvView, setPvView] = useState<"video" | "cover">("video");

  const [startIdx, setStartIdx] = useState(() => Math.max(0, Math.min(segments.length - 1, clip.startIdx)));
  const [endIdx, setEndIdx] = useState(() => Math.max(startIdx, Math.min(segments.length - 1, clip.endIdx)));
  const [padBefore, setPadBefore] = useState(0.25);
  const [padAfter, setPadAfter] = useState(0.4);

  const [aspect, setAspect] = useState<ClipAspect>(() => prefs.aspect ?? fromOldLayout(prefs.layout).aspect);
  const [fit, setFit] = useState<ClipFit>(() => prefs.fit ?? fromOldLayout(prefs.layout).fit);
  const [brandText, setBrandText] = useState(() => prefs.brandText ?? "");
  const [watermark, setWatermark] = useState(() => prefs.watermark ?? true);
  const [cardBg, setCardBg] = useState<CardBg>(() => prefs.cardBg ?? "blur");
  const [cardCrop, setCardCrop] = useState<CardCrop>(() => prefs.cardCrop ?? "4:3");
  const [speed, setSpeed] = useState(() => prefs.speed ?? 1);
  useEffect(() => { if (videoRef.current) videoRef.current.playbackRate = speed; }, [speed]);
  const [subScale, setSubScale] = useState(() => prefs.subScale ?? 1);
  // Quick cover overrides (the toolbar under the cover preview); empty = the style's own.
  const [coverAccent, setCoverAccent] = useState(() => prefs.coverAccent ?? "");
  const [coverFont, setCoverFont] = useState<CoverFontId>(() => prefs.coverFont ?? "");
  const [coverScale, setCoverScale] = useState(() => prefs.coverScale ?? 1);
  const [coverTracking, setCoverTracking] = useState(() => prefs.coverTracking ?? 0);
  const [coverAdvanced, setCoverAdvanced] = useState(false);
  const coverStyle = { accent: coverAccent || undefined, font: coverFont || undefined, titleScale: coverScale, tracking: coverTracking };
  const [cropX, setCropXRaw] = useState(0.5);
  // Framing follows the speaker (found in the clip's best frame) until the user moves it.
  const cropTouched = useRef(false);
  const setCropX = (v: number) => { cropTouched.current = true; setCropXRaw(v); };
  const [subStyle, setSubStyle] = useState<SubStyleId>(() => prefs.subStyle ?? "cinema");
  const [subLang, setSubLang] = useState<SubContent>(() => (
    initialForeign || liveZh?.some((x) => !!x?.trim()) || zh?.some((x) => !!x?.trim())
      ? (prefs.subLang ?? "bilingual")
      : "original"
  ));
  const [showTitle, setShowTitle] = useState(() => prefs.showTitle ?? true);
  const [showSource, setShowSource] = useState(() => prefs.showSource ?? true);
  const [title, setTitle] = useState(() => clip.title);
  const [liftBurned, setLiftBurned] = useState(false);
  // Our subtitles: null follows the source (off when the picture already carries subtitles), else the user's pick.
  const [addSubsPick, setAddSubsPick] = useState<boolean | null>(null);

  // Cover & Publish state
  const [coverTpl, setCoverTpl] = useState<CoverTemplateId>(() => prefs.coverTpl ?? "impact");
  const [coverAspectPv, setCoverAspectPv] = useState<"auto" | CoverAspect>("auto");
  const [coverMode, setCoverMode] = useState<CoverMode>(() => prefs.coverMode ?? "flash");
  const [embedThumbnail, setEmbedThumbnail] = useState(() => prefs.embedThumbnail ?? true);
  const [saveCoverSidecar, setSaveCoverSidecar] = useState(() => prefs.saveCoverSidecar ?? true);
  const [extraLandscapeCover, setExtraLandscapeCover] = useState(() => prefs.extraLandscapeCover ?? true);
  const [removePauses, setRemovePauses] = useState(() => prefs.removePauses ?? false);
  const [loudness, setLoudness] = useState(() => prefs.loudness ?? true);
  const [smartFrame, setSmartFrame] = useState(() => prefs.smartFrame ?? true);

  const [coverHook, setCoverHook] = useState(() => clip.coverHook || clip.title);
  const [coverSub, setCoverSub] = useState(() => clip.coverSub || (clip.reason && clip.reason !== t("手动选取片段") ? clip.reason : ""));
  const [coverKicker, setCoverKicker] = useState(() => showDisplay || t("播客高光"));
  const [selectedFrame, setSelectedFrame] = useState<number | null>(null);
  const [coverPreviewUrl, setCoverPreviewUrl] = useState<string | null>(null);

  const [shareCopy, setShareCopy] = useState(() => clip.shareCopy);
  const [articleCopy, setArticleCopy] = useState(() => clip.articleCopy || "");
  const [postPlatform, setPostPlatform] = useState<PostPlatform>("short");
  const [postEdits, setPostEdits] = useState<Partial<Record<PostPlatform, string>>>({});
  const [polishing, setPolishing] = useState(false);

  const [zhEdits, setZhEdits] = useState<Record<number, string>>({});
  const [autoZh, setAutoZh] = useState<Record<number, string>>({});
  const [translatingClip, setTranslatingClip] = useState(false);
  const [origEdits, setOrigEdits] = useState<Record<number, string>>({});
  const [editingIdx, setEditingIdx] = useState<number | null>(null);
  const [activePreviewIdx, setActivePreviewIdx] = useState(startIdx);

  const [prepared, setPrepared] = useState<PreparedState | null>(null);
  const [busyJob, setBusyJob] = useState<{ id: string; stage: string; progress: number; message: string } | null>(null);
  const [exported, setExported] = useState<(RenderVideoClipResult & { elapsedSec?: number }) | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [needsSetup, setNeedsSetup] = useState(false);
  const [copiedFlag, setCopiedFlag] = useState<"post" | "cover" | null>(null);
  const copiedTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(copiedTimer.current), []);
  const flashCopied = (kind: "post" | "cover") => {
    setCopiedFlag(kind);
    window.clearTimeout(copiedTimer.current);
    copiedTimer.current = window.setTimeout(() => setCopiedFlag((c) => (c === kind ? null : c)), 1600);
  };

  const videoRef = useRef<HTMLVideoElement>(null);
  const linesRef = useRef<HTMLDivElement>(null);
  const [pvPlaying, setPvPlaying] = useState(false);
  // Plays on its own, with sound (it's what the clip sounds like).
  const [pvMuted, setPvMuted] = useState(false);
  const [soundBlocked, setSoundBlocked] = useState(false);
  // The clip's preview is the one making sound: the episode player pauses while the window is open.
  useEffect(() => { const st = useStore.getState(); if (st.playing) st.player?.toggle(); }, []);
  const [pvTime, setPvTime] = useState(0);

  useEffect(() => {
    savePrefs({
      aspect,
      fit,
      cardCrop,
      speed,
      subScale,
      coverAccent,
      coverFont,
      coverScale,
      coverTracking,
      brandText,
      watermark,
      cardBg,
      subStyle,
      subLang,
      showTitle,
      showSource,
      coverTpl,
      coverMode,
      embedThumbnail,
      saveCoverSidecar,
      extraLandscapeCover,
      removePauses,
      loudness,
      smartFrame,
    });
  }, [aspect, fit, cardCrop, speed, subScale, coverAccent, coverFont, coverScale, coverTracking, brandText, watermark, cardBg, subStyle, subLang, showTitle, showSource, coverTpl, coverMode, embedThumbnail, saveCoverSidecar, extraLandscapeCover, removePauses, loudness, smartFrame]);

  useEffect(() => {
    if (!busyJob) return;
    const un = listen<VideoClipProgress>("video-clip-progress", (e) => {
      if (e.payload.jobId !== busyJob.id) return;
      setBusyJob({ id: e.payload.jobId, stage: e.payload.stage, progress: e.payload.progress, message: e.payload.message });
    });
    return () => { void un.then((f) => f()); };
  }, [busyJob?.id]);

  const activeJobId = useRef<string | null>(null);
  activeJobId.current = busyJob?.id ?? null;
  useEffect(() => () => { if (activeJobId.current) void api.cancelJob(activeJobId.current); }, []);

  // The stretches this clip is cut from: one, or a montage's several (in order, their gaps left out).
  const montage = (clip.parts?.length ?? 0) > 1;
  const parts = useMemo(() => {
    const list = montage ? clip.parts! : [{ startIdx, endIdx }];
    let offset = 0;
    return list.map((p) => {
      const a = Math.max(0, (segments[p.startIdx]?.start ?? 0) - padBefore);
      const b = Math.max(a + 1, (segments[p.endIdx]?.end ?? a + 5) + padAfter);
      const part = { startIdx: p.startIdx, endIdx: p.endIdx, a, b, offset };
      offset += b - a;
      return part;
    });
  }, [montage, clip.parts, startIdx, endIdx, segments, padBefore, padAfter]);
  const clipStart = parts[0].a;
  const clipEnd = parts[parts.length - 1].b;
  const clipDur = parts.reduce((n, p) => n + (p.b - p.a), 0);
  const rangesKey = parts.map((p) => `${p.a.toFixed(2)}-${p.b.toFixed(2)}`).join("_");
  /** A moment of the source as time in the clip (a montage's later parts start after the earlier ones). */
  const toClipTime = (segIdx: number, at: number) => {
    const p = parts.find((x) => segIdx >= x.startIdx && segIdx <= x.endIdx) ?? parts[0];
    return Math.max(0, p.offset + at - p.a);
  };
  const clipLineIdx = useMemo(() => parts.flatMap((p) => Array.from({ length: p.endIdx - p.startIdx + 1 }, (_, k) => p.startIdx + k)), [parts]);

  const effectiveSegments = useMemo(
    () => segments.map((s, i) => (origEdits[i] !== undefined ? { ...s, text: origEdits[i] } : s)),
    [segments, origEdits],
  );
  const effectiveZh = useMemo(
    () => segments.map((_, i) => (zhEdits[i] !== undefined ? zhEdits[i] : liveZh?.[i] ?? zh?.[i] ?? autoZh[i] ?? null)),
    [segments, liveZh, zh, zhEdits, autoZh],
  );
  const isForeignClip = useMemo(
    () => clipLineIdx.some((i) => { const text = effectiveSegments[i]?.text?.trim(); return !!text && !isZh(text); }),
    [clipLineIdx, effectiveSegments],
  );
  const hasZh = useMemo(
    () => isForeignClip || clipLineIdx.some((i) => !!effectiveZh[i]?.trim()),
    [isForeignClip, clipLineIdx, effectiveZh],
  );

  // Automatically fetch Chinese translations for any untranslated English/foreign lines in this clip.
  useEffect(() => {
    const missing = clipLineIdx.filter((i) => {
      const orig = effectiveSegments[i]?.text?.trim();
      return !!orig && !isZh(orig) && !effectiveZh[i]?.trim();
    });
    if (!missing.length) return;
    let live = true;
    setTranslatingClip(true);
    const texts = missing.map((i) => effectiveSegments[i].text.trim());
    void api.translateTexts(texts, zhTarget).then((out) => {
      if (!live) return;
      setAutoZh((prev) => {
        const next = { ...prev };
        missing.forEach((idx, k) => { if (out[k]?.trim()) next[idx] = out[k].trim(); });
        return next;
      });
    }).catch(() => {}).finally(() => {
      if (live) setTranslatingClip(false);
    });
    return () => { live = false; };
  }, [clipLineIdx, zhTarget, effectiveSegments, liveZh, zh]); // eslint-disable-line react-hooks/exhaustive-deps

  // A filled frame crops the source's own subtitles off the sides, so ours go on there.
  // For foreign/English clips in card layout, our bilingual/Chinese subtitles sit cleanly under the video, so keep them on by default.
  const addSubs = addSubsPick ?? (isForeignClip && fit === "card" ? true : !(prepared?.hasBurnedSubtitles && fit === "card"));
  // Short subtitle lines: split here at once, then the model's punctuated breaks replace them when they arrive.
  // The picture's size is known once the stretch is fetched; YouTube is 16:9 until then.
  const geo = useMemo(() => clipGeometry(aspect, fit, prepared?.width || 1920, prepared?.height || 1080, cardCrop, addSubs), [aspect, fit, cardCrop, prepared?.width, prepared?.height, addSubs]);
  // "画面放法" only matters when the frame's shape differs from the picture's.
  const fitShown = aspect !== "source" && Math.abs(geo.W / geo.H - (prepared?.width || 1920) / (prepared?.height || 1080)) > 0.03;
  const activePreset = matchPreset({ aspect, fit, cardCrop, coverMode, removePauses }, fitShown, geo.fit === "card");
  const applyPreset = (p: ClipPreset) => {
    setAspect(p.values.aspect);
    setFit(p.values.fit);
    setCardCrop(p.values.cardCrop);
    setCoverMode(p.values.coverMode);
    setRemovePauses(p.values.removePauses);
    setPostPlatform(p.post);
  };
  useEffect(() => {
    const src = prepared?.frames[prepared.bestFrame];
    if (!src || cropTouched.current) return;
    const img = new Image();
    img.onload = () => { if (!cropTouched.current) setCropXRaw(subjectCenterX(img, prepared?.hasBurnedSubtitles ? 0.2 : 0)); };
    img.src = src;
  }, [prepared?.frames, prepared?.bestFrame]); // eslint-disable-line react-hooks/exhaustive-deps
  const maxCjk = geo.W > geo.H ? 18 : 14;
  const [lineBreaks, setLineBreaks] = useState<Record<number, string[]>>({});
  const [pairBreaks, setPairBreaks] = useState<Record<number, PairLines>>({});
  useEffect(() => { setLineBreaks({}); setPairBreaks({}); }, [maxCjk, subLang]);
  useEffect(() => {
    if (!addSubs) return;
    const ctl = new AbortController();
    // Bilingual cues split both languages together; one language alone splits only its CJK text.
    const pairs = subLang === "bilingual" ? parts.flatMap((p) => pairsToBreak(effectiveSegments, effectiveZh, p.startIdx, p.endIdx)).filter((x) => !pairBreaks[x.i]) : [];
    const texts = subLang === "bilingual" ? [] : parts.flatMap((p) => textsToBreak(effectiveSegments, effectiveZh, p.startIdx, p.endIdx, subLang)).filter((x) => !lineBreaks[x.i]);
    if (!pairs.length && !texts.length) return;
    const timer = window.setTimeout(() => {
      if (pairs.length) void aiPairBreaks(pairs, maxCjk, ctl.signal).then((r) => { if (!ctl.signal.aborted && Object.keys(r).length) setPairBreaks((p) => ({ ...p, ...r })); }).catch(() => {});
      if (texts.length) void aiLineBreaks(texts, maxCjk, ctl.signal).then((r) => { if (!ctl.signal.aborted && Object.keys(r).length) setLineBreaks((p) => ({ ...p, ...r })); }).catch(() => {});
    }, 400);
    return () => { window.clearTimeout(timer); ctl.abort(); };
  }, [addSubs, rangesKey, subLang, maxCjk, effectiveSegments, effectiveZh]); // eslint-disable-line react-hooks/exhaustive-deps
  // Every part's cues, moved to where that part plays in the clip.
  const buildAllCues = () => parts.flatMap((p) => buildClipCues(effectiveSegments, effectiveZh, p.startIdx, p.endIdx, p.a, p.b, subLang, lineBreaks, maxCjk, pairBreaks)
    .map((c) => ({ ...c, start: c.start + p.offset, end: c.end + p.offset })));
  const cues = useMemo(
    () => (addSubs ? buildAllCues() : []),
    [addSubs, effectiveSegments, effectiveZh, rangesKey, subLang, lineBreaks, pairBreaks, maxCjk], // eslint-disable-line react-hooks/exhaustive-deps
  );

  const visualConfig: ClipVisualConfig = useMemo(() => ({
    geo,
    cardBg,
    subStyle,
    subLang,
    showTitle,
    title,
    showSource,
    showName: showDisplay,
    episodeTitle: ep.title,
    brandText,
    watermark,
    liftForBurnedSub: liftBurned,
    subScale,
  }), [geo, cardBg, subStyle, subLang, showTitle, title, showSource, showDisplay, ep.title, brandText, watermark, liftBurned, subScale]);

  const staticOverlayUrl = useMemo(() => {
    const c = renderStaticOverlay(visualConfig);
    return c ? c.toDataURL("image/png") : null;
  }, [visualConfig]);

  const currentCue = useMemo(() => {
    if (!cues.length) return null;
    if (prepared?.key === rangesKey) {
      return cues.find((c) => pvTime >= c.start && pvTime <= c.end) ?? cues[0];
    }
    const seg = effectiveSegments[activePreviewIdx];
    const relMid = seg ? toClipTime(activePreviewIdx, (seg.start + seg.end) / 2) : 0;
    return cues.find((c) => relMid >= c.start && relMid <= c.end) ?? cues[0];
  }, [cues, prepared, rangesKey, pvTime, effectiveSegments, activePreviewIdx]); // eslint-disable-line react-hooks/exhaustive-deps

  const cueBandPreview = useMemo(() => {
    if (!currentCue) return null;
    const band = renderCueBand(currentCue, visualConfig);
    const height = geo.H;
    return {
      url: band.canvas.toDataURL("image/png"),
      topPct: (band.y / height) * 100,
    };
  }, [currentCue, visualConfig, geo.H]);

  const posterBackdrop = ep.cover || (ep.videoId ? `https://i.ytimg.com/vi/${ep.videoId}/hqdefault.jpg` : undefined);
  const preparedValid = !!prepared && prepared.key === rangesKey;
  // Smart framing: where a crop cuts the picture, it follows the speaker (found while fetching) instead of
  // staying where the slider put it.
  const cropping = (geo.fit !== "card" && aspect !== "source") || geo.cropBox;
  const track = preparedValid && prepared?.subjectTrack?.length ? prepared.subjectTrack : null;
  const cropBoxW = geo.fit === "card" ? geo.video.w : geo.W;
  const cropBoxH = geo.fit === "card" ? geo.video.h : geo.H;
  // Only where the crop has room to move (a 16:9 picture filling 16:9 has none).
  const canPan = cropping && panRoom(cropBoxW, cropBoxH, prepared?.width || 1920, prepared?.height || 1080) > 1;
  const following = smartFrame && canPan && !!track;
  const viewCropX = following && prepared ? centreShare(trackAt(track!, pvTime), cropBoxW, cropBoxH, prepared.width || 1920, prepared.height || 1080) : cropX;
  const preparedValidRef = useRef(preparedValid);
  preparedValidRef.current = preparedValid;
  // A new range is being fetched: the old clip holds still under the progress instead of playing on.
  useEffect(() => { if (!preparedValid) videoRef.current?.pause(); }, [preparedValid]);
  // Pauses to cut (raw clip time) and the clip's length once they're gone.
  const pauses = useMemo(
    () => (removePauses && preparedValid && prepared ? pauseCuts(prepared.silences, prepared.duration) : null),
    [removePauses, preparedValid, prepared],
  );
  const cutsNow = pauses?.cuts ?? NO_CUTS;
  const keptDur = pauses && prepared ? prepared.duration - pauses.saved : clipDur;
  const activeFrameIdx = selectedFrame != null && prepared?.frames[selectedFrame] ? selectedFrame : (prepared?.bestFrame ?? 0);
  const activeCoverBackdrop = (prepared?.frames[activeFrameIdx]) || posterBackdrop || null;
  const videoCoverAspect: CoverAspect = geo.W > geo.H ? "16:9" : aspect === "9:16" || (aspect === "source" && geo.H / geo.W > 1.5) ? "9:16" : aspect === "1:1" ? "1:1" : "3:4";
  const effectiveCoverAspect: CoverAspect = coverAspectPv === "auto" ? videoCoverAspect : coverAspectPv;

  useEffect(() => {
    let live = true;
    void renderCoverCanvas({
      template: coverTpl,
      aspect: effectiveCoverAspect,
      hookTitle: coverHook,
      subTitle: coverSub,
      kicker: coverKicker,
      showName: showDisplay,
      durationSec: keptDur / speed,
      backdropUrl: activeCoverBackdrop,
      brand: brandText, ...coverStyle, cropBottom: prepared?.hasBurnedSubtitles ? 0.2 : 0,
    }).then((c) => {
      if (live) setCoverPreviewUrl(c.toDataURL("image/png"));
    }).catch(() => {});
    return () => { live = false; };
  }, [coverTpl, effectiveCoverAspect, coverHook, coverSub, coverKicker, showDisplay, keptDur, speed, activeCoverBackdrop, brandText, coverAccent, coverFont, coverScale, coverTracking]);

  // Real previews of every style with this clip's words and picture (only while the cover tab is open).
  const [tplThumbs, setTplThumbs] = useState<Partial<Record<CoverTemplateId, string>>>({});
  useEffect(() => {
    if (sideTab !== "cover") return;
    let live = true;
    const timer = window.setTimeout(async () => {
      for (const tpl of COVER_TEMPLATES) {
        const c = await renderCoverCanvas({ template: tpl.id, aspect: "3:4", hookTitle: coverHook, subTitle: "", kicker: coverKicker, showName: showDisplay, durationSec: 0, backdropUrl: activeCoverBackdrop, brand: brandText, ...coverStyle, cropBottom: prepared?.hasBurnedSubtitles ? 0.2 : 0 });
        if (!live) return;
        const small = document.createElement("canvas");
        small.width = 180; small.height = 240;
        small.getContext("2d")!.drawImage(c, 0, 0, 180, 240);
        const url = small.toDataURL("image/jpeg", 0.82);
        setTplThumbs((p) => ({ ...p, [tpl.id]: url }));
      }
    }, 350);
    return () => { live = false; window.clearTimeout(timer); };
  }, [sideTab, coverHook, coverKicker, showDisplay, activeCoverBackdrop, brandText, coverAccent, coverFont, coverScale, coverTracking]);

  const generatedPost = useMemo(
    () => buildPlatformPost(postPlatform, {
      ep,
      title,
      coverHook,
      coverSub,
      shareCopy,
      articleCopy,
      tags: clip.tags,
      segments: effectiveSegments,
      zh: effectiveZh,
      startIdx,
      endIdx,
      clipStart,
    }),
    [postPlatform, ep, title, coverHook, coverSub, shareCopy, articleCopy, clip.tags, effectiveSegments, effectiveZh, startIdx, endIdx, clipStart],
  );
  const currentPostText = postEdits[postPlatform] ?? generatedPost;

  const switchSideTab = (tab: SideTab) => {
    setSideTab(tab);
    if (tab === "cover") setPvView("cover");
    else if (tab === "clip") setPvView("video");
  };

  const ensurePrepared = async (): Promise<PreparedState | null> => {
    if (preparedValid && prepared) return prepared;
    if (!ep.videoId) { setError(t("当前单集没有可用视频源")); return null; }
    const jobId = `vclip-prep-${Date.now()}`;
    setError(null);
    setNeedsSetup(false);
    setBusyJob({ id: jobId, stage: "download", progress: 0.06, message: t("正在获取这一段原视频…") });
    try {
      const res = await api.videoClipPrepare(jobId, ep.videoId, clipStart, clipEnd, montage ? parts.map((p) => [p.a, p.b] as [number, number]) : undefined);
      const next: PreparedState = {
        key: rangesKey,
        path: res.path,
        lead: res.lead ?? 0,
        start: clipStart,
        end: clipEnd,
        duration: res.duration || clipDur,
        hasBurnedSubtitles: res.hasBurnedSubtitles,
        width: res.width,
        height: res.height,
        frames: res.frames ?? [],
        bestFrame: res.bestFrame ?? 0,
        silences: res.silences ?? [],
        subjectTrack: res.subjectTrack ?? [],
      };
      setPrepared(next);
      if (selectedFrame == null) setSelectedFrame(next.bestFrame);
      if (res.hasBurnedSubtitles) setLiftBurned(true);
      return next;
    } catch (e) {
      if (!rawErrorText(e).includes("已取消")) setError(errorText(e)); // i18n-ignore
      return null;
    } finally {
      setBusyJob(null);
    }
  };  // The stretch starts downloading as soon as the dialog opens (and after the range changes): preview, cover
  // frames and export are ready by the time they're asked for.
  const autoPrepared = useRef("");
  useEffect(() => {
    if (!ep.videoId || preparedValid || busyJob || autoPrepared.current === rangesKey) return;
    const timer = window.setTimeout(() => { autoPrepared.current = rangesKey; void ensurePrepared(); }, prepared ? 900 : 150);
    return () => window.clearTimeout(timer);
  }, [rangesKey, preparedValid, !!busyJob]); // eslint-disable-line react-hooks/exhaustive-deps


  /** `fill`: only the copy that's missing (opening a highlight that came without it), quietly.
   *  `again`: a new take on the post (title and text) that differs from the one shown; the cover stays. */
  const runAiPolish = async (fill = false, again = false) => {
    if (polishing) return;
    setPolishing(true);
    setError(null);
    setNeedsSetup(false);
    try {
      const res = await polishClipCopy({ ep, segments: effectiveSegments, zh: effectiveZh, startIdx, endIdx, ranges: montage ? parts : undefined, previous: again ? currentPostText : undefined });
      // A clip that came without title or cover lines (a montage, a hand-picked stretch) takes the model's.
      if (again) setTitle(res.title);
      else if (!fill || !clip.coverHook) {
        setTitle(res.title);
        setCoverHook(res.coverHook);
        setCoverSub(res.coverSub);
      }
      setShareCopy(res.shareCopy);
      setArticleCopy(res.articleCopy);
      setPostEdits({});
      if (fill) void saveClipCopy(ep.id, clip.id, { shareCopy: res.shareCopy, articleCopy: res.articleCopy });
      if (!fill) notify(t("AI 已生成封面金句与多平台文案"));
    } catch (e) {
      setNeedsSetup(Boolean((e as { setup?: boolean })?.setup) || /设置|API Key|模型/.test(rawErrorText(e)));
      setError(errorText(e));
    } finally {
      setPolishing(false);
    }
  };  // A highlight picked by the model arrives with its title and cover lines only; the posts are written on open.
  const autoFilled = useRef(false);
  useEffect(() => {
    if (autoFilled.current || clip.articleCopy) return;
    autoFilled.current = true;
    void llmReady("chat").then((ok) => { if (ok) void runAiPolish(true); });
  }, []); // eslint-disable-line react-hooks/exhaustive-deps


  const copyCoverImage = async (aspectOverride?: CoverAspect) => {
    try {
      const c = await renderCoverCanvas({
        template: coverTpl,
        aspect: aspectOverride ?? effectiveCoverAspect,
        hookTitle: coverHook,
        subTitle: coverSub,
        kicker: coverKicker,
        showName: showDisplay,
        durationSec: keptDur / speed,
        backdropUrl: activeCoverBackdrop,
      brand: brandText, ...coverStyle, cropBottom: prepared?.hasBurnedSubtitles ? 0.2 : 0,
      });
      const blob = await new Promise<Blob | null>((r) => c.toBlob(r, "image/png"));
      if (!blob) throw new Error("blob");
      await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
      flashCopied("cover");
      notify(t("封面图已复制到剪贴板"));
    } catch {
      notify(t("复制失败，请用「保存封面」"));
    }
  };

  const saveCoverImage = async (aspectOverride?: CoverAspect) => {
    try {
      const targetAspect = aspectOverride ?? effectiveCoverAspect;
      const c = await renderCoverCanvas({
        template: coverTpl,
        aspect: targetAspect,
        hookTitle: coverHook,
        subTitle: coverSub,
        kicker: coverKicker,
        showName: showDisplay,
        durationSec: keptDur / speed,
        backdropUrl: activeCoverBackdrop,
      brand: brandText, ...coverStyle, cropBottom: prepared?.hasBurnedSubtitles ? 0.2 : 0,
      });
      const safeTitle = (title.trim() || ep.title).replace(/[\\/:*?"<>|\u0000-\u001f]/g, " ").trim().slice(0, 50);
      const name = `${safeTitle || t("SubCast 切片")}-${t("封面")}-${targetAspect.replace(":", "x")}`;
      const p = await api.saveImage(name, c.toDataURL("image/png"));
      notify(t("封面已保存到 {path}", { path: p.replace(DOWNLOADS_DIR, tx("folder", "下载") + "/") }));
    } catch (e) {
      notify(errorText(e));
    }
  };

  /** Exports the clip as set up now; true when a file was written (batch export goes on from there). */
  const runExport = async (askPath: boolean): Promise<boolean> => {
    if (busyJob) return false;
    setExported(null);
    setError(null);
    setNeedsSetup(false);

    let customPath: string | null = null;
    const safeTitle = (title.trim() || ep.title).replace(/[\\/:*?"<>|\u0000-\u001f]/g, " ").trim().slice(0, 60);
    const defaultName = `${safeTitle || t("SubCast 切片")} (${clock(clipStart).replace(/:/g, "-")})`;
    if (askPath) {
      customPath = await saveDialog({
        defaultPath: `${defaultName}.mp4`,
        filters: [{ name: "MP4", extensions: ["mp4"] }],
      }).catch(() => null);
      if (!customPath) return false;
    }

    const prep = await ensurePrepared();
    if (!prep) return false;

    const jobId = `vclip-render-${Date.now()}`;
    const startedAt = performance.now();
    setBusyJob({ id: jobId, stage: "render", progress: 0.03, message: t("正在生成封面和画面图层…") });
    try {
      const subsOn = addSubsPick ?? (isForeignClip && fit === "card" ? true : !(prep.hasBurnedSubtitles && fit === "card"));
      // Guarantee English/foreign highlight clips always have Chinese translations ready before burning subtitles.
      let exportZh = effectiveZh;
      if (subsOn && subLang !== "original") {
        const missing = clipLineIdx.filter((i) => {
          const orig = effectiveSegments[i]?.text?.trim();
          return !!orig && !isZh(orig) && !exportZh[i]?.trim();
        });
        if (missing.length) {
          setBusyJob({ id: jobId, stage: "render", progress: 0.04, message: t("正在补充中文字幕翻译…") });
          try {
            const out = await api.translateTexts(missing.map((i) => effectiveSegments[i].text.trim()), zhTarget);
            const nextZh = [...exportZh];
            const patch: Record<number, string> = {};
            missing.forEach((idx, k) => {
              const translated = out[k]?.trim();
              if (translated) { nextZh[idx] = translated; patch[idx] = translated; }
            });
            exportZh = nextZh;
            if (Object.keys(patch).length) setAutoZh((prev) => ({ ...prev, ...patch }));
          } catch { /* fall back to whatever is available */ }
        }
      }
      const staticCanvas = renderStaticOverlay({ ...visualConfig, geo: clipGeometry(aspect, fit, prep.width || 1920, prep.height || 1080, cardCrop, subsOn) });
      const staticOverlayPng = staticCanvas ? canvasToPngBase64(staticCanvas) : null;
      // Decided again from what preparing found: an export started before any preview still skips doubled subtitles.
      const exportCues = subsOn
        ? parts.flatMap((p) => buildClipCues(effectiveSegments, exportZh, p.startIdx, p.endIdx, p.a, p.b, subLang, lineBreaks, maxCjk, pairBreaks)
            .map((c) => ({ ...c, start: c.start + p.offset, end: c.end + p.offset })))
        : [];
      // The fetched picture's real size decides the layout (an export can start before any preview).
      const exportConfig: ClipVisualConfig = { ...visualConfig, geo: clipGeometry(aspect, fit, prep.width || 1920, prep.height || 1080, cardCrop, subsOn) };
      const eg = exportConfig.geo;
      // Pauses cut: the clip's stretches to keep, and every cue moved onto the cut clock (cues inside a cut drop).
      const exportPauses = removePauses ? pauseCuts(prep.silences, prep.duration) : null;
      const xc = exportPauses?.cuts ?? [];
      const exportDur = prep.duration - (exportPauses?.saved ?? 0);
      const renderedCues: { start: number; end: number; y: number; pngBase64: string }[] = [];
      for (let i = 0; i < exportCues.length; i++) {
        const cue = exportCues[i];
        const cs = toKeptTime(cue.start, xc);
        const ce = toKeptTime(cue.end, xc);
        if (ce - cs < 0.05) continue;
        const band = renderCueBand(cue, exportConfig);
        renderedCues.push({ start: cs / speed, end: ce / speed, y: band.y, pngBase64: canvasToPngBase64(band.canvas) });
        if ((i + 1) % 6 === 0 && i + 1 < exportCues.length) {
          setBusyJob({ id: jobId, stage: "render", progress: 0.04 + ((i + 1) / exportCues.length) * 0.07, message: t("正在生成封面和画面图层…") });
          await new Promise((r) => window.setTimeout(r, 0));
        }
      }

      const bestIdx = selectedFrame != null && prep.frames[selectedFrame] ? selectedFrame : prep.bestFrame;
      const backdropForExport = prep.frames[bestIdx] || posterBackdrop || null;
      const mainAspect: CoverAspect = videoCoverAspect;
      const needMainCover = coverMode !== "none" || embedThumbnail || saveCoverSidecar;

      let coverPng: string | null = null;
      if (needMainCover) {
        const coverCanvas = await renderCoverCanvas({
          template: coverTpl,
          aspect: mainAspect,
          hookTitle: coverHook,
          subTitle: coverSub,
          kicker: coverKicker,
          showName: showDisplay,
          durationSec: exportDur / speed,
          backdropUrl: backdropForExport,
          brand: brandText, ...coverStyle, cropBottom: prep.hasBurnedSubtitles ? 0.2 : 0,
        });
        coverPng = canvasToPngBase64(coverCanvas);
      }

      let landscapeCoverPng: string | null = null;
      if (mainAspect !== "16:9" && saveCoverSidecar && extraLandscapeCover) {
        const landCanvas = await renderCoverCanvas({
          template: coverTpl,
          aspect: "16:9",
          hookTitle: coverHook,
          subTitle: coverSub,
          kicker: coverKicker,
          showName: showDisplay,
          durationSec: exportDur / speed,
          backdropUrl: backdropForExport,
          brand: brandText, ...coverStyle, cropBottom: prep.hasBurnedSubtitles ? 0.2 : 0,
        });
        landscapeCoverPng = canvasToPngBase64(landCanvas);
      }

      const expCropping = ((eg.fit !== "card" && aspect !== "source") || eg.cropBox)
        && panRoom(eg.fit === "card" ? eg.video.w : eg.W, eg.fit === "card" ? eg.video.h : eg.H, prep.width || 1920, prep.height || 1080) > 1;
      const expTrack = smartFrame && expCropping && prep.subjectTrack?.length ? prep.subjectTrack : null;
      const expBoxW = eg.fit === "card" ? eg.video.w : eg.W;
      const expBoxH = eg.fit === "card" ? eg.video.h : eg.H;
      const result = await api.videoClipRender(jobId, {
        rawPath: prep.path,
        outputPath: customPath,
        outputDir: useStore.getState().settings.clipExportDir || null,
        defaultName,
        layout: "frame",
        frame: { w: eg.W, h: eg.H, fit: eg.fit, vx: eg.video.x, vy: eg.video.y, vw: eg.video.w, vh: eg.video.h, cropBox: eg.cropBox },
        cardBg,
        cropX: expTrack?.length === 1 ? centreShare(expTrack[0][1], expBoxW, expBoxH, prep.width || 1920, prep.height || 1080) : cropX,
        pan: expTrack && expTrack.length > 1 ? expTrack : undefined,
        duration: prep.duration,
        lead: prep.lead,
        speed,
        staticOverlayPng,
        cues: renderedCues,
        coverPng,
        coverMode,
        embedThumbnail,
        saveCoverSidecar,
        landscapeCoverPng,
        keepRanges: exportPauses?.cuts.length ? exportPauses.keep : undefined,
        loudness,
      });
      const elapsedSec = Math.max(0.1, (performance.now() - startedAt) / 1000);
      setExported({ ...result, elapsedSec });
      noteExport({
        path: result.videoPath, episodeId: ep.id, episodeTitle: ep.title,
        title: title.trim() || (segments[startIdx]?.text ?? "").slice(0, 40),
        duration: exportDur / speed + (coverMode === "hold" ? 1 : 0), aspect, at: Date.now(),
      });
      notify(t("视频切片已导出到 {path}", { path: result.videoPath.replace(DOWNLOADS_DIR, tx("folder", "下载") + "/") }), {
        label: t("打开文件夹"),
        run: () => void api.revealFile(result.videoPath).catch(() => {}),
      });
      return true;
    } catch (e) {
      if (!rawErrorText(e).includes("已取消")) setError(errorText(e)); // i18n-ignore
      return false;
    } finally {
      setBusyJob(null);
    }
  };

  const copyCurrentPost = async () => {
    try {
      await navigator.clipboard.writeText(currentPostText);
      flashCopied("post");
      notify(t("发布文案已复制"));
    } catch {
      notify(t("复制失败"));
    }
  };

  const togglePreviewPlay = useCallback(() => {
    const v = videoRef.current;
    if (!v) return;
    if (v.paused) void v.play().catch(() => {});
    else v.pause();
  }, []);

  // Removing pauses: the preview jumps over each one as it plays (checked every frame, not at timeupdate's ~4 Hz).
  useEffect(() => {
    if (!pvPlaying || !cutsNow.length || !prepared) return;
    let raf = 0;
    const tick = () => {
      const v = videoRef.current;
      if (v) {
        const cut = cutAt(v.currentTime - prepared.lead, cutsNow);
        if (cut) v.currentTime = prepared.lead + cut[1];
      }
      raf = requestAnimationFrame(tick);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  }, [pvPlaying, cutsNow, prepared]);

  // Sync active transcript line highlight while preview video plays.
  useEffect(() => {
    if (!preparedValid || !pvPlaying) return;
    let match = clipLineIdx[0] ?? startIdx;
    for (const idx of clipLineIdx) {
      const seg = effectiveSegments[idx];
      if (!seg) continue;
      const s = toClipTime(idx, seg.start);
      const e = toClipTime(idx, seg.end);
      if (pvTime >= s - 0.05) match = idx;
      if (pvTime <= e) break;
    }
    setActivePreviewIdx((cur) => {
      if (cur === match) return cur;
      const el = linesRef.current?.querySelector<HTMLElement>(`[data-vclip-i="${match}"]`);
      el?.scrollIntoView({ block: "nearest", behavior: "smooth" });
      return match;
    });
  }, [preparedValid, pvPlaying, pvTime, clipLineIdx, startIdx, effectiveSegments]); // eslint-disable-line react-hooks/exhaustive-deps

  const requestClose = useCallback(() => {
    if (busyJob?.stage === "render") {
      void ask({
        title: t("正在导出视频切片，确定取消并关闭？"),
        message: t("关闭后当前导出任务会立即中止。"),
        ok: t("取消导出"),
        danger: true,
      }).then((yes) => {
        if (!yes) return;
        if (activeJobId.current) void api.cancelJob(activeJobId.current);
        closeVideoClipDialog();
      });
      return;
    }
    closeVideoClipDialog();
  }, [busyJob?.stage]);

  // Modal-scoped keyboard shortcuts: Space play/pause, ←/→ seek 2s, ⌘/Ctrl+Enter export.
  const runExportRef = useRef(runExport);
  runExportRef.current = runExport;

  // Batch export: each highlight in turn gets its range and titles, then the same export as the button.
  const batch = target.batch && target.batch.length > 1 ? target.batch : null;
  const [run, setRun] = useState<{ i: number; ok: number; step: "set" | "export" } | null>(null);
  const stopBatch = useRef(false);
  const [stopping, setStopping] = useState(false);
  useEffect(() => {
    if (!run || !batch) return;
    if (run.step === "set") {
      const c = batch[run.i];
      setStartIdx(Math.max(0, Math.min(segments.length - 1, c.startIdx)));
      setEndIdx(Math.max(c.startIdx, Math.min(segments.length - 1, c.endIdx)));
      setTitle(c.title);
      setCoverHook(c.coverHook || c.title);
      setCoverSub(c.coverSub || "");
      setRun({ ...run, step: "export" });
      return;
    }
    // A render later, so the export reads this clip's range and titles.
    let live = true;
    void runExportRef.current(false).then((ok) => {
      if (!live) return;
      const done = { ...run, ok: run.ok + (ok ? 1 : 0) };
      if (stopBatch.current || run.i + 1 >= batch.length) {
        setRun(null);
        stopBatch.current = false;
        setStopping(false);
        notify(t("批量导出完成：{ok}/{n} 段", { ok: done.ok, n: batch.length }));
        return;
      }
      setRun({ i: run.i + 1, ok: done.ok, step: "set" });
    });
    return () => { live = false; };
  }, [run?.i, run?.step]); // eslint-disable-line react-hooks/exhaustive-deps
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") {
        e.preventDefault();
        if (!busyJob) void runExportRef.current(false);
        return;
      }
      const typing = (e.target as HTMLElement | null)?.closest?.("input, textarea, select, [contenteditable]");
      if (typing) return;
      if (e.code === "Space" && pvView === "video" && preparedValid) {
        e.preventDefault();
        togglePreviewPlay();
      } else if ((e.key === "ArrowLeft" || e.key === "ArrowRight") && pvView === "video" && preparedValid && prepared && videoRef.current) {
        e.preventDefault();
        const delta = (e.key === "ArrowLeft" ? -2 : 2) * speed;
        const nextAt = Math.max(0, Math.min(prepared.duration, (videoRef.current.currentTime - prepared.lead) + delta));
        videoRef.current.currentTime = prepared.lead + nextAt;
        setPvTime(nextAt);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busyJob, pvView, preparedValid, prepared, speed, togglePreviewPlay]);

  const card = geo.fit === "card";
  const stageBgClass = card ? (cardBg === "paper" ? "bg-paper" : cardBg === "night" ? "bg-night" : "bg-blur") : "bg-dark";
  // The preview places the picture with the same geometry the export uses.
  // A definite size (fitted into 440 × 560): absolutely placed layers give the box no size of their own.
  // The preview takes the height the column has left after its tabs, range bar and controls, so they never
  // spill into the footer on a short window.
  const pvColRef = useRef<HTMLDivElement>(null);
  const [pvRoom, setPvRoom] = useState(520);
  const [pvRoomW, setPvRoomW] = useState(340);
  useEffect(() => {
    const col = pvColRef.current;
    if (!col) return;
    const measure = () => {
      const kids = Array.from(col.children) as HTMLElement[];
      const gap = parseFloat(getComputedStyle(col).rowGap) || 0;
      const others = kids.filter((k) => !k.classList.contains("vclip-stage")).reduce((n, k) => n + k.offsetHeight, 0);
      const room = Math.floor(col.clientHeight - others - gap * Math.max(0, kids.length - 1));
      setPvRoom(Math.max(180, room));
      setPvRoomW(Math.max(200, col.clientWidth));
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(col);
    for (const k of Array.from(col.children)) if (!k.classList.contains("vclip-stage")) ro.observe(k);
    // Hints and bars come and go below the preview: measure again when they do.
    const mo = new MutationObserver(measure);
    mo.observe(col, { childList: true });
    return () => { ro.disconnect(); mo.disconnect(); };
  }, [pvView, montage, segments.length > 1, preparedValid, geo.W > geo.H, startIdx]);
  const pvScale = Math.min(Math.min(440, pvRoomW) / geo.W, Math.min(520, pvRoom) / geo.H);
  const stageStyle: React.CSSProperties = { width: Math.round(geo.W * pvScale), height: Math.round(geo.H * pvScale), flex: "none" };
  const boxStyle: React.CSSProperties = {
    left: `${(geo.video.x / geo.W) * 100}%`, top: `${(geo.video.y / geo.H) * 100}%`,
    width: `${(geo.video.w / geo.W) * 100}%`, height: `${(geo.video.h / geo.H) * 100}%`,
  };
  const mediaStyle: React.CSSProperties | undefined = card && !geo.cropBox ? { objectFit: "contain" } : { objectFit: "cover", objectPosition: `${viewCropX * 100}% center` };
  // An iframe can't object-fit: size it to cover (or fit) the box and pan it like the export.
  const srcRatio = (prepared?.width || 1920) / (prepared?.height || 1080);
  const boxRatio = geo.video.w / geo.video.h;
  const coverIt = !(card && !geo.cropBox);
  const ytFrameBase: React.CSSProperties = coverIt && srcRatio > boxRatio
    ? { width: `${(srcRatio / boxRatio) * 100}%`, height: "100%", left: `${(1 - srcRatio / boxRatio) * viewCropX * 100}%`, top: 0 }
    : coverIt
      ? { width: "100%", height: `${(boxRatio / srcRatio) * 100}%`, top: `${((1 - boxRatio / srcRatio) / 2) * 100}%`, left: 0 }
      : srcRatio > boxRatio
        ? { width: "100%", height: `${(boxRatio / srcRatio) * 100}%`, top: `${((1 - boxRatio / srcRatio) / 2) * 100}%`, left: 0 }
        : { height: "100%", width: `${(srcRatio / boxRatio) * 100}%`, left: `${((1 - srcRatio / boxRatio) / 2) * 100}%`, top: 0 };
  // Scaled up so YouTube's own title bar and controls fall outside the box (a stand-in until the cut is ready).
  // The YouTube stand-in stays hidden until it really plays, and goes away on an error (embedding blocked, no
  // referrer in a debug build...): the poster shows instead of YouTube's error page.
  const ytRef = useRef<HTMLIFrameElement>(null);
  const [ytShown, setYtShown] = useState(false);
  const [ytFailed, setYtFailed] = useState(false);
  useEffect(() => { setYtShown(false); setYtFailed(false); }, [ep.videoId, clipStart, clipEnd]);
  useEffect(() => {
    const onMsg = (e: MessageEvent) => {
      if (!/youtube(-nocookie)?\.com$/.test(new URL(e.origin || "http://x").hostname)) return;
      if (e.source !== ytRef.current?.contentWindow) return;
      let m: { event?: string; info?: unknown } | null = null;
      try { m = typeof e.data === "string" ? JSON.parse(e.data) : e.data; } catch { return; }
      if (!m) return;
      if (m.event === "onError") setYtFailed(true);
      const state = m.event === "onStateChange" ? m.info : m.event === "infoDelivery" ? (m.info as { playerState?: number } | null)?.playerState : undefined;
      if (state === 1) setYtShown(true);
    };
    window.addEventListener("message", onMsg);
    return () => window.removeEventListener("message", onMsg);
  }, []);
  const ytFrameStyle: React.CSSProperties = { ...ytFrameBase, transform: "scale(1.35)", transformOrigin: "center" };
  const preparing = !!busyJob && ["download", "ffmpeg", "trim"].includes(busyJob.stage);

  return (
    <Modal title={t("剪成视频")} onClose={requestClose} wide>
      {/* Square and landscape clips get half the dialog so the preview isn't a postage stamp; portrait keeps a slim column. */}
      {batch && (
        <p className="vclip-batch-note small">
          {run ? t("正在依次导出，第 {i}/{n} 段：{title}", { i: run.i + 1, n: batch.length, title: batch[run.i].title })
            : t("批量导出 {n} 段：用第一段把比例、字幕和封面设好，每段会用同样的设置、自己的标题导出。", { n: batch.length })}
        </p>
      )}
      <div className={`vclip-layout${geo.W >= geo.H ? " wide-pv" : ""}`}>
        {/* Left: live WYSIWYG video / cover preview */}
        <div className="vclip-preview-col" ref={pvColRef}>
          <div className="vclip-pv-switch">
            <Seg
              value={pvView}
              options={[["video", t("正片预览")], ["cover", t("封面预览")]]}
              onChange={setPvView}
            />
            {pvView === "cover" && (
              <Seg
                value={coverAspectPv}
                options={[["auto", t("随视频")], ["9:16", "9:16"], ["3:4", "3:4"], ["1:1", "1:1"], ["16:9", "16:9"]]}
                onChange={setCoverAspectPv}
              />
            )}
          </div>

          {pvView === "cover" ? (
            <div className={`vclip-cover-stage aspect-${effectiveCoverAspect.replace(":", "-")}`}>
              {coverPreviewUrl
                ? <img src={coverPreviewUrl} alt={t("封面预览")} className="vclip-cover-img" />
                : <div className="poster-wait"><Loader2 size={16} className="spin" /> {t("正在生成封面…")}</div>}
            </div>
          ) : (
            <div className={`vclip-stage ${geo.W > geo.H ? "is-land" : "is-port"} ${stageBgClass}`} style={stageStyle}>
              {card && cardBg === "blur" && posterBackdrop && (
                <img src={posterBackdrop} alt="" className="vclip-blur-bg" />
              )}
              <div className="vclip-video-box" style={boxStyle}>
                {prepared ? (
                  <video
                    ref={videoRef}
                    className={preparedValid ? undefined : "stale"}
                    src={convertFileSrc(prepared.path)}
                    autoPlay
                    playsInline
                    muted={pvMuted}
                    onLoadedMetadata={(e) => {
                      const v = e.currentTarget;
                      if (!preparedValidRef.current) { v.pause(); return; }
                      v.currentTime = prepared.lead;
                      v.playbackRate = speed;
                      // With sound; where the system won't autoplay sound, it plays muted and says so.
                      v.muted = pvMuted;
                      v.volume = 1;
                      void v.play().catch(() => { v.muted = true; setPvMuted(true); setSoundBlocked(true); void v.play().catch(() => {}); });
                    }}
                    style={mediaStyle}
                    onTimeUpdate={(e) => {
                      // The file starts `lead` early (a stream copy); the clip loops within its own span.
                      const v = e.currentTarget;
                      const at = v.currentTime - prepared.lead;
                      if (at >= prepared.duration - 0.05 || v.ended) { v.currentTime = prepared.lead; void v.play().catch(() => {}); }
                      setPvTime(Math.max(0, at));
                    }}
                    onEnded={(e) => { e.currentTarget.currentTime = prepared.lead; void e.currentTarget.play().catch(() => {}); }}
                    onPlay={() => {
                      setPvPlaying(true);
                      const s = useStore.getState();
                      if (s.playing) s.player?.toggle();
                    }}
                    onPause={() => setPvPlaying(false)}
                    onClick={preparedValid ? togglePreviewPlay : undefined}
                  />
                ) : (
                  <div className="vclip-placeholder">
                    {/* While the stretch downloads, YouTube plays it in place (muted), framed like the export. */}
                    {posterBackdrop && <img src={posterBackdrop} alt="" style={mediaStyle} />}
                    {ep.videoId && !montage && !ytFailed && (
                      <iframe
                        ref={ytRef}
                        className={`vclip-yt${ytShown ? " on" : ""}`}
                        title={t("预览")}
                        style={ytFrameStyle}
                        onLoad={() => ytRef.current?.contentWindow?.postMessage(JSON.stringify({ event: "listening", id: 1, channel: "widget" }), "*")}
                        allow="autoplay; encrypted-media"
                        src={`https://www.youtube.com/embed/${ep.videoId}?start=${Math.floor(clipStart)}&end=${Math.ceil(clipEnd)}&autoplay=1&mute=1&controls=0&playsinline=1&rel=0&modestbranding=1&loop=1&playlist=${ep.videoId}&disablekb=1&iv_load_policy=3&cc_load_policy=0&enablejsapi=1&origin=${encodeURIComponent(location.origin)}`}
                      />
                    )}
                  </div>
                )}
              </div>
              {staticOverlayUrl && <img src={staticOverlayUrl} alt="" className="vclip-overlay-img" />}
              {busyJob && busyJob.stage !== "render" && (
                <div className="vclip-pv-progress" role="status">
                  <span>{preparing ? t("正在准备高清画面") : tr(busyJob.message)}</span>
                  <span className="num">{Math.round(busyJob.progress * 100)}%</span>
                  <i style={{ width: `${Math.max(3, busyJob.progress * 100)}%` }} />
                </div>
              )}
              {cueBandPreview && (
                <img src={cueBandPreview.url} alt="" className="vclip-cue-img" style={{ top: `${cueBandPreview.topPct}%` }} />
              )}
            </div>
          )}

          {pvView === "video" && preparedValid && prepared && (
            <div className="vclip-scrub">
              <input
                type="range"
                className="vclip-seek"
                min={0}
                max={Math.max(0.1, prepared.duration || clipDur)}
                step={0.05}
                value={Math.min(pvTime, prepared.duration || clipDur)}
                aria-label={t("预览进度")}
                onChange={(e) => {
                  const next = Number(e.target.value);
                  setPvTime(next);
                  if (videoRef.current) videoRef.current.currentTime = prepared.lead + next;
                }}
              />
            </div>
          )}

          {pvView === "video" && !montage && segments.length > 1 && (
            <ClipRangeBar segments={effectiveSegments} zh={effectiveZh} startIdx={startIdx} endIdx={endIdx} onChange={(a, b) => { setStartIdx(a); setEndIdx(b); }} />
          )}
          {pvView === "video" && !montage && <OpeningHint segments={segments} startIdx={startIdx} endIdx={endIdx} onStart={setStartIdx} />}

          <div className="vclip-pv-bar">
            {pvView === "cover" ? (
              <span className="small muted">{t("封面预览")} · {videoCoverAspect}</span>
            ) : preparedValid ? (
              <>
                <button type="button" className="icon-btn sm" onClick={togglePreviewPlay} title={pvPlaying ? t("暂停") : t("播放")}>
                  {pvPlaying ? <Pause size={14} fill="currentColor" /> : <Play size={14} fill="currentColor" />}
                </button>
                <button type="button" className={`icon-btn sm${soundBlocked && pvMuted ? " vclip-unmute" : ""}`} onClick={() => { setSoundBlocked(false); setPvMuted(!pvMuted); if (videoRef.current) { videoRef.current.muted = !pvMuted; void videoRef.current.play().catch(() => {}); } }} title={pvMuted ? t("取消静音") : t("静音")}>
                  {pvMuted ? <VolumeX size={14} /> : <Volume2 size={14} />}{soundBlocked && pvMuted && <span>{t("点这里开声音")}</span>}
                </button>
                <span className="num small muted nowrap">{clock(toKeptTime(pvTime, cutsNow) / speed)} / {clock(keptDur / speed)}</span>
                {speed !== 1 && <span className="vclip-speed-badge num" title={t("当前成片倍速，可在右侧「视频」栏调整")}>{speed}×</span>}
              </>
            ) : pvView === "video" && !montage && segments.length > 1 ? null : (
              <span className="small muted">{montage ? t("{n} 段拼接", { n: parts.length }) : `${clock(clipStart)} – ${clock(clipEnd)}`} · {Math.round(clipDur)}s</span>
            )}
            <span className="spacer" />

          </div>

        </div>

        {/* Right: 3 clean studio tabs */}
        <div className="vclip-side">
          <div className="vclip-tabs">
            <Seg
              value={sideTab}
              options={[["clip", t("视频")], ["cover", t("封面")], ["copy", t("文案")]]}
              onChange={switchSideTab}
            />
          </div>

          {sideTab === "clip" && (
            <>
              <section className="vclip-sec">
                <div className="vclip-sec-head">
                  <span className="vclip-label">{t("发到哪里")}</span>
                  {!activePreset && <span className="small muted">{t("自定义")}</span>}
                </div>
                <div className="vclip-presets" role="radiogroup" aria-label={t("发到哪里")}>
                  {CLIP_PRESETS.map((p) => (
                    <button key={p.id} type="button" role="radio" aria-checked={activePreset === p.id} className={`vclip-preset${activePreset === p.id ? " on" : ""}`} title={t(p.hint)} onClick={() => applyPreset(p)}>
                      {t(p.name)}
                    </button>
                  ))}
                </div>
                <div className="reading-row">
                  <span>{t("画面比例")}</span>
                  <Seg
                    value={aspect}
                    options={[["source", t("原视频")], ["9:16", "9:16"], ["3:4", "3:4"], ["1:1", "1:1"], ["16:9", "16:9"]]}
                    onChange={setAspect}
                  />
                </div>
                {fitShown && (
                  <div className="reading-row">
                    <span>{t("画面放法")}</span>
                    <Seg
                      value={fit}
                      options={[["card", t("完整画面")], ["fill", t("铺满裁切")]]}
                      onChange={setFit}
                    />
                  </div>
                )}
                <div className="reading-row" title={t("成片按这个速度导出")}>
                  <span>{t("成片倍速")}</span>
                  <Seg
                    value={String(speed)}
                    options={SPEEDS.map((r) => [String(r), `${r}×`])}
                    onChange={(v) => setSpeed(Number(v))}
                  />
                </div>
                <label className="vclip-sub-toggle">
                  <input type="checkbox" checked={removePauses} onChange={(e) => setRemovePauses(e.target.checked)} />
                  {t("去掉停顿")}
                  {removePauses && (
                    <span className="muted small">
                      {!preparedValid
                        ? t("获取画面后自动找出停顿")
                        : pauses?.cuts.length
                          ? t("剪掉 {n} 处，省 {s} 秒", { n: pauses.cuts.length, s: pauses.saved.toFixed(1) })
                          : t("这段没有明显停顿")}
                    </span>
                  )}
                </label>
                <label className="vclip-sub-toggle" title={t("按 -14 LUFS 统一响度，抖音、视频号、B站、YouTube 都按这个标准播放")}>
                  <input type="checkbox" checked={loudness} onChange={(e) => setLoudness(e.target.checked)} />
                  {t("音量统一")}
                  {loudness && <span className="muted small">{t("不同节目导出后一样响")}</span>}
                </label>
                <label className="vclip-sub-toggle">
                  <input type="checkbox" checked={addSubs} onChange={(e) => setAddSubsPick(e.target.checked)} />
                  {t("加字幕")}
                  {translatingClip && <span className="muted small"><Loader2 size={11} className="spin" /> {t("正在补充中文翻译…")}</span>}
                  {!translatingClip && prepared?.hasBurnedSubtitles && !isForeignClip && <span className="muted small">{t("原视频已有字幕，默认不加")}</span>}
                </label>
                {addSubs && <div className="reading-row">
                  <span>{t("字幕语言")}</span>
                  <Seg
                    value={subLang}
                    options={hasZh ? [["bilingual", t("双语")], ["translation", t("中文")], ["original", t("原文")]] : [["original", t("原文")]]}
                    onChange={setSubLang}
                  />
                </div>}
                {addSubs && <div className="reading-row">
                  <span>{t("字幕大小")}</span>
                  <div className="vclip-crop-row vclip-size-row">
                    <input type="range" min={0.7} max={1.5} step={0.05} value={subScale} onChange={(e) => setSubScale(Number(e.target.value))} aria-label={t("字幕大小")} />
                    <span className="num small muted">{Math.round(subScale * 100)}%</span>
                  </div>
                </div>}
              </section>
              {(card || (!card && aspect !== "source") || geo.cropBox) && (
                <details className="vclip-sec vclip-adv">
                  <summary className="vclip-sec-head"><span className="vclip-label">{t("画面细节")}</span><span className="vclip-adv-end"><span className="small muted">{t("裁切、底色与取景")}</span><ChevronDown size={14} className="vclip-chev" /></span></summary>
                  {card && (
                    <div className="reading-row">
                      <span>{t("画面裁成")}</span>
                      <Seg
                        value={cardCrop}
                        options={[["none", t("完整")], ["4:3", "4:3"], ["1:1", "1:1"]]}
                        onChange={setCardCrop}
                      />
                    </div>
                  )}
                  {card && (
                    <div className="reading-row">
                      <span>{t("卡片底色")}</span>
                      <Seg
                        value={cardBg}
                        options={[["blur", t("画面模糊")], ["night", t("墨夜")], ["paper", t("纸白")]]}
                        onChange={setCardBg}
                      />
                    </div>
                  )}
                  {cropping && (
                    <>
                      {canPan && <label className="vclip-sub-toggle">
                        <input type="checkbox" checked={smartFrame} onChange={(e) => setSmartFrame(e.target.checked)} />
                        {t("跟着说话的人取景")}
                        {smartFrame && <span className="muted small">{!preparedValid ? t("获取画面后自动找人") : track ? (track.length > 1 ? t("画面会随人物移动 {n} 次", { n: (track.length - 1) / 2 }) : t("人物一直在同一位置")) : t("没找到人物，用下面的位置")}</span>}
                      </label>}
                      <div className="reading-row">
                        <span>{t("水平取景")}</span>
                        <div className="vclip-crop-row">
                          <input type="range" min={0} max={1} step={0.02} value={following ? viewCropX : cropX} disabled={following} onChange={(e) => setCropX(Number(e.target.value))} />
                          <span className="num small muted">{following ? t("自动") : `${Math.round(cropX * 100)}%`}</span>
                        </div>
                      </div>
                    </>
                  )}
                </details>
              )}
              {addSubs && (
                <details className="vclip-sec vclip-adv">
                  <summary className="vclip-sec-head"><span className="vclip-label">{t("字幕样式")}</span><span className="vclip-adv-end"><span className="small muted">{t(SUB_STYLES.find((x) => x.id === subStyle)?.name ?? "")}</span><ChevronDown size={14} className="vclip-chev" /></span></summary>
                  <div className="vclip-styles">
                    {SUB_STYLES.map((st) => (
                      <button key={st.id} type="button" className={`vclip-style-btn ${subStyle === st.id ? "on" : ""}`} onClick={() => setSubStyle(st.id)}>
                        <span className="vclip-swatch" style={{ background: st.swatchBg, color: st.swatchFg }}>Aa</span>
                        <span>{t(st.name)}</span>
                      </button>
                    ))}
                  </div>
                  {!card && prepared?.hasBurnedSubtitles && (
                    <label className="vclip-sub-toggle"><input type="checkbox" checked={liftBurned} onChange={(e) => setLiftBurned(e.target.checked)} /> {t("避让原字幕")}</label>
                  )}
                </details>
              )}
              <details className="vclip-sec vclip-adv">
                <summary className="vclip-sec-head"><span className="vclip-label">{t("标题与署名")}</span><span className="vclip-adv-end"><span className="small muted clamp1">{showTitle ? title : t("不显示标题")}</span><ChevronDown size={14} className="vclip-chev" /></span></summary>
                <div className="vclip-chips">
                  <ToggleChip on={showTitle} onChange={setShowTitle}>{t("顶部金句标题")}</ToggleChip>
                  <ToggleChip on={showSource} onChange={setShowSource}>{t("节目出处")}</ToggleChip>
                  <ToggleChip on={watermark} onChange={setWatermark}>{t("SubCast 水印")}</ToggleChip>
                </div>
                <div className="vclip-fields">
                  {showTitle && (
                    <div className="vclip-field">
                      <span>{t("金句标题")}</span>
                      <TextField value={title} onValue={setTitle} clearable placeholder={t("输入切片顶部金句标题")} aria-label={t("金句标题")}
                        suffix={title ? <span className="num">{[...title].length}</span> : undefined} />
                    </div>
                  )}
                  <div className="vclip-field">
                    <span>{t("底部署名")}</span>
                    <TextField value={brandText} onValue={setBrandText} clearable placeholder={t("账号名或品牌，可不填")} aria-label={t("底部署名")} />
                  </div>
                </div>
              </details>
              <details className="vclip-sec vclip-adv">
                <summary className="vclip-sec-head">
                  <span className="vclip-label">{t("微调起止与字幕文字")}</span>
                  <span className="vclip-adv-end"><span className="num small muted">{montage ? t("{n} 段拼接", { n: parts.length }) : `${clock(clipStart)} – ${clock(clipEnd)}`} · {Math.round(clipDur)}s</span><ChevronDown size={14} className="vclip-chev" /></span>
                </summary>
                {/* Start and end side by side: where each sits, a sentence earlier or later, and its lead-in or tail. */}
                <div className="vclip-bounds">
                  {([
                    ["start", t("开头"), clipStart, padBefore, setPadBefore, -0.3, 1.0,
                      startIdx <= 0, () => setStartIdx((i) => Math.max(0, i - 1)),
                      startIdx >= endIdx, () => setStartIdx((i) => Math.min(endIdx, i + 1))],
                    ["end", t("结尾"), clipEnd, padAfter, setPadAfter, -0.3, 1.2,
                      endIdx <= startIdx, () => setEndIdx((i) => Math.max(startIdx, i - 1)),
                      endIdx >= segments.length - 1, () => setEndIdx((i) => Math.min(segments.length - 1, i + 1))],
                  ] as const).map(([key, label, at, pad, setPad, lo, hi, noEarlier, earlier, noLater, later]) => (
                    <div className="vclip-bound" key={key}>
                      <div className="vclip-bound-head">
                        <span className="vclip-bound-name">{label}</span>
                        {!montage && <span className="num small muted">{clock(at)}</span>}
                        {!montage && (
                          <span className="vclip-nudge" role="group" aria-label={label}>
                            <button type="button" disabled={noEarlier} onClick={earlier} title={t("提前一句")} aria-label={t("提前一句")}><ChevronLeft size={14} /></button>
                            <button type="button" disabled={noLater} onClick={later} title={t("推后一句")} aria-label={t("推后一句")}><ChevronRight size={14} /></button>
                          </span>
                        )}
                      </div>
                      <label className="vclip-pad">
                        <span className="small muted" title={key === "start" ? t("开头留白") : t("结尾留白")}>{t("留白")}</span>
                        <input type="range" min={lo} max={hi} step={0.05} value={pad} onChange={(e) => setPad(Number(e.target.value))} />
                        <span className="num small muted">{pad.toFixed(2)}s</span>
                      </label>
                    </div>
                  ))}
                </div>
                <div className="vclip-lines" ref={linesRef}>
                  {clipLineIdx.map((idx, k) => {
                    const seg = segments[idx];
                    const partBreak = montage && k > 0 && clipLineIdx[k - 1] !== idx - 1;
                    const zhText = effectiveZh[idx] ?? "";
                    const origText = effectiveSegments[idx]?.text ?? seg.text;
                    const isEditing = editingIdx === idx;
                    const isActive = activePreviewIdx === idx;
                    return (
                      <div
                        key={idx}
                        data-vclip-i={idx}
                        className={`vclip-line ${isActive ? "on" : ""}${partBreak ? " part-start" : ""}`}
                        onClick={() => {
                          setActivePreviewIdx(idx);
                          if (preparedValid && videoRef.current) {
                            const at = toClipTime(idx, seg.start) + 0.05;
                            videoRef.current.currentTime = (prepared?.lead ?? 0) + at;
                            setPvTime(at);
                          }
                        }}
                      >
                        <span className="num vclip-lt">{clock(seg.start)}</span>
                        <div className="vclip-lbody">
                          {isEditing ? (
                            <div className="vclip-edit-box" onClick={(e) => e.stopPropagation()}>
                              {hasZh && (
                                <TextField size="sm" value={zhText} placeholder={t("中文译文")} aria-label={t("中文译文")} onValue={(v) => setZhEdits((m) => ({ ...m, [idx]: v }))} />
                              )}
                              <TextField size="sm" value={origText} placeholder={t("原文")} aria-label={t("原文")} onValue={(v) => setOrigEdits((m) => ({ ...m, [idx]: v }))} onEnter={() => setEditingIdx(null)} />
                              <div className="vclip-edit-actions">
                                <button type="button" className="btn ghost sm" onClick={() => {
                                  setZhEdits((m) => { const n = { ...m }; delete n[idx]; return n; });
                                  setOrigEdits((m) => { const n = { ...m }; delete n[idx]; return n; });
                                }}><RotateCcw size={11} /> {t("还原")}</button>
                                <button type="button" className="btn primary sm" onClick={() => setEditingIdx(null)}><Check size={11} /> {t("完成")}</button>
                              </div>
                            </div>
                          ) : (
                            <>
                              {zhText && <p className="vclip-lzh">{zhText}</p>}
                              <p className="vclip-lorig">{origText}</p>
                            </>
                          )}
                        </div>
                        {!isEditing && (
                          <button type="button" className="link-btn vclip-edit-btn" onClick={(e) => { e.stopPropagation(); setEditingIdx(idx); }}>{t("改字")}</button>
                        )}
                      </div>
                    );
                  })}
                </div>
              </details>
            </>
          )}

          {sideTab === "cover" && (
            <>
              <section className="vclip-sec">
                <div className="vclip-sec-head">
                  <span className="vclip-label">{t("封面视觉模板")}</span>
                  <span className="small muted">{t("用【】包住关键词自动高亮")}</span>
                </div>
                <div className="vclip-tpl-grid">
                  {COVER_TEMPLATES.map((tpl) => (
                    <button key={tpl.id} type="button" className={`vclip-tpl-btn ${coverTpl === tpl.id ? "on" : ""}`} onClick={() => setCoverTpl(tpl.id)}>
                      <span className="vclip-tpl-swatch">
                        {tplThumbs[tpl.id] ? <img src={tplThumbs[tpl.id]} alt="" /> : <Loader2 size={12} className="spin muted" />}
                      </span>
                      <span>{t(tpl.name)}</span>
                    </button>
                  ))}
                </div>
                <div className="vclip-fields">
                  <div className="vclip-field">
                    <span>{t("大字标题")}</span>
                    <TextField value={coverHook} onValue={setCoverHook} clearable placeholder={t("封面大字标题，如：未来三年，【会提问的人】胜出")} aria-label={t("大字标题")} />
                  </div>
                  <div className="vclip-field">
                    <span>{t("副标题")}</span>
                    <TextField value={coverSub} onValue={setCoverSub} clearable placeholder={t("副标题钩子（可选）")} aria-label={t("副标题")} />
                  </div>
                  <div className="vclip-field">
                    <span>{t("眉标")}</span>
                    <TextField value={coverKicker} onValue={setCoverKicker} clearable placeholder={t("顶部眉标（节目名）")} aria-label={t("眉标")} />
                  </div>
                </div>
                <div className="vclip-cover-style">
                  <div className="vclip-cover-tools" role="toolbar" aria-label={t("封面文字")}>
                    <select className="vclip-font-select" value={coverFont} onChange={(e) => setCoverFont(e.target.value as CoverFontId)} aria-label={t("字体")}>
                      {COVER_FONTS.map(([v, label]) => <option key={v} value={v}>{t(label)}</option>)}
                    </select>
                    <div className="vclip-swatches" role="radiogroup" aria-label={t("高亮色")}>
                      {ACCENTS.map((c) => (
                        <button key={c || "auto"} type="button" role="radio" aria-checked={coverAccent === c} className={`sw${coverAccent === c ? " on" : ""}${c ? "" : " auto"}`}
                          style={c ? { background: c } : undefined} title={c ? c : t("跟随样式")} onClick={() => setCoverAccent(c)} />
                      ))}
                    </div>
                    <button type="button" className={`link-btn small${coverAdvanced ? " on" : ""}`} aria-expanded={coverAdvanced} onClick={() => setCoverAdvanced(!coverAdvanced)}>{t("高级")}</button>
                  </div>
                  {coverAdvanced && (
                    <div className="vclip-cover-adv">
                      <label><span>{t("字号")}</span><input type="range" min={0.8} max={1.25} step={0.05} value={coverScale} onChange={(e) => setCoverScale(Number(e.target.value))} /><span className="num">{Math.round(coverScale * 100)}%</span></label>
                      <label><span>{t("字间距")}</span><input type="range" min={-0.05} max={0.2} step={0.01} value={coverTracking} onChange={(e) => setCoverTracking(Number(e.target.value))} /><span className="num">{Math.round(coverTracking * 100)}</span></label>
                      {(coverScale !== 1 || coverTracking !== 0) && <button type="button" className="link-btn small" onClick={() => { setCoverScale(1); setCoverTracking(0); }}>{t("还原")}</button>}
                    </div>
                  )}
                </div>
              </section>

              <section className="vclip-sec">
                <div className="vclip-sec-head">
                  <span className="vclip-label">{t("封面背景剧照")}</span>
                  {!preparedValid && (
                    <button type="button" className="btn ghost sm" disabled={!!busyJob} onClick={() => void ensurePrepared()}>
                      <ImageIcon size={12} /> {t("提取本段高清剧照")}
                    </button>
                  )}
                </div>
                {prepared?.frames.length ? (
                  <div className="vclip-frames">
                    {prepared.frames.map((src, idx) => (
                      <button
                        key={idx}
                        type="button"
                        className={`vclip-frame-btn ${activeFrameIdx === idx ? "on" : ""}`}
                        onClick={() => setSelectedFrame(idx)}
                        title={idx === prepared.bestFrame ? t("AI 自动优选最清晰帧") : undefined}
                      >
                        <img src={src} alt="" />
                        {idx === prepared.bestFrame && <span className="vclip-frame-best">{t("推荐")}</span>}
                      </button>
                    ))}
                  </div>
                ) : (
                  <p className="muted small">{t("默认使用节目封面；点右上角「提取本段高清剧照」或导出时自动抽取 5 帧真实人物画面并优选。")}</p>
                )}
              </section>

              <section className="vclip-sec">
                <div className="reading-row">
                  <span>{t("MP4 片头封面")}</span>
                  <Seg
                    value={coverMode}
                    options={[["flash", t("隐形首帧")], ["hold", t("定格 1 秒")], ["none", t("不压片头")]]}
                    onChange={setCoverMode}
                  />
                </div>
                <p className="muted small">
                  {coverMode === "flash"
                    ? t("开头 0.14 秒隐形放入爆款封面：小红书/视频号/抖音/B站自动抓第 0 帧当封面，播放几乎无感且不吞字。")
                    : coverMode === "hold"
                      ? t("片头静态展示 1 秒爆款封面后再开讲，音画同步顺延，适合直接发朋友圈与公众号视频。")
                      : t("正片开头不压封面，仅通过下方选项内嵌缩略图或保存独立封面 PNG。")}
                </p>
                <div className="vclip-chips">
                  <ToggleChip on={embedThumbnail} onChange={setEmbedThumbnail}>{t("内嵌视频缩略图")}</ToggleChip>
                  <ToggleChip on={saveCoverSidecar} onChange={setSaveCoverSidecar}>{t("同目录保存封面 PNG")}</ToggleChip>
                  {videoCoverAspect !== "16:9" && saveCoverSidecar && (
                    <ToggleChip on={extraLandscapeCover} onChange={setExtraLandscapeCover}>{t("附带 16:9 横版封面（公众号/B站）")}</ToggleChip>
                  )}
                </div>
                <div className="vclip-cover-actions">
                  <button type="button" className="btn ghost sm" onClick={() => void copyCoverImage()}>
                    {copiedFlag === "cover" ? <><Check size={12} /> {t("已复制")}</> : <><Copy size={12} /> {t("复制封面图")}</>}
                  </button>
                  <button type="button" className="btn ghost sm" onClick={() => void saveCoverImage()}><ImageIcon size={12} /> {t("保存当前封面")}</button>
                  <button type="button" className="btn ghost sm" onClick={() => void saveCoverImage("3:4")}><ImageIcon size={12} /> {t("保存 3:4 小红书封面")}</button>
                </div>
              </section>
            </>
          )}

          {sideTab === "copy" && (
            <section className="vclip-sec vclip-copy-sec">
              <div className="vclip-sec-head">
                <Seg
                  value={postPlatform}
                  options={[["short", t("短视频")], ["article", t("视频文章")], ["social", t("X · 推文")]]}
                  onChange={setPostPlatform}
                />
                <div className="row-actions tight">
                  <span className="num small muted">{t("{n} 字", { n: currentPostText.trim().length })}</span>
                  {postEdits[postPlatform] !== undefined && (
                    <button type="button" className="link-btn" onClick={() => setPostEdits((m) => { const n = { ...m }; delete n[postPlatform]; return n; })}>
                      {t("还原")}
                    </button>
                  )}
                  <button type="button" className="btn sm" onClick={() => void copyCurrentPost()}>
                    {copiedFlag === "post" ? <><Check size={12} /> {t("已复制")}</> : <><Copy size={12} /> {t("复制文案")}</>}
                  </button>
                </div>
              </div>
              {polishing ? (
                <p className="vclip-writing small"><Loader2 size={12} className="spin" /> {t("AI 正在写发布文案…")}</p>
              ) : (
                <button type="button" className="link-btn small vclip-again" onClick={() => void runAiPolish(true, true)}>{t("换一版")}</button>
              )}
              <p className="muted small">
                {postPlatform === "short"
                  ? t("适配小红书、微信视频号、抖音、B站：直接粘贴标题、推荐钩子与标签。")
                  : postPlatform === "article"
                    ? t("适配公众号文章、即刻、知乎：包含观点导读、双语原话摘录与回跳出处。")
                    : t("适配 X / 朋友圈：精炼一句话钩子 + 金句原话 + 跳转链接。")}
              </p>
              <textarea
                className="vclip-post-box"
                value={currentPostText}
                onChange={(e) => setPostEdits((m) => ({ ...m, [postPlatform]: e.target.value }))}
              />
            </section>
          )}

          {error && (
            <div className="vclip-err-row">
              <p className="error-text">{error}</p>
              {needsSetup && <button type="button" className="btn sm" onClick={() => { closeVideoClipDialog(); openSettings("model"); }}>{t("去设置")}</button>}
            </div>
          )}

        </div>
      </div>

      <div className="modal-foot vclip-foot">
        {/* Always in view: what's happening (with stages and a finish line), or what to do with the export. */}
        {busyJob ? (
          <div className="vclip-foot-progress" role="status">
            <div className="vclip-foot-row">
              <span className="vclip-step">{busyJob.stage === "render" ? t("第 2/2 步 · 合成视频") : t("第 1/2 步 · 获取画面")}</span>
              <span className="muted small clamp1">{tr(busyJob.message)}</span>
              <span className="num">{Math.round(busyJob.progress * 100)}%</span>
              <button type="button" className="link-btn" onClick={() => void api.cancelJob(busyJob.id)}>{t("取消")}</button>
            </div>
            <div className="bar live"><div style={{ width: `${Math.max(3, busyJob.progress * 100)}%` }} /></div>
          </div>
        ) : exported ? (
          <div className="vclip-foot-done">
            <span className="vclip-done-mark">
              <Check size={14} strokeWidth={3} /> {t("已导出")}
              {exported.elapsedSec != null && <span className="vclip-elapsed num">· {exported.elapsedSec.toFixed(1)}s</span>}
            </span>
            <button type="button" className="btn sm" onClick={() => void api.revealFile(exported.videoPath).catch((e) => notify(errorText(e)))}>
              <FolderOpen size={13} /> {t("在文件夹中显示")}
            </button>
            <button type="button" className="btn ghost sm" onClick={() => void copyCurrentPost()}>
              {copiedFlag === "post" ? <><Check size={13} /> {t("已复制")}</> : <><Copy size={13} /> {t("复制文案")}</>}
            </button>
            <button type="button" className="btn ghost sm" onClick={() => void copyCoverImage()}>
              {copiedFlag === "cover" ? <><Check size={13} /> {t("已复制")}</> : <><ImageIcon size={13} /> {t("复制封面")}</>}
            </button>
          </div>
        ) : (
          <button type="button" className="btn ghost" onClick={() => void copyCurrentPost()}>
            {copiedFlag === "post" ? <><Check size={14} /> {t("已复制")}</> : <><Copy size={14} /> {t("复制发布文案")}</>}
          </button>
        )}
        <span className="spacer" />
        <button type="button" className="btn ghost" onClick={requestClose}>{t("关闭")}</button>
        {!batch && (
          <button type="button" className="btn" disabled={!!busyJob} onClick={() => void runExport(true)}>
            {t("另存为…")}
          </button>
        )}
        {batch ? (
          run ? (
            <button type="button" className="btn primary" onClick={() => { stopBatch.current = true; setStopping(true); }} disabled={stopping}>
              <Loader2 size={14} className="spin" /> {stopping ? t("本段导出后停止") : t("第 {i}/{n} 段 · 停止", { i: run.i + 1, n: batch.length })}
            </button>
          ) : (
            <button type="button" className="btn primary" disabled={!!busyJob} onClick={() => { stopBatch.current = false; setRun({ i: 0, ok: 0, step: "set" }); }} title={t("用现在的设置依次导出选中的每一段")}>
              <Scissors size={14} /> {t("导出这 {n} 段", { n: batch.length })}
            </button>
          )
        ) : (
          <button type="button" className="btn primary" disabled={!!busyJob} onClick={() => void runExport(false)} title={`${t("导出 MP4")} (${kbd("⌘↵")})`}>
            <Scissors size={14} /> {t("导出 MP4")}
          </button>
        )}
      </div>
    </Modal>
  );
}

/** Under the range bar: the clip opens on a link word or mid-sentence, with a one-click better start. */
function OpeningHint({ segments, startIdx, endIdx, onStart }: { segments: Segment[]; startIdx: number; endIdx: number; onStart: (i: number) => void }) {
  const [hidden, setHidden] = useState<number | null>(null);
  const issue = openingIssue(segments, startIdx);
  if (!issue || hidden === startIdx) return null;
  const to = betterStart(segments, startIdx, endIdx);
  const preview = to != null ? [...segments[to].text.trim()].slice(0, 12).join("") : "";
  return (
    <div className="vclip-opening" role="note">
      <span className="vclip-opening-dot" aria-hidden />
      <span className="small">
        {issue.kind === "lead-word"
          ? t("开头是「{w}…」，没看过前文的人可能接不上", { w: issue.word })
          : t("开头接在上一句中间，前 3 秒可能听不懂")}
      </span>
      {to != null && (
        <button type="button" className="link-btn small" onClick={() => onStart(to)} title={segments[to].text}>
          {t("从「{s}…」开始", { s: preview })}
        </button>
      )}
      <button type="button" className="icon-btn sm" onClick={() => setHidden(startIdx)} aria-label={t("忽略")} title={t("忽略")}><X size={12} /></button>
    </div>
  );
}

/** The speaker's place (0–1 across the picture) at clip time `t` on an eased track. */
function trackAt(track: [number, number][], t: number): number {
  if (t <= track[0][0]) return track[0][1];
  for (let i = 1; i < track.length; i++) {
    const [t1, x1] = track[i];
    if (t < t1) { const [t0, x0] = track[i - 1]; return t1 - t0 < 1e-3 ? x1 : x0 + (x1 - x0) * (t - t0) / (t1 - t0); }
  }
  return track[track.length - 1][1];
}

/** Spare width (px) a crop of `bw`×`bh` has in a `sw`×`sh` picture scaled to cover it. */
function panRoom(bw: number, bh: number, sw: number, sh: number): number {
  return sw * Math.max(bw / sw, bh / sh) - bw;
}

/** A speaker place → the crop's share of the spare width (what `cropX` means), centring them in a `bw`×`bh`
 *  box cut from a `sw`×`sh` picture scaled to cover it; the same rule the export's crop uses. */
function centreShare(sx: number, bw: number, bh: number, sw: number, sh: number): number {
  const w = sw * Math.max(bw / sw, bh / sh);
  if (w - bw < 1) return 0.5;
  return Math.min(1, Math.max(0, (sx * w - bw / 2) / (w - bw)));
}
