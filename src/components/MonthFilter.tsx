import { useMemo, useState } from "react";
import { CalendarDays, ChevronLeft, ChevronRight, X } from "lucide-react";
import type { Episode } from "../lib/api";
import { monthLabel, monthName } from "../lib/format";
import { t } from "../lib/i18n";

const key = (ms: number) => { const d = new Date(ms); return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}`; };

/** Month filter state for an episode list. */
export function useMonthFilter(eps: Episode[] | null) {
  const [month, setMonth] = useState<string | null>(null);
  const filtered = useMemo(() => (month && eps ? eps.filter((e) => e.published && key(e.published) === month) : eps), [eps, month]);
  return { month, setMonth, filtered };
}

/** A year of months, each showing how many episodes came out — a compact calendar that doubles as a heat map.
 *  Months with nothing are dimmed and can't be picked; clicking the picked month again clears it. */
export function MonthFilter({ episodes, month, onChange }: { episodes: Episode[] | null; month: string | null; onChange: (m: string | null) => void }) {
  const counts = useMemo(() => {
    const m = new Map<string, number>();
    for (const e of episodes ?? []) if (e.published) m.set(key(e.published), (m.get(key(e.published)) ?? 0) + 1);
    return m;
  }, [episodes]);
  const years = useMemo(() => [...new Set([...counts.keys()].map((k) => Number(k.slice(0, 4))))].sort((a, b) => b - a), [counts]);
  const [year, setYear] = useState<number | null>(null);
  const pickedYear = year != null && years.includes(year) ? year : null;
  const y = pickedYear ?? (month && years.includes(Number(month.slice(0, 4))) ? Number(month.slice(0, 4)) : years[0]) ?? new Date().getFullYear();
  if (!episodes?.length) return null;
  if (counts.size < 2) {
    if (!month) return null;
    return (
      <div className="month-filter" role="group" aria-label={t("按月份筛选")}>
        <div className="mf-head">
          <CalendarDays size={14} />
          <span className="mf-year">{month.slice(0, 4)}</span>
          <span className="spacer" />
          <button className="chip sm on" onClick={() => onChange(null)}>{monthName(Number(month.slice(5)), true)} <X size={11} /></button>
        </div>
      </div>
    );
  }
  const max = Math.max(...counts.values());
  const yearTotal = [...counts.entries()].filter(([k]) => k.startsWith(`${y}-`)).reduce((n, [, c]) => n + c, 0);
  const idx = years.indexOf(y);
  return (
    <div className="month-filter" role="group" aria-label={t("按月份筛选")}>
      <div className="mf-head">
        <CalendarDays size={14} />
        <button className="icon-btn sm" disabled={idx >= years.length - 1} onClick={() => setYear(years[idx + 1])} aria-label={t("上一年")}><ChevronLeft size={14} /></button>
        <span className="mf-year">{y}</span>
        <button className="icon-btn sm" disabled={idx <= 0} onClick={() => setYear(years[idx - 1])} aria-label={t("下一年")}><ChevronRight size={14} /></button>
        <span className="muted small">{t("{n} 期", { n: yearTotal })}</span>
        <span className="spacer" />
        {month && <button className="chip sm on" onClick={() => onChange(null)}>{monthName(Number(month.slice(5)), true)} <X size={11} /></button>}
      </div>
      <div className="mf-grid">
        {Array.from({ length: 12 }, (_, i) => {
          const k = `${y}-${String(i + 1).padStart(2, "0")}`;
          const n = counts.get(k) ?? 0;
          return (
            <button key={k} className={`mf-cell ${month === k ? "on" : ""} ${n / max > 0.6 ? "hot" : ""}`} disabled={!n} onClick={() => onChange(month === k ? null : k)}
              style={{ "--heat": n ? 0.18 + 0.6 * (n / max) : 0 } as React.CSSProperties} title={n ? t("{month} · {n} 期", { month: monthLabel(i + 1, y), n }) : t("{month}没有节目", { month: monthLabel(i + 1) })}>
              <span className="mf-m">{monthName(i + 1)}</span>
              <span className="mf-n">{n || "·"}</span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
