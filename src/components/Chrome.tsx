import { useShallow } from "zustand/react/shallow";
import { useEffect, useMemo, useRef, useState } from "react";
import { BookMarked, ChevronDown, ChevronLeft, ChevronRight, CircleArrowUp, Compass, Download, Inbox, Link2, Loader2, PanelLeftClose, PanelLeftOpen, Pause, Play, Plus, RotateCcw, RotateCw, Search, Settings as Cog, X, ChartColumn, ListChecks } from "lucide-react";
import { api, errorText, type SearchResult } from "../lib/api";
import { useStore, type View } from "../store";
import { installUpdate, useUpdate } from "../lib/updater";
import { PICKS, deepPickFor, isSubscribed, pickName, sameFeed, showName } from "../lib/picks";
import { isChineseUi, kbd, t, tk, tr, tx, useLang } from "../lib/i18n";
import { scheduleAutoSync } from "../lib/sync";
import { clock } from "../lib/format";
import { useShowName } from "../lib/zh";
import { useJobs } from "../lib/jobs";
import { BiTitle, ZhText } from "./Bi";
import { useLayout } from "../lib/layout";
import { useOffline } from "../lib/offline";
import logoUrl from "../assets/logo.png";
import { showIcon } from "./Lists";
import { EqBars, SearchArt, useKonami, useTaps } from "./Illustrations";
import { useDueCount } from "../lib/words";
import { LoadingIcon, pressPlay } from "./Players";
import { useVideoDock } from "./VideoDock";

export function Sidebar() {
  const { shows, view, go, set, showAliases } = useStore(useShallow((s) => ({ shows: s.shows, view: s.view, go: s.go, set: s.set, showAliases: s.settings.showAliases })));
  useLang((s) => s.lang);
  // Eggs: five quick clicks on the logo make it dance; the Konami code switches the seek bar to tape.
  const [dance, setDance] = useState(false);
  const tapLogo = useTaps(5, () => { setDance(true); window.setTimeout(() => setDance(false), 3600); });
  useKonami(() => { const on = document.body.classList.toggle("tape-seek"); useStore.getState().notify(on ? t("磁带模式已开启") : t("磁带模式已关闭")); });
  const is = (v: View) => view.kind === v.kind && (v.kind !== "show" || (view.kind === "show" && view.id === v.id)) && (v.kind !== "channel" || (view.kind === "channel" && view.input === v.input));
  // A 精读 channel is listed once: hidden here when its show is already followed (either version).
  const showCurated = useStore((s) => s.settings.sideCurated);
  const curated = useMemo(
    () => (!showCurated ? [] : PICKS.filter((p) => p.deep && !isSubscribed(p, shows) && !shows.some((s) => deepPickFor(s.title)?.id === p.id))),
    [showCurated, shows],
  );
  const { sideOpen, put: putLayout, subsOpen, curatedOpen, subsAll } = useLayout(useShallow((l) => ({ sideOpen: l.sideOpen, put: l.put, subsOpen: l.subsOpen, curatedOpen: l.curatedOpen, subsAll: l.subsAll })));
  const update = useUpdate(useShallow((s) => ({ phase: s.phase, version: s.version, progress: s.progress, quiet: s.quiet })));
  const rail = !sideOpen;
  // Folding applies to the full sidebar only: the icon rail has no headers to unfold it from.
  const showSubs = rail || subsOpen;
  const showCuratedList = rail || curatedOpen;
  // A long list shows its first SUBS_SHORT, plus the open show wherever it sits.
  const long = !rail && shows.length > SUBS_LONG;
  const listed = long && !subsAll ? shows.filter((s, i) => i < SUBS_SHORT || is({ kind: "show", id: s.id })) : shows;
  const unplayedAll = shows.reduce((n, s) => n + s.unplayed, 0);
  const hasDownloads = useOffline((s) => Object.keys(s.saved).length + Object.keys(s.active).length > 0);
  const wordsNav = useStore((s) => s.settings.learnOn && s.settings.learnSide);
  const due = useDueCount();
  // A render function, not a component: defined inside Sidebar, a component would remount its button on
  // every render and swallow clicks that land mid-render.
  const nav = (v: View, icon: React.ReactNode, label: string, badge?: number) => (
    <button key={v.kind} className={`nav ${is(v) ? "on" : ""}`} onClick={() => go(v)} title={rail ? (badge ? `${label} · ${badge}` : label) : undefined} aria-label={badge ? `${label} · ${badge}` : label}>
      <span className="nav-ic">{icon}{rail && badge ? <span className="rail-dot" aria-hidden /> : null}</span>
      <span className="label">{label}</span>
      {badge ? <span className="nav-badge">{badge > 99 ? "99+" : badge}</span> : null}
    </button>
  );
  return (
    <aside className={`sidebar ${rail ? "rail" : ""}`}>
      <div className="traffic-space" data-tauri-drag-region />
      <div className={`brand${dance ? " dance" : ""}`} data-tauri-drag-region onClick={tapLogo}>
        <img className="logo-img" src={logoUrl} alt="" />
        <p className="brand-name">SubCast</p>
        {dance && <EqBars className="brand-eq" />}
      </div>
      <nav>
        {nav({ kind: "latest" }, <Inbox size={16} />, t("首页"))}
        {nav({ kind: "discover" }, <Compass size={16} />, t("发现"))}
        {nav({ kind: "stats" }, <ChartColumn size={16} />, t("收听统计"))}
        {(hasDownloads || view.kind === "downloads") && nav({ kind: "downloads" }, <Download size={16} />, t("已下载"))}
        {(wordsNav || view.kind === "words") && nav({ kind: "words" }, <BookMarked size={16} />, t("生词本"), due)}
      </nav>
      <div className="side-sep" />
      <div className="side-head">
        <SideFold open={subsOpen} label={tx("sidebar", "订阅")} count={unplayedAll} onToggle={() => putLayout({ subsOpen: !subsOpen })} />
        <span className="side-head-actions">
          {shows.length > 1 && <button className="icon-btn sm side-manage" title={t("管理订阅")} aria-label={t("管理订阅")} onClick={() => set({ manageOpen: true })}><ListChecks size={14} /></button>}
          <button className="icon-btn sm" title={t("添加节目")} aria-label={t("添加节目")} onClick={() => set({ addOpen: true, addTab: "search" })}><Plus size={14} /></button>
        </span>
      </div>
      <div className="show-list">
        {showSubs && listed.map((s) => (
          <button key={s.id} className={`show-item ${is({ kind: "show", id: s.id }) ? "on" : ""}`} onClick={() => go({ kind: "show", id: s.id })} title={showAliases?.[s.id]?.trim() || (rail ? showName(s.title) : s.title)}>
            <span className="show-cover">{showIcon(s)}{rail && s.unplayed > 0 && <span className="rail-dot" />}</span>
            <SideName id={s.id} title={s.title} />
            {s.unplayed > 0 && <span className="unplayed">{s.unplayed > 99 ? "99+" : s.unplayed}</span>}
          </button>
        ))}
        {showSubs && long && <button type="button" className="side-more" aria-expanded={subsAll} onClick={() => putLayout({ subsAll: !subsAll })}>{subsAll ? t("收起") : t("显示全部（{n}）", { n: shows.length })}</button>}
        {!shows.length && <p className="muted small side-empty">{t("还没有订阅")}</p>}
        {curated.length > 0 && <>
          <div className="side-head second"><SideFold open={curatedOpen} label={t("乔木精读")} count={0} onToggle={() => putLayout({ curatedOpen: !curatedOpen })} /></div>
          {showCuratedList && curated.map((p) => (
            <button key={p.id} className={`show-item curated ${is({ kind: "channel", input: p.input }) ? "on" : ""}`} onClick={() => go({ kind: "channel", input: p.input })} title={t("{name} · 点击查看，决定是否订阅", { name: pickName(p) })}>
              <span className="show-cover"><img src={p.cover} alt="" loading="lazy" /></span>
              <span className="show-names"><span className="show-title">{pickName(p)}</span>{isChineseUi() && p.zh !== p.title && <span className="show-orig">{p.title}</span>}</span>
            </button>
          ))}
        </>}
      </div>
      <TaskChip />
      <div className="side-foot">
        <button className={`nav settings-btn ${view.kind === "settings" ? "on" : ""}`} onClick={() => useStore.getState().openSettings()} title={rail ? t("设置") : undefined} aria-label={t("设置")}><span className="cog-wrap"><Cog size={16} /></span><span className="label">{t("设置")}</span></button>
        <UpgradeButton {...update} />
        <button className="icon-btn side-toggle" onClick={() => putLayout({ sideOpen: rail })} title={rail ? t("展开侧栏（⌘B）") : t("收起为图标（⌘B）")} aria-label={rail ? t("展开侧栏") : t("收起侧栏")}>{rail ? <PanelLeftOpen size={16} /> : <PanelLeftClose size={16} />}</button>
      </div>
    </aside>
  );
}

const SUBS_LONG = 12, SUBS_SHORT = 8;

/** A sidebar group's header: click to fold the group; folded, it shows the group's unplayed total. */
function SideFold({ open, label, count, onToggle }: { open: boolean; label: string; count: number; onToggle: () => void }) {
  return (
    <button type="button" className="side-fold label" aria-expanded={open} title={open ? t("折叠") : t("展开")} onClick={onToggle}>
      <ChevronDown size={12} className={`side-fold-ic ${open ? "" : "shut"}`} aria-hidden />
      <span>{label}</span>
      {!open && count > 0 && <span className="side-fold-count">{count > 99 ? "99+" : count}</span>}
    </button>
  );
}

const PLACEHOLDERS = [tk("粘贴 YouTube / 小宇宙 / 播客链接，或搜索节目名"), tk("试试搜 Lex Fridman"), tk("粘贴一个 YouTube 视频链接，直接看双语字幕")];
const placeholder = PLACEHOLDERS[Math.floor(Math.random() * PLACEHOLDERS.length)];
export const looksLikeLink = (t: string) => /^(https?:\/\/|@)|youtube\.com|youtu\.be|xiaoyuzhoufm\.com|feed\.xyzfm\.space|podcasts\.apple\.com|\.(xml|rss)\b/i.test(t.trim());

/** A failed remote cover must become the letter tile, not WebKit's broken-image question mark. */
function SearchCover({ title, cover }: { title: string; cover?: string }) {
  const [failed, setFailed] = useState(false);
  useEffect(() => { setFailed(false); }, [cover]);
  if (!cover || failed) return <div className="cover-fallback">{title.slice(0, 1)}</div>;
  return <img src={cover} alt="" onError={() => setFailed(true)} />;
}

/** One search hit: the row opens the show's page (latest episodes, subscribe there); the button subscribes at once. */
export function SearchRow({ r, done, adding, onOpen, onSubscribe, active, onHover }: { r: SearchResult; done: boolean; adding: string | null; onOpen: () => void; onSubscribe: () => void; active?: boolean; onHover?: () => void }) {
  return (
    <div className={`drop-row clickable ${active ? "is-active" : ""}`} role="button" tabIndex={0} onMouseEnter={onHover} onMouseDown={(e) => e.preventDefault()} onClick={onOpen} onKeyDown={(e) => { if (e.key === "Enter") onOpen(); }}>
      <SearchCover title={r.title} cover={r.cover} />
      <div className="drop-meta">
        <BiTitle text={r.title} className="ep-name" subClassName="clamp1" />
        <p className="meta-line clamp1">{r.source === "youtube" && <span className="src-badge">YouTube</span>}{r.source === "youtube" && r.author === "YouTube" ? null : r.author}</p>
      </div>
      {done ? <span className="sub-done">{t("已订阅")}</span> : <button type="button" className="btn sm" onClick={(e) => { e.stopPropagation(); onSubscribe(); }} disabled={adding != null}>{adding === r.feedUrl ? <Loader2 size={13} className="spin" /> : <Plus size={13} />} {t("订阅")}</button>}
    </div>
  );
}

/** The bar above every page: back/forward history, search box that subscribes to pasted links or searches by name. */
export function TopBar() {
  const { history, future, back, forward, reloadShows, go, openEpisode, notify, shows } = useStore(useShallow((s) => ({
    history: s.history,
    future: s.future,
    back: s.back,
    forward: s.forward,
    reloadShows: s.reloadShows,
    go: s.go,
    openEpisode: s.openEpisode,
    notify: s.notify,
    shows: s.shows,
  })));
  const [q, setQ] = useState("");
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [results, setResults] = useState<SearchResult[] | null>(null);
  const [adding, setAdding] = useState<string | null>(null);
  const [activeIdx, setActiveIdx] = useState(-1);
  const timer = useRef<number | undefined>(undefined);
  const reqSeq = useRef(0);
  const wrapRef = useRef<HTMLDivElement>(null);
  const dropRef = useRef<HTMLDivElement>(null);
  const link = looksLikeLink(q);
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => { if (!wrapRef.current?.contains(e.target as Node)) setOpen(false); };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }, [open]);
  useEffect(() => {
    window.clearTimeout(timer.current);
    setActiveIdx(-1);
    if (!q.trim() || link) { reqSeq.current++; setBusy(false); setResults(null); return; }
    const seq = ++reqSeq.current;
    timer.current = window.setTimeout(async () => {
      setBusy(true);
      try {
        const list = (await api.searchPodcasts(q.trim(), /[\u4e00-\u9fff]/.test(q) ? "CN" : "US")).slice(0, 8);
        if (seq === reqSeq.current) setResults(list);
      } catch {
        if (seq === reqSeq.current) setResults([]);
      } finally {
        if (seq === reqSeq.current) setBusy(false);
      }
    }, 300);
  }, [q, link]);
  useEffect(() => {
    if (activeIdx >= 0) dropRef.current?.querySelector(".drop-row.is-active")?.scrollIntoView({ block: "nearest" });
  }, [activeIdx]);
  const subscribe = async (input: string) => {
    setAdding(input);
    try {
      const r = await api.addSubscription(input);
      setQ(""); setOpen(false); setResults(null); setActiveIdx(-1);
      if (r.show) { await reloadShows(); scheduleAutoSync(); notify(t("已订阅「{name}」", { name: r.show.title })); go({ kind: "show", id: r.show.id }); }
      else if (r.episode) openEpisode(r.episode);
    } catch (e) { notify(errorText(e)); } finally { setAdding(null); }
  };
  const openShow = (input: string) => {
    setQ(""); setOpen(false); setResults(null); setActiveIdx(-1);
    const existing = shows.find((s) => sameFeed(s.feedUrl, input));
    go(existing ? { kind: "show", id: existing.id } : { kind: "channel", input });
  };
  // Enter opens the highlighted (or first) result's page; a pasted link is subscribed at once.
  const submit = () => {
    if (link) { void subscribe(q.trim()); return; }
    const pick = results?.[activeIdx >= 0 ? activeIdx : 0];
    if (pick) openShow(pick.feedUrl);
  };
  return (
    <div className="topbar" data-tauri-drag-region>
      <div className="nav-history">
        <button className="round-btn" onClick={back} disabled={!history.length} title={t("返回（⌘[）")} aria-label={t("返回")}><ChevronLeft size={17} /></button>
        <button className="round-btn" onClick={forward} disabled={!future.length} title={t("前进（⌘]）")} aria-label={t("前进")}><ChevronRight size={17} /></button>
      </div>
      <div className="search-wrap" ref={wrapRef}>
        <div className={`top-search ${open ? "focus" : ""}`}>
          {link ? <Link2 size={16} /> : <Search size={16} />}
          <input value={q} placeholder={t(placeholder)} spellCheck={false} autoCapitalize="off" autoCorrect="off"
            onFocus={() => setOpen(true)}
            onChange={(e) => setQ(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "ArrowDown" && open && results?.length) { e.preventDefault(); setActiveIdx((i) => (i + 1) % results.length); return; }
              if (e.key === "ArrowUp" && open && results?.length) { e.preventDefault(); setActiveIdx((i) => (i <= 0 ? results.length - 1 : i - 1)); return; }
              if (e.key === "Enter") submit();
              if (e.key === "Escape") { setQ(""); setResults(null); setActiveIdx(-1); setOpen(false); (e.target as HTMLInputElement).blur(); }
            }} />
          {busy || adding ? <Loader2 size={14} className="spin" /> : (
            <>
              {q && <button type="button" className="icon-btn sm search-clear" title={t("清除")} aria-label={t("清除")} onMouseDown={(e) => e.preventDefault()} onClick={() => { setQ(""); setResults(null); setActiveIdx(-1); }}><X size={13} /></button>}
              {link ? <button type="button" className="btn primary sm top-add-btn" title={t("回车订阅")} onMouseDown={(e) => e.preventDefault()} onClick={() => void subscribe(q.trim())}>{t("添加")}</button> : !q && !open && <kbd className="kbd-hint" title={t("按 ⌘L 搜索")}>{kbd("⌘L")}</kbd>}
            </>
          )}
        </div>
        {open && results && (
          <div className="search-drop" ref={dropRef}>
            {results.length === 0 ? <div className="drop-empty"><SearchArt /><p className="muted small">{t("没有找到，换个关键词试试")}</p></div> : results.map((r, i) => (
              <SearchRow key={r.feedUrl} r={r} active={i === activeIdx} onHover={() => setActiveIdx(i)} done={shows.some((s) => sameFeed(s.feedUrl, r.feedUrl))} adding={adding} onOpen={() => openShow(r.feedUrl)} onSubscribe={() => void subscribe(r.feedUrl)} />
            ))}
          </div>
        )}
      </div>
      <div className="spacer" data-tauri-drag-region />
    </div>
  );
}

function SideName({ id, title }: { id: string; title: string }) {
  const { zh, orig } = useShowName(title, id);
  return (
    <span className="show-names">
      <span className="show-title">{zh ?? orig}</span>
      {zh && <span className="show-orig">{orig}</span>}
    </span>
  );
}

/** A running transcription anywhere in the app, with a way back to its episode. */
function TaskChip() {
  const asr = useJobs((s) => s.asr);
  const go = useStore((s) => s.go);
  const all = Object.entries(asr).filter(([, j]) => j.stage === "download" || j.stage === "transcribe");
  if (!all.length) return null;
  const [id, job] = all[0];
  const label = `${job.stage === "download" ? (job.message ? tr(job.message) : t("正在准备")) : t("正在生成字幕")} · ${Math.round(job.progress * 100)}%${all.length > 1 ? ` (+${all.length - 1})` : ""}`;
  return (
    <button className="task-chip" title={label} aria-label={label} onClick={() => go({ kind: "episode", id })}>
      <Loader2 size={13} className="spin" /> {label}
    </button>
  );
}

export function MiniPlayer() {
  const loading = useStore((s) => s.loadingSince);
  const floatingId = useVideoDock((d) => d.floating?.ep.id);
  const { current, view, playing, player, time, length, openEpisode, skipBack, skipForward } = useStore(useShallow((s) => ({
    current: s.current,
    view: s.view,
    playing: s.playing,
    player: s.player,
    time: Math.floor(s.time),
    length: s.length,
    openEpisode: s.openEpisode,
    skipBack: s.settings.skipBack ?? 15,
    skipForward: s.settings.skipForward ?? 30,
  })));
  const { panel, panelOpen } = useLayout(useShallow((l) => ({ panel: l.panel, panelOpen: l.panelOpen })));
  const [dragRatio, setDragRatio] = useState<number | null>(null);
  if (!current || (view.kind === "episode" && view.id === current.id) || (current.videoId && floatingId === current.id)) return null;
  // Left-aligned with the page content; on an episode page it stays within the middle column.
  const right = view.kind === "episode" && panelOpen ? panel + 24 : 24;
  const ratioFrom = (el: HTMLDivElement, clientX: number) => {
    const rect = el.getBoundingClientRect();
    return Math.min(1, Math.max(0, (clientX - rect.left) / Math.max(1, rect.width)));
  };
  const onSeekDown = (e: React.PointerEvent<HTMLDivElement>) => {
    e.stopPropagation();
    if (!player || !length) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    setDragRatio(ratioFrom(e.currentTarget, e.clientX));
  };
  const onSeekMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (dragRatio == null) return;
    e.stopPropagation();
    setDragRatio(ratioFrom(e.currentTarget, e.clientX));
  };
  const onSeekUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (dragRatio == null) return;
    e.stopPropagation();
    const r = ratioFrom(e.currentTarget, e.clientX);
    setDragRatio(null);
    if (player && length) player.seek(r * length);
  };
  const shownTime = dragRatio != null && length ? Math.round(dragRatio * length) : time;
  const shownPct = dragRatio != null ? dragRatio * 100 : length ? Math.min(100, (time / length) * 100) : 0;
  const dismiss = () => {
    const s = useStore.getState();
    if (s.playing) s.player?.toggle();
    s.set({ current: null, playing: false, player: null, loadingSince: null });
  };
  return (
    <div className="mini" style={{ "--mini-right": `${right}px` } as React.CSSProperties}>
      <button className={`play-btn sm ${loading ? "loading" : ""}`} onClick={() => { if (current.videoId && !player) { openEpisode(current); } else { pressPlay(); } }} disabled={!player && !loading && !current.videoId} aria-busy={!!loading} title={loading ? t("正在加载…（点击取消）") : undefined} aria-label={loading ? t("正在加载…（点击取消）") : playing ? t("暂停") : t("播放")}>
        {loading ? <LoadingIcon size={16} /> : playing ? <Pause size={16} fill="currentColor" /> : <Play size={16} fill="currentColor" />}
      </button>
      {current.cover && <img className="mini-cover" src={current.cover} alt="" />}
      <div className="mini-meta" role="button" tabIndex={0} onClick={() => openEpisode(current)} onKeyDown={(e) => e.key === "Enter" && openEpisode(current)}>
        <p className="ep-name"><ZhText text={current.title} /></p>
        <div className="mini-sub">
          {current.showTitle && current.showId !== "single" && <span className="meta-line clamp1">{showName(current.showTitle)}</span>}
          <span className="mini-time">{clock(shownTime)}{length ? ` / ${clock(length)}` : ""}</span>
        </div>
        <div
          className={`mini-seek${dragRatio != null ? " dragging" : ""}`}
          role="slider"
          aria-label={t("播放进度")}
          aria-valuemin={0}
          aria-valuemax={Math.max(1, Math.round(length))}
          aria-valuenow={shownTime}
          title={t("点击或拖动跳转进度")}
          onClick={(e) => e.stopPropagation()}
          onPointerDown={onSeekDown}
          onPointerMove={onSeekMove}
          onPointerUp={onSeekUp}
          onPointerCancel={() => setDragRatio(null)}
        >
          <div className="bar"><div style={{ width: `${shownPct}%` }} /></div>
        </div>
      </div>
      <div className="mini-skips">
        <button className="icon-btn sm" disabled={!player} title={t("后退 {n} 秒（←）", { n: skipBack })} aria-label={t("后退 {n} 秒", { n: skipBack })} onClick={() => player?.seek(Math.max(0, time - skipBack))}><RotateCcw size={14} /></button>
        <button className="icon-btn sm" disabled={!player} title={t("前进 {n} 秒（→）", { n: skipForward })} aria-label={t("前进 {n} 秒", { n: skipForward })} onClick={() => player?.seek(time + skipForward)}><RotateCw size={14} /></button>
        <button className="icon-btn sm mini-close" title={t("关闭")} aria-label={t("关闭")} onClick={dismiss}><X size={14} /></button>
      </div>
    </div>
  );
}

/** Next to Settings once a new version is downloaded (or waiting to be): one click installs it and restarts. */
function UpgradeButton({ phase, version, progress, quiet }: { phase: string; version?: string; progress: number | null; quiet?: boolean }) {
  if (phase === "installing" || (phase === "downloading" && !quiet)) {
    return <span className="icon-btn upgrade-btn busy" title={phase === "installing" ? t("正在安装新版本…") : progress == null ? t("正在下载新版本…") : t("正在下载新版本 {pct}%", { pct: Math.round(progress * 100) })}><Loader2 size={16} className="spin" /></span>;
  }
  if (phase !== "ready" && phase !== "available") return null;
  const label = phase === "ready" ? t("SubCast {version} 已下载好，点一下安装并重启", { version: version ?? "" }) : t("SubCast {version} 可以更新了，点一下下载、安装并重启", { version: version ?? "" });
  return <button className="icon-btn upgrade-btn" onClick={() => void installUpdate()} title={label} aria-label={label}><CircleArrowUp size={17} /></button>;
}

export function Modal({ title, onClose, children, wide, head }: { title: string; onClose: () => void; children: React.ReactNode; wide?: boolean; head?: React.ReactNode }) {
  useEffect(() => {
    const k = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [onClose]);
  return (
    <div className="modal-back" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className={`modal ${wide ? "wide" : ""}`} role="dialog" aria-modal="true" aria-label={title}>
        <div className="modal-head">{head ?? <h3>{title}</h3>}<button className="icon-btn" onClick={onClose} aria-label={t("关闭")}><X size={18} /></button></div>
        <div className="modal-body">{children}</div>
      </div>
    </div>
  );
}

export function Toast() {
  const { toast, action } = useStore(useShallow((s) => ({ toast: s.toast, action: s.toastAction })));
  if (!toast) return null;
  return (
    <div className={`toast${action ? " has-action" : ""}`} role="status">
      <span>{toast}</span>
      {action && <button type="button" className="toast-action" onClick={() => { action.run(); useStore.setState({ toast: null, toastAction: null }); }}>{action.label}</button>}
    </div>
  );
}
