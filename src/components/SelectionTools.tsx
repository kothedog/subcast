import { lazy, Suspense, useEffect, useLayoutEffect, useRef, useState } from "react";
import { Check, Copy, Image as ImageIcon, Languages, Loader2, Scissors } from "lucide-react";
import { api, type Episode } from "../lib/api";
import { createManualClip } from "../lib/highlights";
import { useJobs } from "../lib/jobs";
import { CJK } from "../lib/zh";
import { showName } from "../lib/picks";
import { useStore } from "../store";
import { useClipTools } from "../lib/clipTools";
import { t } from "../lib/i18n";
import { openVideoClipDialog } from "./VideoClipDialog";

const PosterDialog = lazy(() => import("./PosterDialog").then((m) => ({ default: m.PosterDialog })));

const ZONES = ".deep-body, .lines, .notes-body, .chat-scroll, .summary";

interface Sel { text: string; x: number; y: number; below: boolean; ts?: string; startIdx?: number; endIdx?: number }

/** A small toolbar over selected text in transcripts, 精读 and notes: copy with source, translate, make a poster. */
export function SelectionTools() {
  const [sel, setSel] = useState<Sel | null>(null);
  const [tr, setTr] = useState<string | null>(null);
  const [trCopied, setTrCopied] = useState(false);
  const [poster, setPoster] = useState<{ text: string; ts?: string } | null>(null);
  const barRef = useRef<HTMLDivElement>(null);
  const [clampedX, setClampedX] = useState<number | null>(null);
  const view = useStore((s) => s.view);
  const current = useStore((s) => s.current);
  const [hasVideo, setHasVideo] = useState(false);
  useEffect(() => {
    if (view.kind !== "episode") { setHasVideo(!!current?.videoId); return; }
    if (current?.id === view.id) { setHasVideo(!!current.videoId); return; }
    let live = true;
    void api.getEpisode(view.id).then((ep) => { if (live) setHasVideo(!!ep?.videoId); }).catch(() => {});
    return () => { live = false; };
  }, [view, current?.id, current?.videoId]);
  const timer = useRef<number | undefined>(undefined);
  useEffect(() => {
    const read = () => {
      const s = window.getSelection();
      if (!s || s.isCollapsed || !s.rangeCount) { setSel(null); setTr(null); return; }
      const text = s.toString().replace(/\s+/g, " ").trim();
      const r = s.getRangeAt(0);
      const host = (r.commonAncestorContainer.nodeType === 1 ? r.commonAncestorContainer : r.commonAncestorContainer.parentElement) as HTMLElement | null;
      if (text.length < 4 || !host?.closest(ZONES) || host.closest(".sel-bar")) { setSel(null); return; }
      // One word in subtitles with the learning card on: its word card shows instead (with 加入生词本).
      if (/^[A-Za-z][A-Za-z'’-]*$/.test(text) && host.closest(".lines.learning")) { setSel(null); return; }
      const rect = r.getBoundingClientRect();
      const startLine = (r.startContainer.nodeType === 1 ? (r.startContainer as HTMLElement) : r.startContainer.parentElement)?.closest<HTMLElement>(".line[data-i]");
      const endLine = (r.endContainer.nodeType === 1 ? (r.endContainer as HTMLElement) : r.endContainer.parentElement)?.closest<HTMLElement>(".line[data-i]");
      const ts = (startLine?.querySelector(".line-time")?.textContent ?? undefined) || undefined;
      const sRaw = startLine ? Number(startLine.dataset.i) : NaN;
      const eRaw = endLine ? Number(endLine.dataset.i) : sRaw;
      const startIdx = Number.isFinite(sRaw) ? Math.min(sRaw, Number.isFinite(eRaw) ? eRaw : sRaw) : undefined;
      const endIdx = Number.isFinite(sRaw) ? Math.max(sRaw, Number.isFinite(eRaw) ? eRaw : sRaw) : undefined;
      const below = rect.top < 70;
      setTr(null);
      setTrCopied(false);
      setClampedX(null);
      setSel({ text, x: Math.max(12, Math.min(rect.left + rect.width / 2, innerWidth - 12)), y: below ? rect.bottom + 10 : rect.top - 10, below, ts, startIdx, endIdx });
    };
    const onChange = () => { window.clearTimeout(timer.current); timer.current = window.setTimeout(read, 220); };
    document.addEventListener("selectionchange", onChange);
    return () => document.removeEventListener("selectionchange", onChange);
  }, []);

  useEffect(() => {
    if (!sel) return;
    const hide = (e: Event) => {
      if (e.type === "scroll" && barRef.current?.contains(e.target as Node)) return;
      setSel(null);
      setTr(null);
    };
    window.addEventListener("scroll", hide, true);
    window.addEventListener("resize", hide);
    return () => {
      window.removeEventListener("scroll", hide, true);
      window.removeEventListener("resize", hide);
    };
  }, [sel]);

  useLayoutEffect(() => {
    if (!sel || !barRef.current) return;
    const w = barRef.current.offsetWidth;
    if (!w) return;
    const pad = 12;
    const half = w / 2;
    const next = Math.max(pad + half, Math.min(sel.x, window.innerWidth - pad - half));
    if (Math.abs((clampedX ?? sel.x) - next) > 1) setClampedX(next);
  }, [sel, tr, clampedX]);

  const episode = async (): Promise<Episode | null> => {
    const { view, current } = useStore.getState();
    if (view.kind === "episode") return (await api.getEpisode(view.id)) ?? current;
    return current;
  };
  const copy = async () => {
    if (!sel) return;
    const ep = await episode();
    const src = ep ? `——《${ep.title}》${ep.showTitle ? ` · ${showName(ep.showTitle)}` : ""}${sel.ts ? ` · ${sel.ts}` : ""}${ep.link ? `\n${ep.link}` : ""}` : "";
    try { await navigator.clipboard.writeText(`“${sel.text}”\n${src}`); useStore.getState().notify(t("已复制，带出处")); } catch { useStore.getState().notify(t("复制失败")); }
  };
  const translate = async () => {
    if (!sel) return;
    setTr("…");
    try { setTr((await api.translateTexts([sel.text], CJK.test(sel.text) && !/[a-z]{4}/i.test(sel.text) ? "en" : "zh-CN"))[0] || t("没有翻译结果")); }
    catch { setTr(t("翻译服务暂时连不上，稍后再试")); }
  };

  const clipTools = useClipTools();
  const clipVideo = async () => {
    if (!sel || sel.startIdx == null) return;
    const ep = await episode();
    if (!ep?.videoId) return;
    const trData = useJobs.getState().transcripts[ep.id];
    if (!trData?.segments.length) return;
    const zhItems = useJobs.getState().translations[ep.id]?.items;
    const clip = createManualClip(trData.segments, zhItems, sel.startIdx, sel.endIdx ?? sel.startIdx, ep);
    setSel(null);
    window.getSelection()?.removeAllRanges();
    openVideoClipDialog({ ep, segments: trData.segments, zh: zhItems, clip });
  };

  const copyTr = async () => {
    if (!tr || tr === "…") return;
    try {
      await navigator.clipboard.writeText(tr);
      setTrCopied(true);
      window.setTimeout(() => setTrCopied(false), 1500);
      useStore.getState().notify(t("已复制"));
    } catch { useStore.getState().notify(t("复制失败")); }
  };

  return (
    <>
      {sel && (
        <div ref={barRef} className={`sel-bar ${sel.below ? "below" : ""}`} style={{ left: clampedX ?? sel.x, top: sel.y }} role="toolbar" aria-label={t("选中文字操作")}>
          <div className="sel-actions" onMouseDown={(e) => e.preventDefault()}>
            <button onClick={copy}><Copy size={13} /> {t("复制")}</button>
            <button onClick={translate}><Languages size={13} /> {t("翻译")}</button>
            <button onClick={() => { setPoster({ text: sel.text, ts: sel.ts }); setSel(null); }}><ImageIcon size={13} /> {t("生成海报")}</button>
            {clipTools && hasVideo && sel.startIdx != null && (
              <button onClick={() => void clipVideo()}><Scissors size={13} /> {t("剪视频")}</button>
            )}
          </div>
          {tr && (
            <div className="sel-tr">
              {tr === "…" ? <Loader2 size={13} className="spin" /> : (
                <>
                  <span>{tr}</span>
                  <button type="button" className="icon-btn sm sel-tr-copy" title={t("复制")} aria-label={t("复制")} onClick={() => void copyTr()}>
                    {trCopied ? <Check size={12} /> : <Copy size={12} />}
                  </button>
                </>
              )}
            </div>
          )}
        </div>
      )}
      {poster && (
        <Suspense fallback={null}>
          <PosterDialog text={poster.text} ts={poster.ts} getEpisode={episode} onClose={() => setPoster(null)} />
        </Suspense>
      )}
    </>
  );
}
