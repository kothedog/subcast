// 收听统计: what the listening says about the listener, from the time actually spent playing (seeks don't count).
// Everything is computed on this computer from the listens table. The one exception is the optional AI portrait,
// which sends a short summary (show names, episode titles, hours) to the model the listener picked, and says so.
import { useEffect, useMemo, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { Loader2, RefreshCw, Sparkles, Square } from "lucide-react";
import { api, errorText, llmChat, type ListenRow } from "../lib/api";
import { providerFor, useStore } from "../store";
import { isChineseUi, t, tk, useLang } from "../lib/i18n";
import { PICKS, showName } from "../lib/picks";
import { llmReady } from "../lib/jobs";
import { Markdown } from "./Markdown";
import { EmptyState, HeadphonesArt } from "./Illustrations";

const DAY = 86_400_000;
const ymd = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
const cjk = (s: string) => /[一-鿿]/.test(s);

/** "12 小时 30 分" / "45 分钟" / "38 秒". */
function span(seconds: number): string {
  const m = Math.round(seconds / 60);
  if (seconds < 60) return t("{n} 秒", { n: Math.round(seconds) });
  if (m < 60) return t("{n} 分钟", { n: m });
  const h = Math.floor(m / 60), r = m % 60;
  return r ? t("{h} 小时 {m} 分", { h, m: r }) : t("{h} 小时", { h });
}
const dayLabel = (day: string) => { const d = new Date(`${day}T00:00:00`); return isChineseUi() ? `${d.getMonth() + 1} 月 ${d.getDate()} 日` : d.toLocaleDateString(undefined, { month: "short", day: "numeric" }); }; // i18n-ignore

type Range = "week" | "month" | "year" | "all";
const RANGES: [Range, string, number][] = [["week", tk("近 7 天"), 7], ["month", tk("近 30 天"), 30], ["year", tk("近一年"), 365], ["all", tk("全部"), 3650]];

/** Time made tangible. Durations are rounded public figures; the tooltip says which. */
const EQUIV: { secs: number; one: string; many: string; basis: string }[] = [
  { secs: 13 * 3600, one: tk("一次北京直飞纽约"), many: tk("{n} 次北京直飞纽约"), basis: tk("按单程约 13 小时") },
  { secs: 4.5 * 3600, one: tk("一趟京沪高铁"), many: tk("{n} 趟京沪高铁"), basis: tk("按单程约 4.5 小时") },
  { secs: 2 * 3600, one: tk("一部电影"), many: tk("{n} 部电影"), basis: tk("按每部约 2 小时") },
  { secs: 4 * 60, one: tk("一首歌"), many: tk("{n} 首歌"), basis: tk("按每首约 4 分钟") },
];
function equivalent(total: number) {
  const e = EQUIV.find((x) => total >= x.secs) ?? EQUIV[EQUIV.length - 1];
  const n = Math.round((total / e.secs) * 10) / 10;
  return { text: n < 1.15 ? t(e.one) : t(e.many, { n: n >= 10 ? Math.round(n) : n }), basis: t(e.basis) };
}

/** Listening clock personas, by where the busiest three-hour window falls. */
const PERSONAS: { from: number; name: string; line: string }[] = [
  { from: 0, name: tk("深夜守灯人"), line: tk("世界睡着的时候，你在听别人讲话。") },
  { from: 5, name: tk("晨读者"), line: tk("一天还没开始，你已经先听了一段。") },
  { from: 9, name: tk("上午派"), line: tk("你把最清醒的时间留给了耳朵。") },
  { from: 12, name: tk("午后漫步者"), line: tk("午饭之后，是你和节目的约会。") },
  { from: 17, name: tk("归途听客"), line: tk("天色暗下来，你在回家的路上听完一集。") },
  { from: 20, name: tk("夜航者"), line: tk("夜里安静，最适合把一期长对谈听完。") },
];
const personaOf = (h: number) => [...PERSONAS].reverse().find((p) => h >= p.from)!;

interface ShowStat { id: string; epId?: string; title: string; cover: string; seconds: number; episodes: Set<string>; done: number; months: Set<string>; first: string; single: boolean }

export function StatsView() {
  const { go, shows: subscribed } = useStore(useShallow((s) => ({ go: s.go, shows: s.shows })));
  useLang((s) => s.lang);
  const [rows, setRows] = useState<ListenRow[] | null>(null);
  const [range, setRange] = useState<Range>("month");
  useEffect(() => { void api.listeningStats(ymd(new Date(Date.now() - 3650 * DAY))).then(setRows).catch(() => setRows([])); }, []);

  const s = useMemo(() => {
    if (!rows) return null;
    const days = RANGES.find((r) => r[0] === range)![2];
    const since = ymd(new Date(Date.now() - (days - 1) * DAY));
    const inRange = rows.filter((r) => r.day >= since);
    const total = inRange.reduce((n, r) => n + r.seconds, 0);
    const translated = inRange.filter((r) => r.translated).reduce((n, r) => n + r.seconds, 0);

    // Per episode: time spent, and whether it was finished (played to 90% by now).
    const eps = new Map<string, { title: string; show: string; secs: number; duration: number; progress: number; published: number; firstDay: string; lastHour: number; lastDay: string }>();
    for (const r of inRange) {
      const e = eps.get(r.episodeId) ?? { title: r.episodeTitle, show: r.showTitle, secs: 0, duration: r.duration, progress: r.progress, published: r.published, firstDay: r.day, lastHour: r.hour, lastDay: r.day };
      e.secs += r.seconds; if (r.day < e.firstDay) e.firstDay = r.day;
      eps.set(r.episodeId, e);
    }
    const heard = [...eps.values()].filter((e) => e.secs > 60);
    const finished = heard.filter((e) => e.duration > 0 && e.progress >= e.duration * 0.9);

    // Streaks over everything (a day counts with 5 minutes or more).
    const byDay = new Map<string, number>();
    for (const r of rows) byDay.set(r.day, (byDay.get(r.day) ?? 0) + r.seconds);
    let streak = 0;
    for (let d = new Date(), first = true; ; d = new Date(d.getTime() - DAY), first = false) {
      if ((byDay.get(ymd(d)) ?? 0) >= 300) streak++;
      else if (!first) break;
      if (streak > 3650) break;
    }
    let best = 0, run = 0, prev = "";
    for (const day of [...byDay.keys()].filter((d) => (byDay.get(d) ?? 0) >= 300).sort()) {
      run = prev && ymd(new Date(new Date(`${prev}T00:00:00`).getTime() + DAY)) === day ? run + 1 : 1;
      best = Math.max(best, run); prev = day;
    }

    // Shows. Episodes opened on their own are listed by their own title, not lumped together.
    const shows = new Map<string, ShowStat>();
    const firstEver = new Map<string, string>();
    for (const r of rows) { const k = r.showId; if (!firstEver.has(k) || r.day < firstEver.get(k)!) firstEver.set(k, r.day); }
    for (const r of inRange) {
      const single = !r.showTitle || r.showId === "single";
      const key = single ? `ep:${r.episodeId}` : r.showId;
      const x = shows.get(key) ?? { id: single ? "" : r.showId, epId: single ? r.episodeId : undefined, title: single ? r.episodeTitle : r.showTitle, cover: r.cover, seconds: 0, episodes: new Set(), done: 0, months: new Set(), first: firstEver.get(r.showId) ?? r.day, single };
      x.seconds += r.seconds; x.episodes.add(r.episodeId); x.months.add(r.day.slice(0, 7));
      shows.set(key, x);
    }
    for (const x of shows.values()) x.done = [...x.episodes].filter((id) => finished.some((f) => f === eps.get(id))).length;
    const top = [...shows.values()].map((x) => ({ ...x, cover: x.cover || subscribed.find((s) => s.id === x.id)?.cover || "" })).sort((a, b) => b.seconds - a.seconds).slice(0, 6);

    // Topics, from the curated shows' tags; the rest is 「其他」.
    const topics = new Map<string, number>();
    for (const r of inRange) {
      const pick = PICKS.find((p) => p.title === r.showTitle || p.channel === r.showTitle || p.zh === r.showTitle || (!!r.showTitle && p.title.toLowerCase().startsWith(r.showTitle.toLowerCase().slice(0, 12))));
      const topic = pick?.topics.find((x) => x !== "乔木精读") ?? tk("其他"); // i18n-ignore (topic ids)
      topics.set(topic, (topics.get(topic) ?? 0) + r.seconds);
    }

    // The clock: seconds per hour, the busiest three-hour window, weekday vs weekend.
    const hours = Array(24).fill(0) as number[];
    let weekday = 0, weekend = 0;
    const weekdayDays = new Set<string>(), weekendDays = new Set<string>();
    for (const r of inRange) {
      hours[r.hour] += r.seconds;
      const wd = new Date(`${r.day}T00:00:00`).getDay();
      if (wd === 0 || wd === 6) { weekend += r.seconds; weekendDays.add(r.day); } else { weekday += r.seconds; weekdayDays.add(r.day); }
    }
    let peak = 0;
    for (let h = 0; h < 24; h++) if (hours[h] + hours[(h + 1) % 24] + hours[(h + 2) % 24] > hours[peak] + hours[(peak + 1) % 24] + hours[(peak + 2) % 24]) peak = h;

    // Ears: the share heard without Chinese subtitles, week by week (last 12 weeks, whatever the range).
    const weeks: { label: string; total: number; plain: number }[] = [];
    const monday = new Date(); monday.setHours(0, 0, 0, 0); monday.setDate(monday.getDate() - ((monday.getDay() + 6) % 7));
    for (let i = 11; i >= 0; i--) {
      const start = new Date(monday.getTime() - i * 7 * DAY), end = new Date(start.getTime() + 7 * DAY);
      const w = rows.filter((r) => r.day >= ymd(start) && r.day < ymd(end) && !cjk(r.episodeTitle));
      weeks.push({ label: `${start.getMonth() + 1}/${start.getDate()}`, total: w.reduce((n, r) => n + r.seconds, 0), plain: w.filter((r) => !r.translated).reduce((n, r) => n + r.seconds, 0) });
    }

    // Foreign-language time and the words it carried (about 150 English words a minute of conversation).
    const foreign = inRange.filter((r) => !cjk(r.episodeTitle)).reduce((n, r) => n + r.seconds, 0);

    // Small discoveries.
    const facts: { big: string; line: string }[] = [];
    const longest = [...new Set(inRange.map((r) => r.day))].map((d) => [d, inRange.filter((r) => r.day === d).reduce((n, r) => n + r.seconds, 0)] as const).sort((a, b) => b[1] - a[1])[0];
    if (longest && longest[1] >= 1200) facts.push({ big: span(longest[1]), line: t("{day}是你听得最久的一天。", { day: dayLabel(longest[0]) }) });
    const late = inRange.filter((r) => r.hour <= 4 && r.seconds > 120).sort((a, b) => b.hour - a.hour || b.day.localeCompare(a.day))[0];
    if (late) facts.push({ big: `${String(late.hour).padStart(2, "0")}:00`, line: t("{day}凌晨，你还在听《{title}》。", { day: dayLabel(late.day), title: clip(late.episodeTitle) }) });
    const again = heard.filter((e) => e.duration > 600 && e.secs > e.duration * 1.4).sort((a, b) => b.secs / b.duration - a.secs / a.duration)[0];
    if (again) facts.push({ big: t("{n} 遍", { n: Math.round((again.secs / again.duration) * 10) / 10 }), line: t("《{title}》你听了不止一遍。值得反复听的，往往是真的打动了你。", { title: clip(again.title) }) });
    const fresh = finished.filter((e) => e.published && new Date(`${e.firstDay}T00:00:00`).getTime() - e.published < 2 * DAY).length;
    if (fresh >= 2) facts.push({ big: t("{n} 集", { n: fresh }), line: t("新一期一出来，你两天内就听完了。你是这些节目最早的听众之一。") });
    const longestEp = heard.filter((e) => e.duration >= 5400 && e.progress >= e.duration * 0.9).sort((a, b) => b.duration - a.duration)[0];
    if (longestEp) facts.push({ big: span(longestEp.duration), line: t("你完整听完了《{title}》，这是这段时间里最长的一集。", { title: clip(longestEp.title) }) });
    if (best >= 3) facts.push({ big: t("{n} 天", { n: best }), line: t("你最长连续 {n} 天每天都听了至少 5 分钟。", { n: best }) });

    return { total, translated, heard: heard.length, finished: finished.length, streak, best, top, topics, hours, peak, weekday, weekend, weekdayDays: weekdayDays.size, weekendDays: weekendDays.size, weeks, foreign, facts: facts.slice(0, 4), byDay, inRange };
  }, [rows, range, subscribed]);

  if (!rows || !s) return <div className="page stats" />;
  if (!rows.length) {
    return (
      <div className="page stats">
        <h1 className="page-title">{t("收听统计")}</h1>
        <EmptyState art={<HeadphonesArt />} title={t("还没有收听记录")} note={t("从这一版开始，听过的每一分钟都会记在这里，只存在你的电脑上。")}>
          <button className="btn primary" onClick={() => go({ kind: "latest" })}>{t("去听一集")}</button>
        </EmptyState>
      </div>
    );
  }
  const rangeName = t(RANGES.find((r) => r[0] === range)![1]);
  const eq = equivalent(s.total);
  const words = Math.round((s.foreign / 60) * 150);
  const persona = personaOf(s.peak);
  const perWeekday = s.weekdayDays ? s.weekday / s.weekdayDays : 0, perWeekend = s.weekendDays ? s.weekend / s.weekendDays : 0;
  return (
    <div className="page stats">
      <div className="stats-head">
        <h1 className="page-title">{t("收听统计")}</h1>
        <div className="seg-control" role="radiogroup">
          {RANGES.map(([k, label]) => <button key={k} role="radio" aria-checked={range === k} className={range === k ? "on" : ""} onClick={() => setRange(k)}>{t(label)}</button>)}
        </div>
      </div>

      {s.total < 60 ? <p className="stats-lede">{t("{range}还没有收听。挑一集，从这里开始。", { range: rangeName })}</p> : (
        <section className="stats-lede-card">
          <p className="stats-lede">{t("{range}，你听了", { range: rangeName })} <b>{span(s.total)}</b>{t("，相当于")} <b title={eq.basis}>{eq.text}</b>{t("。")}</p>
          {words >= 1000 && <p className="stats-lede-sub" title={t("按对谈每分钟约 150 个英文单词估算")}>{t("你的耳朵大约接住了 {n} 个外语单词。", { n: words.toLocaleString() })}</p>}
          <div className="stat-tiles">
            <Tile label={t("听过的单集")} value={t("{n} 集", { n: s.heard })} sub={s.finished ? t("听完 {n} 集", { n: s.finished }) : undefined} />
            <Tile label={t("借助字幕")} value={`${Math.round((s.translated / Math.max(1, s.total)) * 100)}%`} sub={t("开着中文或双语字幕")} />
            <Tile label={t("连续收听")} value={t("{n} 天", { n: s.streak })} sub={s.best > s.streak ? t("最长 {n} 天", { n: s.best }) : t("每天至少 5 分钟")} />
            <Tile label={t("外语时间")} value={span(s.foreign)} sub={t("占 {n}%", { n: Math.round((s.foreign / Math.max(1, s.total)) * 100) })} />
          </div>
        </section>
      )}

      {s.total >= 60 && (
        <>
          <div className="stats-grid">
            <section className="stats-card">
              <div className="stats-card-h"><h2>{t("你把时间给了谁")}</h2></div>
              <ol className="top-shows">
                {s.top.map((x, i) => {
                  const badge = x.months.size >= 3 ? t("老朋友") : Date.now() - new Date(`${x.first}T00:00:00`).getTime() < 14 * DAY && !x.single ? t("新欢") : "";
                  const hasShow = !!x.id && subscribed.some((sub) => sub.id === x.id);
                  const openTarget = () => {
                    if (hasShow) go({ kind: "show", id: x.id });
                    else if (x.single && x.epId) go({ kind: "episode", id: x.epId });
                  };
                  const clickable = hasShow || (x.single && !!x.epId);
                  return (
                    <li key={x.id || x.epId || x.title} className={clickable ? undefined : "static"} role={clickable ? "button" : undefined} tabIndex={clickable ? 0 : undefined} onClick={clickable ? openTarget : undefined} onKeyDown={clickable ? (e) => e.key === "Enter" && openTarget() : undefined}>
                      <span className="top-rank num">{i + 1}</span>
                      {x.cover ? <img src={x.cover} alt="" loading="lazy" /> : <span className="top-ph" />}
                      <span className="top-name">
                        <span className="top-title"><span className="clamp1">{x.single ? x.title : isChineseUi() ? showName(x.title) : x.title}</span>{badge && <em className="top-badge">{badge}</em>}{x.single && <em className="top-badge plain">{t("单集")}</em>}</span>
                        <span className="top-bar"><span style={{ width: `${(x.seconds / s.top[0].seconds) * 100}%` }} /></span>
                        <span className="top-meta muted small">{t("{n} 集", { n: x.episodes.size })}{x.done ? ` · ${t("听完 {n}", { n: x.done })}` : ""}</span>
                      </span>
                      <span className="top-time small num">{span(x.seconds)}</span>
                    </li>
                  );
                })}
              </ol>
            </section>
            <section className="stats-card clock-card">
              <div className="stats-card-h"><h2>{t("你的收听生物钟")}</h2></div>
              <Clock hours={s.hours} peak={s.peak} />
              <p className="persona"><b>{t(persona.name)}</b></p>
              <p className="muted small persona-line">{t(persona.line)} {t("最常在 {from}–{to} 之间听。", { from: `${s.peak}:00`, to: `${(s.peak + 3) % 24}:00` })}</p>
              {perWeekday > 0 && perWeekend > 0 && <p className="muted small persona-line">{perWeekend > perWeekday * 1.15 ? t("周末平均每天比工作日多听 {n}%。", { n: Math.round((perWeekend / perWeekday - 1) * 100) }) : perWeekday > perWeekend * 1.15 ? t("工作日平均每天比周末多听 {n}%，节目是你日常的一部分。", { n: Math.round((perWeekday / perWeekend - 1) * 100) }) : t("工作日和周末听得差不多多。")}</p>}
            </section>
          </div>

          {s.facts.length > 0 && (
            <section className="facts">
              {s.facts.map((f, i) => <div key={i} className="fact"><b className="num">{f.big}</b><p>{f.line}</p></div>)}
            </section>
          )}

          <div className="stats-grid">
            <section className="stats-card">
              <div className="stats-card-h"><h2>{t("话题版图")}</h2><span className="muted small">{t("按节目归类")}</span></div>
              <Topics topics={s.topics} />
            </section>
            <section className="stats-card">
              <div className="stats-card-h"><h2>{t("耳朵的成长")}</h2><span className="muted small">{t("近 12 周")}</span></div>
              <Ears weeks={s.weeks} />
            </section>
          </div>

          <Portrait rows={s.inRange} range={range} rangeName={rangeName} />
        </>
      )}

      <section className="stats-card">
        <div className="stats-card-h"><h2>{t("一年的样子")}</h2><span className="muted small">{t("每一格是一天")}</span></div>
        <Heatmap byDay={s.byDay} />
      </section>
      <p className="muted small stats-foot">{t("只统计真正在播放的时间，拖动进度条不算。数据只保存在这台电脑上。")}</p>
    </div>
  );
}

const clip = (s: string) => (s.length > 40 ? `${s.slice(0, 38)}…` : s);

function Tile({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return <div className="stat-tile"><span className="muted small">{label}</span><b className="num">{value}</b>{sub && <span className="muted small">{sub}</span>}</div>;
}

/** 24 hours around a dial, midnight at the top; the busiest three hours are drawn in the accent colour. */
function Clock({ hours, peak }: { hours: number[]; peak: number }) {
  const max = Math.max(1, ...hours);
  const c = 110, r0 = 46, len = 54;
  const inPeak = (h: number) => (h - peak + 24) % 24 < 3;
  return (
    <svg className="clock" viewBox="0 0 220 220" role="img" aria-label={t("你的收听生物钟")}>
      <circle cx={c} cy={c} r={r0 - 6} className="clock-face" />
      {hours.map((v, h) => {
        const a = (h / 24) * Math.PI * 2 - Math.PI / 2 + Math.PI / 24;
        const l = v ? 6 + (len - 6) * Math.sqrt(v / max) : 3;
        return <line key={h} x1={c + Math.cos(a) * r0} y1={c + Math.sin(a) * r0} x2={c + Math.cos(a) * (r0 + l)} y2={c + Math.sin(a) * (r0 + l)} className={`clock-bar ${v ? "" : "zero"} ${inPeak(h) && v ? "peak" : ""}`}><title>{`${h}:00 · ${v ? span(v) : t("没有收听")}`}</title></line>;
      })}
      {[0, 6, 12, 18].map((h) => { const a = (h / 24) * Math.PI * 2 - Math.PI / 2; return <text key={h} x={c + Math.cos(a) * (r0 - 16)} y={c + Math.sin(a) * (r0 - 16) + 4} className="clock-label">{h}</text>; })}
    </svg>
  );
}

function Topics({ topics }: { topics: Map<string, number> }) {
  const list = [...topics.entries()].sort((a, b) => b[1] - a[1]);
  const total = list.reduce((n, [, v]) => n + v, 0) || 1;
  return (
    <div className="topic-chart">
      <div className="topic-bar" role="img" aria-label={t("话题版图")}>
        {list.map(([k, v], i) => {
          const pct = (v / total) * 100;
          return <span key={k} className={`topic-seg t${Math.min(i, 5)}`} style={{ width: `${pct}%` }} title={`${t(k)} · ${Math.round(pct)}% · ${span(v)}`} />;
        })}
      </div>
      <ul className="topic-legend">
        {list.map(([k, v], i) => {
          const pct = Math.round((v / total) * 100);
          return (
            <li key={k} className="topic-row" title={`${t(k)} · ${span(v)}`}>
              <span className={`topic-dot t${Math.min(i, 5)}`} aria-hidden />
              <span className="topic-name">{t(k)}</span>
              <span className="topic-span muted small num">{span(v)}</span>
              <span className="topic-pct num">{pct}%</span>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

/** Week by week, how much of the foreign-language listening was without Chinese subtitles. */
function Ears({ weeks }: { weeks: { label: string; total: number; plain: number }[] }) {
  const has = weeks.filter((w) => w.total > 300);
  if (has.length < 2) return <p className="muted small">{t("多听几周，这里会画出你不靠字幕的时间怎么变化。")}</p>;
  const share = (w: { total: number; plain: number }) => (w.total ? w.plain / w.total : 0);
  const first = share(has[0]), last = share(has[has.length - 1]);
  return (
    <>
      <div className="ears">
        {weeks.map((w, i) => <div key={i} className="ears-col" title={`${w.label} · ${w.total ? `${Math.round(share(w) * 100)}%` : t("没有收听")}`}><span className="ears-bar" style={{ height: `${Math.max(w.total ? 4 : 0, share(w) * 100)}%` }} /><span className="ears-x">{i % 3 === 0 ? w.label : ""}</span></div>)}
      </div>
      <p className="muted small ears-note">{last > first + 0.05 ? t("不开中文字幕听的比例，从 {a}% 升到了 {b}%。耳朵在悄悄变好。", { a: Math.round(first * 100), b: Math.round(last * 100) }) : t("柱子是每周不开中文字幕听外语节目的比例。字幕是拐杖，也是梯子。")}</p>
    </>
  );
}

/** Optional: the listener's model reads a short summary and writes a portrait. Cached per range until asked again. */
function Portrait({ rows, range, rangeName }: { rows: ListenRow[]; range: Range; rangeName: string }) {
  const settings = useStore((s) => s.settings);
  const provider = providerFor(settings, "chat");
  const [text, setText] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [ready, setReady] = useState<boolean | null>(null);
  const abortRef = useRef<AbortController | null>(null);
  useEffect(() => { void llmReady("chat").then(setReady); }, [settings.chatProvider, settings.chatModel, settings.providers]);
  useEffect(() => {
    abortRef.current?.abort();
    abortRef.current = null;
    setBusy(null);
    setText(null);
    setError("");
    let live = true;
    void api.getBlob("stats", `portrait-${range}`).then((v) => { if (live) setText(v || null); });
    return () => { live = false; };
  }, [range]);
  useEffect(() => () => { abortRef.current?.abort(); }, []);
  const brief = () => {
    const shows = new Map<string, number>(), titles = new Map<string, number>(), hours = Array(24).fill(0) as number[];
    for (const r of rows) { shows.set(r.showTitle || "单集", (shows.get(r.showTitle || "单集") ?? 0) + r.seconds); // i18n-ignore (sent to the model)
      titles.set(r.episodeTitle, (titles.get(r.episodeTitle) ?? 0) + r.seconds); hours[r.hour] += r.seconds; }
    const top = <K,>(m: Map<K, number>, n: number) => [...m.entries()].sort((a, b) => b[1] - a[1]).slice(0, n).map(([k, v]) => `${k}（${Math.round(v / 60)} 分钟）`); // i18n-ignore
    return `时间范围：${rangeName}\n节目：${top(shows, 8).join("；")}\n单集标题：${top(titles, 25).join("；")}\n每小时收听分钟：${hours.map((v) => Math.round(v / 60)).join(",")}`; // i18n-ignore
  };
  const run = async () => {
    if (!provider) return;
    abortRef.current?.abort();
    const ctl = new AbortController();
    abortRef.current = ctl;
    const forRange = range;
    setBusy(""); setError("");
    try {
      const out = await llmChat({ provider, maxTokens: 4000, signal: ctl.signal, onDelta: (d) => { if (!ctl.signal.aborted) setBusy(d); }, messages: [{ role: "user", content: brief() }],
        system: "你是一位细腻又严谨的播客编辑。根据用户的收听数据写一段中文收听画像，只依据给出的数据，不编造未出现的节目或事实。格式：第一行是一个 4–8 字的称号（加粗）；第二行是 3 个兴趣标签，用「·」分隔；然后一段 80–140 字的画像，有文学感但每句都能在数据里找到依据；最后一行以「下一步：」开头，建议一个他可能喜欢的新方向。不要使用标题符号。" }); // i18n-ignore
      if (ctl.signal.aborted) return;
      if (!out.trim()) throw new Error(t("模型没有返回内容，请重试或换一个模型"));
      setText(out); await api.setBlob("stats", `portrait-${forRange}`, out);
    } catch (e) { if (!ctl.signal.aborted) setError(errorText(e)); } finally { if (abortRef.current === ctl) { abortRef.current = null; setBusy(null); } }
  };
  const shown = busy ?? text;
  return (
    <section className="stats-card portrait">
      <div className="stats-card-h">
        <h2><Sparkles size={14} className="accent" /> {t("AI 收听画像")}</h2>
        {busy != null ? <button className="btn ghost sm" onClick={() => abortRef.current?.abort()}><Square size={12} fill="currentColor" /> {t("停止")}</button>
          : text && <button className="link-btn small" onClick={() => void run()}><RefreshCw size={12} /> {t("重新生成")}</button>}
      </div>
      {shown ? <div className="portrait-text"><Markdown text={shown} onSeek={() => {}} /></div>
        : ready === false ? <p className="muted small">{t("在设置里配好模型后，可以让 AI 读一读你的收听，写一段只属于你的画像。")} <button className="link-btn small" onClick={() => useStore.getState().openSettings("model")}>{t("去设置")}</button></p>
        : (
          <div className="portrait-cta">
            <p className="muted small">{t("会把节目名、单集标题和收听时段发给 {name}，生成一段画像。只在你点下面的按钮时发送。", { name: provider ? t(provider.name) : "" })}</p>
            <button className="btn primary sm" onClick={() => void run()} disabled={busy != null || !ready}>{busy != null ? <><Loader2 size={13} className="spin" /> {t("正在读你的收听…")}</> : t("生成我的收听画像")}</button>
          </div>
        )}
      {error && <p className="error-text small">{error}</p>}
    </section>
  );
}

/** A year of days, columns are weeks (Monday first), darker = longer; like a contributions calendar. */
function Heatmap({ byDay }: { byDay: Map<string, number> }) {
  const today = new Date();
  const start = new Date(today.getTime() - 364 * DAY);
  start.setDate(start.getDate() - ((start.getDay() + 6) % 7));
  const cells: { day: string; secs: number; future: boolean }[] = [];
  for (let d = new Date(start); d <= today || (d.getDay() + 6) % 7 !== 0; d = new Date(d.getTime() + DAY)) {
    cells.push({ day: ymd(d), secs: byDay.get(ymd(d)) ?? 0, future: d > today });
    if (cells.length > 380) break;
  }
  const level = (s: number) => (s <= 0 ? 0 : s < 900 ? 1 : s < 1800 ? 2 : s < 3600 ? 3 : 4);
  const months: { col: number; label: string }[] = [];
  cells.forEach((c, i) => { if (i % 7 === 0) { const d = new Date(`${c.day}T00:00:00`); if (d.getDate() <= 7) months.push({ col: i / 7, label: isChineseUi() ? `${d.getMonth() + 1}月` : d.toLocaleString(undefined, { month: "short" }) }); } }); // i18n-ignore
  return (
    <div className="heatmap-wrap">
      <div className="heatmap-months" style={{ gridTemplateColumns: `repeat(${Math.ceil(cells.length / 7)}, 1fr)` }}>
        {months.map((m) => <span key={m.col} style={{ gridColumn: `${m.col + 1} / span 4` }}>{m.label}</span>)}
      </div>
      <div className="heatmap" style={{ gridTemplateColumns: `repeat(${Math.ceil(cells.length / 7)}, 1fr)` }}>
        {cells.map((c) => <span key={c.day} className={`hm l${level(c.secs)} ${c.future ? "future" : ""}`} title={c.future ? "" : `${c.day} · ${c.secs ? span(c.secs) : t("没有收听")}`} />)}
      </div>
      <div className="heatmap-legend muted small">{t("少")}{[0, 1, 2, 3, 4].map((l) => <span key={l} className={`hm l${l}`} />)}{t("多")}</div>
    </div>
  );
}
