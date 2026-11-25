import { useMemo, useRef, useState } from "react";
import type { Segment } from "../lib/api";
import { clock } from "../lib/format";
import { t } from "../lib/i18n";

/** Sentences shown either side of the clip. */
const CONTEXT = 6;

/** The clip's start and end on a strip of its sentences (and a few either side), dragged by two handles that
 *  snap to sentence boundaries. The clip only changes when a handle is let go, so a drag doesn't refetch. */
export function ClipRangeBar({ segments, zh, startIdx, endIdx, onChange }: {
  segments: Segment[];
  zh?: (string | null | undefined)[];
  startIdx: number;
  endIdx: number;
  onChange: (startIdx: number, endIdx: number) => void;
}) {
  const [drag, setDrag] = useState<{ which: "start" | "end"; s: number; e: number } | null>(null);
  const trackRef = useRef<HTMLDivElement>(null);
  // The window stays put while dragging (it re-centres after the change lands).
  const win = useMemo(() => {
    const w0 = Math.max(0, startIdx - CONTEXT);
    const w1 = Math.min(segments.length - 1, endIdx + CONTEXT);
    const t0 = segments[w0]?.start ?? 0;
    const t1 = Math.max(t0 + 1, segments[w1]?.end ?? t0 + 1);
    return { w0, w1, t0, t1 };
  }, [segments, startIdx, endIdx]);
  const s = drag?.s ?? startIdx;
  const e = drag?.e ?? endIdx;
  const pct = (time: number) => ((time - win.t0) / (win.t1 - win.t0)) * 100;

  const idxAt = (clientX: number, which: "start" | "end", curS: number, curE: number) => {
    const r = trackRef.current?.getBoundingClientRect();
    if (!r) return which === "start" ? curS : curE;
    const at = win.t0 + Math.min(1, Math.max(0, (clientX - r.left) / r.width)) * (win.t1 - win.t0);
    const lo = which === "start" ? win.w0 : curS;
    const hi = which === "start" ? curE : win.w1;
    let best = which === "start" ? curS : curE;
    let bestD = Infinity;
    for (let i = lo; i <= hi; i++) {
      const edge = which === "start" ? segments[i].start : segments[i].end;
      const d = Math.abs(edge - at);
      if (d < bestD) { bestD = d; best = i; }
    }
    return best;
  };

  const onDown = (which: "start" | "end") => (ev: React.PointerEvent<HTMLButtonElement>) => {
    ev.preventDefault();
    ev.currentTarget.setPointerCapture(ev.pointerId);
    setDrag({ which, s: startIdx, e: endIdx });
  };
  const onMove = (ev: React.PointerEvent<HTMLButtonElement>) => {
    if (!drag) return;
    const i = idxAt(ev.clientX, drag.which, drag.s, drag.e);
    setDrag((d) => (d ? (d.which === "start" ? { ...d, s: i } : { ...d, e: i }) : d));
  };
  const onUp = () => {
    if (!drag) return;
    setDrag(null);
    if (drag.s !== startIdx || drag.e !== endIdx) onChange(drag.s, drag.e);
  };
  const onKey = (which: "start" | "end") => (ev: React.KeyboardEvent) => {
    const step = ev.key === "ArrowLeft" ? -1 : ev.key === "ArrowRight" ? 1 : 0;
    if (!step) return;
    ev.preventDefault();
    ev.stopPropagation();
    if (which === "start") onChange(Math.max(0, Math.min(endIdx, startIdx + step)), endIdx);
    else onChange(startIdx, Math.min(segments.length - 1, Math.max(startIdx, endIdx + step)));
  };
  // A click on a sentence outside the clip stretches the clip to it.
  const onTrackClick = (i: number) => {
    if (i < startIdx) onChange(i, endIdx);
    else if (i > endIdx) onChange(startIdx, i);
  };

  const a = segments[s]?.start ?? 0;
  const b = segments[e]?.end ?? a;
  const dragIdx = drag ? (drag.which === "start" ? s : e) : null;
  const dragCue = dragIdx != null ? (zh?.[dragIdx]?.trim() || segments[dragIdx]?.text?.trim() || "") : "";
  const blocks = [];
  for (let i = win.w0; i <= win.w1; i++) {
    const seg = segments[i];
    const inside = i >= s && i <= e;
    blocks.push(
      <span
        key={i}
        className={`vclip-rb-seg${inside ? " in" : ""}`}
        style={{ left: `${pct(seg.start)}%`, width: `${Math.max(0.4, pct(seg.end) - pct(seg.start))}%` }}
        title={zh?.[i]?.trim() || seg.text}
        onClick={() => onTrackClick(i)}
      />,
    );
  }

  return (
    <div className="vclip-rangebar">
      <div className="vclip-rb-track" ref={trackRef}>
        {blocks}
        <div className="vclip-rb-sel" style={{ left: `${pct(a)}%`, width: `${pct(b) - pct(a)}%` }} />
        {(["start", "end"] as const).map((which) => (
          <button
            key={which}
            type="button"
            className={`vclip-rb-handle ${which}${drag?.which === which ? " drag" : ""}`}
            style={{ left: `${pct(which === "start" ? a : b)}%` }}
            aria-label={which === "start" ? t("拖动调整开头") : t("拖动调整结尾")}
            title={which === "start" ? t("拖动调整开头") : t("拖动调整结尾")}
            onPointerDown={onDown(which)}
            onPointerMove={onMove}
            onPointerUp={onUp}
            onPointerCancel={onUp}
            onKeyDown={onKey(which)}
          />
        ))}
      </div>
      <div className="vclip-rb-meta small muted num">
        <span>{clock(a)}</span>
        <span>{t("{n} 句", { n: e - s + 1 })}</span>
        <span>{clock(b)}</span>
      </div>
      {drag && dragCue && (
        <div className="vclip-rb-cue small" role="status">
          <span className="vclip-rb-cue-tag">{drag.which === "start" ? t("开头") : t("结尾")}</span>
          <span className="clamp1">{dragCue}</span>
        </div>
      )}
    </div>
  );
}
