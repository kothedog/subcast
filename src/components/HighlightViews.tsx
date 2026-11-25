import { useEffect, useMemo, useRef, useState } from "react";
import { ArrowDownUp, Check, Copy, Layers, Loader2, MoreHorizontal, Pause, Play, RefreshCw, Scissors, Sparkles, Square } from "lucide-react";
import { useShallow } from "zustand/react/shallow";
import { api, errorText, type Episode, type Segment } from "../lib/api";
import { clock } from "../lib/format";
import { buildPlatformPost, clearHighlightsError, loadHighlights, montageClip, polishClipCopy, previewEpisodeRange, runHighlights, saveClipCopy, stopHighlights, useHighlights, type HighlightClip } from "../lib/highlights";
import { t, tk, useLang } from "../lib/i18n";
import { useJobs } from "../lib/jobs";
import { isZh, useZh } from "../lib/zh";
import { useStore } from "../store";
import { openVideoClipDialog } from "./VideoClipDialog";
import { ClipHistory } from "./ClipHistory";

/** Renders a highlight card's opening quote, automatically fetching a Chinese translation for English lines. */
function HighlightQuote({ zh, orig }: { zh?: string; orig: string }) {
  const autoZh = useZh(!zh && orig && !isZh(orig) ? orig : null);
  const shownZh = zh || autoZh;
  return (
    <div className="hl-quote">
      {shownZh && <p className="hl-qzh clamp2">{shownZh}</p>}
      {orig && <p className="hl-qorig clamp2">{orig}</p>}
    </div>
  );
}

const INTENT_SUGGESTIONS = [
  tk("犀利观点与金句"),
  tk("实战方法与具体案例"),
  tk("争议判断与行业趋势"),
];

/** Gets cutting ready for an episode with highlights: its video addresses are resolved in the background
 *  (~30 s the first time), and the best two clips are fetched with the exact range the clip window asks for
 *  (0.25 s before, 0.4 s after), so opening them plays the real cut at once. */
export function useClipWarm(ep: Episode, segments: Segment[]) {
  const bundle = useHighlights((s) => s.bundles[ep.id]);
  const topRanges = useMemo(() => [...(bundle?.clips ?? [])].sort((a, b) => b.score - a.score).slice(0, 2)
    .map((c) => [Math.max(0, (segments[c.startIdx]?.start ?? c.start) - 0.25), (segments[c.endIdx]?.end ?? c.end) + 0.4] as [number, number]), [bundle, segments]);
  const key = topRanges.map((r) => r.join("-")).join("|");
  useEffect(() => {
    if (ep.videoId && bundle?.clips.length) void api.videoClipWarm(ep.videoId, topRanges).catch(() => {});
  }, [ep.videoId, key]); // eslint-disable-line react-hooks/exhaustive-deps
}

export function HighlightsPanel({
  ep,
  segments,
  zh,
  onJumpToTranscript,
}: {
  ep: Episode;
  segments: Segment[];
  zh?: (string | null | undefined)[];
  onJumpToTranscript: (index: number, time: number) => void;
}) {
  useLang((s) => s.lang);
  const bundle = useHighlights((s) => s.bundles[ep.id]);
  const loading = useHighlights((s) => !!s.loading[ep.id]);
  const error = useHighlights((s) => s.error[ep.id]);
  const needsSetup = useHighlights((s) => !!s.needsSetup[ep.id]);
  const liveZh = useJobs((s) => s.translations[ep.id]?.items);
  const mergedZh = liveZh ?? zh;
  const { notify, openSettings, isCurrent, playing, playTime } = useStore(useShallow((s) => ({
    notify: s.notify,
    openSettings: s.openSettings,
    isCurrent: s.current?.id === ep.id,
    playing: s.playing,
    playTime: s.current?.id === ep.id ? Math.floor(s.time) : -1,
  })));

  const [intent, setIntent] = useState(bundle?.intent ?? "");
  const [sortBy, setSortBy] = useState<"score" | "time">("score");
  const [reasking, setReasking] = useState(false);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const copyTimer = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(copyTimer.current), []);

  useEffect(() => {
    void loadHighlights(ep.id);
  }, [ep.id]);
  // Resolving the video's addresses takes ~30 s: done now, cutting a clip later only downloads its stretch.
  useClipWarm(ep, segments);
  const [copying, setCopying] = useState<string | null>(null);
  // Picking clips for one montage (several stretches cut together, in time order).
  /** Ticking highlights: to cut them into one video, or to export each as its own. */
  const [picking, setPicking] = useState<false | "montage" | "batch">(false);
  const [picked, setPicked] = useState<string[]>([]);

  const clips = useMemo(() => {
    const list = bundle?.clips ?? [];
    if (sortBy === "time") return [...list].sort((a, b) => a.start - b.start);
    return [...list].sort((a, b) => b.score - a.score || a.start - b.start);
  }, [bundle, sortBy]);

  const startRun = (customIntent?: string) => {
    clearHighlightsError(ep.id);
    setReasking(false);
    void runHighlights(ep, segments, mergedZh, customIntent ?? intent);
  };

  /** Copies the model-written post for the clip, writing it first when the clip doesn't have one yet. */
  const copyShare = async (c: HighlightClip) => {
    if (copying) return;
    try {
      let share = c.articleCopy ? c.shareCopy : "";
      if (!share) {
        setCopying(c.id);
        const res = await polishClipCopy({ ep, segments, zh: mergedZh, startIdx: c.startIdx, endIdx: c.endIdx });
        share = res.shareCopy;
        void saveClipCopy(ep.id, c.id, { shareCopy: res.shareCopy, articleCopy: res.articleCopy });
      }
      // The same short-video post the clip window shows (title, text, tags, source).
      await navigator.clipboard.writeText(buildPlatformPost("short", { ep, title: c.title, coverHook: c.coverHook, coverSub: c.coverSub, shareCopy: share, tags: c.tags, segments, zh: mergedZh, startIdx: c.startIdx, endIdx: c.endIdx, clipStart: c.start }));
      setCopiedId(c.id);
      window.clearTimeout(copyTimer.current);
      copyTimer.current = window.setTimeout(() => setCopiedId((cur) => (cur === c.id ? null : cur)), 1600);
      notify(t("传播文案已复制"));
    } catch (e) {
      notify(errorText(e));
    } finally {
      setCopying(null);
    }
  };
  const togglePick = (id: string) => setPicked((p) => (p.includes(id) ? p.filter((x) => x !== id) : [...p, id]));
  const openMontage = (ids: string[]) => {
    const chosen = (bundle?.clips ?? []).filter((c) => ids.includes(c.id));
    if (chosen.length < 2) return;
    setPicking(false);
    setPicked([]);
    openVideoClipDialog({ ep, segments, zh: mergedZh, clip: montageClip(chosen) });
  };
  // Each picked highlight as its own video: the dialog opens on the first to set things up, then exports them all.
  const openBatch = (ids: string[]) => {
    const chosen = clips.filter((c) => ids.includes(c.id));
    if (chosen.length < 2) return;
    setPicking(false);
    setPicked([]);
    openVideoClipDialog({ ep, segments, zh: mergedZh, clip: chosen[0], batch: chosen });
  };
  // The best clips, in time order, up to about three minutes: a one-click "best of".
  const bestOf = () => {
    const ranked = [...(bundle?.clips ?? [])].sort((a, b) => b.score - a.score);
    const out: HighlightClip[] = [];
    let total = 0;
    for (const c of ranked) {
      if (out.length >= 5 || (out.length >= 2 && total + (c.end - c.start) > 200)) break;
      out.push(c);
      total += c.end - c.start;
    }
    openMontage(out.map((c) => c.id));
  };

  if (loading) {
    return (
      <div className="hl-empty">
        <div className="hl-card-intro">
          <Sparkles size={20} className="hl-spark" />
          <p className="es-title">{t("正在从整期文稿挑选高光片段")}</p>
          <p className="muted small">{t("按「钩子 → 论证 → 金句收束」结构筛选，自动吸附完整句首尾…")}</p>
          <div className="hl-wait-row">
            <Loader2 size={14} className="spin" />
            <button type="button" className="btn ghost sm" onClick={() => stopHighlights(ep.id)}>
              <Square size={12} fill="currentColor" /> {t("停止")}
            </button>
          </div>
        </div>
      </div>
    );
  }

  if (!bundle?.clips.length) {
    return (
      <div className="hl-empty">
        <div className="hl-card-intro">
          <Sparkles size={20} className="hl-spark" />
          <p className="es-title">{t("自动挑选高光金句片段")}</p>
          <p className="muted small">{t("从整期中抽取 30–120 秒独立成篇的高传播段落，支持一键剪出带双语字幕的视频切片。")}</p>
          <div className="hl-intent-box">
            <input
              value={intent}
              onChange={(e) => setIntent(e.target.value)}
              placeholder={t("可选：想重点找哪类话题，例如「创业踩坑」")}
              onKeyDown={(e) => { if (e.key === "Enter") startRun(); }}
            />
            <div className="hl-chips">
              {INTENT_SUGGESTIONS.map((s) => (
                <button
                  key={s}
                  type="button"
                  className={`hl-chip ${intent === t(s) ? "on" : ""}`}
                  onClick={() => setIntent(t(s))}
                >{t(s)}</button>
              ))}
            </div>
          </div>
          {error && <p className="error-text">{error}</p>}
          <div className="row-actions center">
            {needsSetup && (
              <button type="button" className="btn" onClick={() => openSettings("model")}>{t("去设置")}</button>
            )}
            <button type="button" className="btn primary" disabled={!segments.length} onClick={() => startRun()}>
              <Sparkles size={14} /> {t("提取高光片段")}
            </button>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="hl-panel">
      <div className="hl-toolbar">
        <span className="muted small hl-count">{t("共 {n} 段高光", { n: clips.length })}{bundle.intent ? ` · ${bundle.intent}` : ""}</span>
        <span className="spacer" />
        {ep.videoId && clips.length > 1 && !picking && (
          <>
            <button type="button" className="link-btn" onClick={bestOf} title={t("把评分最高的几段按时间顺序剪成一条")}>
              <Sparkles size={12} /> {t("精华合集")}
            </button>
            <button type="button" className="link-btn" onClick={() => { setPicking("batch"); setPicked([]); }} title={t("勾选几段，用同一套设置各导出一条")}>
              <Scissors size={12} /> {t("批量导出")}
            </button>
          </>
        )}
        <ClipHistory episodeId={ep.id} />
        <HighlightMore
          canPick={!!ep.videoId && clips.length > 1 && !picking}
          sortBy={sortBy}
          onPick={() => { setPicking("montage"); setPicked([]); }}
          onSort={() => setSortBy(sortBy === "score" ? "time" : "score")}
          onReask={() => setReasking(!reasking)}
        />
      </div>

      {reasking && (
        <div className="hl-reask">
          <input
            value={intent}
            onChange={(e) => setIntent(e.target.value)}
            placeholder={t("输入想找的话题方向（留空则全面挑选）")}
            onKeyDown={(e) => { if (e.key === "Enter") startRun(); }}
          />
          <button type="button" className="btn primary sm" onClick={() => startRun()}>{t("重新提取")}</button>
        </div>
      )}

      {picking && (
        <div className="hl-pickbar">
          <span className="small">
            {picking === "batch"
              ? (picked.length ? t("已选 {n} 段，每段导出一条视频", { n: picked.length }) : t("勾选要导出的片段"))
              : (picked.length ? t("已选 {n} 段，会按时间顺序拼成一条", { n: picked.length }) : t("勾选要拼在一起的片段"))}
          </span>
          <span className="spacer" />
          <button type="button" className="btn ghost sm" onClick={() => { setPicking(false); setPicked([]); }}>{t("取消")}</button>
          {picking === "batch" ? (
            <button type="button" className="btn primary sm" disabled={picked.length < 2} onClick={() => openBatch(picked)}>
              <Scissors size={12} /> {t("设置并导出")}
            </button>
          ) : (
            <button type="button" className="btn primary sm" disabled={picked.length < 2} onClick={() => openMontage(picked)}>
              <Scissors size={12} /> {t("拼成视频")}
            </button>
          )}
        </div>
      )}

      {error && <p className="error-text hl-err">{error}</p>}

      <div className="hl-list">
        {clips.map((c) => {
          const previewZh = mergedZh?.[c.startIdx]?.trim();
          const previewOrig = segments[c.startIdx]?.text ?? "";
          const durSec = Math.max(1, Math.round(c.end - c.start));
          const isPlayingThis = isCurrent && playing && playTime >= Math.floor(c.start) && playTime <= Math.ceil(c.end);
          const isCopied = copiedId === c.id;
          return (
            <article key={c.id} className={`hl-card${picking ? " picking" : ""}${picked.includes(c.id) ? " picked" : ""}${isPlayingThis ? " playing" : ""}`} onClick={picking ? () => togglePick(c.id) : undefined}>
              <div className="hl-card-top">
                {picking && <span className={`hl-check${picked.includes(c.id) ? " on" : ""}`}>{picked.includes(c.id) && <Check size={12} strokeWidth={3} />}</span>}
                <span className="hl-score" title={t("传播潜力评分")}>✦ {c.score}</span>
                <span className="num small muted">{clock(c.start)} – {clock(c.end)} · {durSec}s</span>
              </div>
              <h4 className="hl-title">
                <button type="button" className="hl-title-btn" title={t("在字幕里找到这一段")} onClick={(e) => { if (picking) return; e.stopPropagation(); onJumpToTranscript(c.startIdx, c.start); }}>{c.title}</button>
              </h4>
              {c.reason && <p className="hl-reason">{c.reason}</p>}
              <HighlightQuote zh={previewZh} orig={previewOrig} />
              {c.tags.length > 0 && (
                <div className="hl-tags">
                  {c.tags.map((tag) => <span key={tag} className="hl-tag">{tag}</span>)}
                </div>
              )}
              {!picking && <div className="hl-actions">
                <button
                  type="button"
                  className={`btn ghost sm${isPlayingThis ? " on" : ""}`}
                  onClick={() => {
                    if (isPlayingThis) useStore.getState().player?.toggle();
                    else previewEpisodeRange(ep, c.start, c.end);
                  }}
                >
                  {isPlayingThis ? <><Pause size={12} fill="currentColor" /> {t("暂停")}</> : <><Play size={12} fill="currentColor" /> {ep.videoId ? t("试看") : t("试听")}</>}
                </button>
                {ep.videoId && (
                  <button type="button" className="btn primary sm" onClick={() => openVideoClipDialog({ ep, segments, zh: mergedZh, clip: c })}>
                    <Scissors size={12} /> {t("剪成视频")}
                  </button>
                )}
                <button type="button" className="btn ghost sm" disabled={copying === c.id} onClick={() => void copyShare(c)} title={t("复制 AI 写的发布文案")}>
                  {copying === c.id ? <Loader2 size={12} className="spin" /> : isCopied ? <Check size={12} /> : <Copy size={12} />} {isCopied ? t("已复制") : t("复制文案")}
                </button>
              </div>}
            </article>
          );
        })}
      </div>
    </div>
  );
}

/** Displays AI highlight markers along the episode progress bar. */
export function SeekHighlights({
  episodeId,
  length,
  onSeek,
}: {
  episodeId?: string;
  length: number;
  onSeek: (seconds: number) => void;
}) {
  const clips = useHighlights((s) => (episodeId ? s.bundles[episodeId]?.clips : undefined));
  useEffect(() => {
    if (episodeId) void loadHighlights(episodeId);
  }, [episodeId]);

  if (!episodeId || !clips?.length || length <= 1) return null;
  return (
    <div className="seek-marks" aria-hidden>
      {clips.map((c) => {
        const left = Math.max(0, Math.min(99, (c.start / length) * 100));
        const width = Math.max(0.8, Math.min(100 - left, ((c.end - c.start) / length) * 100));
        return (
          <button
            key={c.id}
            type="button"
            className="seek-mark"
            style={{ left: `${left}%`, width: `${width}%` }}
            title={`${c.title} · ${clock(c.start)}`}
            onClick={(e) => { e.stopPropagation(); onSeek(c.start); }}
          />
        );
      })}
    </div>
  );
}

/** The highlights toolbar's less-used actions, behind "⋯". */
function HighlightMore({ canPick, sortBy, onPick, onSort, onReask }: { canPick: boolean; sortBy: "score" | "time"; onPick: () => void; onSort: () => void; onReask: () => void }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false); };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }, [open]);
  const pick = (f: () => void) => () => { setOpen(false); f(); };
  return (
    <div className="menu-wrap" ref={box}>
      <button type="button" className="icon-btn sm" aria-haspopup="menu" aria-expanded={open} title={t("更多")} aria-label={t("更多")} onClick={() => setOpen(!open)}><MoreHorizontal size={15} /></button>
      {open && (
        <div className="menu right" role="menu">
          {canPick && <button role="menuitem" onClick={pick(onPick)}><Layers size={14} /> {t("自选拼接")}</button>}
          <button role="menuitem" onClick={pick(onSort)}><ArrowDownUp size={14} /> {sortBy === "score" ? t("按时间顺序") : t("按传播度")}</button>
          <button role="menuitem" onClick={pick(onReask)}><RefreshCw size={14} /> {t("换方向重选")}</button>
        </div>
      )}
    </div>
  );
}
