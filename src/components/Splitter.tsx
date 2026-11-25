import { useRef } from "react";
import { t } from "../lib/i18n";

/** A draggable column edge. `edge` says which side of the resized column it sits on: dragging toward the column
 *  shrinks it. Past `snap` it collapses; double-click restores the default; arrow keys nudge by 16px. */
export function Splitter({ value, min, max, def, snap, edge, onChange, onCollapse, label }: {
  value: number; min: number; max: number; def: number; snap: number; edge: "right" | "left";
  onChange: (w: number) => void; onCollapse: () => void; label: string;
}) {
  const start = useRef<{ x: number; w: number } | null>(null);
  const clamp = (w: number) => Math.round(Math.min(max, Math.max(min, w)));
  const widthAt = (x: number) => { const s = start.current!; const d = x - s.x; return edge === "right" ? s.w + d : s.w - d; };
  return (
    <div
      className={`splitter ${edge}`}
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuemin={min}
      aria-valuemax={max}
      aria-valuenow={value}
      tabIndex={0}
      title={t("拖动调整宽度 · 双击恢复默认")}
      onPointerDown={(e) => {
        e.preventDefault();
        (e.target as HTMLElement).setPointerCapture(e.pointerId);
        start.current = { x: e.clientX, w: value };
        document.body.classList.add("resizing");
      }}
      onPointerMove={(e) => {
        if (!start.current) return;
        const w = widthAt(e.clientX);
        if (w < snap) return; // collapse happens on release, so the column doesn't flicker mid-drag
        onChange(clamp(w));
      }}
      onPointerUp={(e) => {
        if (!start.current) return;
        const w = widthAt(e.clientX);
        start.current = null;
        document.body.classList.remove("resizing");
        if (w < snap) onCollapse();
      }}
      onPointerCancel={() => { start.current = null; document.body.classList.remove("resizing"); }}
      onLostPointerCapture={() => { start.current = null; document.body.classList.remove("resizing"); }}
      onDoubleClick={() => onChange(def)}
      onKeyDown={(e) => {
        const grow = edge === "right" ? "ArrowRight" : "ArrowLeft";
        const shrink = edge === "right" ? "ArrowLeft" : "ArrowRight";
        if (e.key === grow || e.key === shrink) { e.preventDefault(); e.stopPropagation(); onChange(clamp(value + (e.key === grow ? 16 : -16))); }
        else if (e.key === "Enter") { e.preventDefault(); e.stopPropagation(); onChange(def); }
      }}
    />
  );
}
