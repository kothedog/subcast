import { playSoon } from "./Players";
import { ask } from "../lib/confirm";
import { create } from "zustand";
import { useShallow } from "zustand/react/shallow";
import { memo, useEffect, useMemo, useRef, useState } from "react";
import { ArrowDownToLine, ArrowDownUp, BookOpen, Check, CheckCircle2, ChevronDown, ChevronRight, FolderOpen, ListVideo, Loader2, MoreHorizontal, Pencil, Play, Plus, RefreshCw, RotateCcw, Search, Trash2, X, Youtube } from "lucide-react";
import { bytes, clearDownloads, useOffline } from "../lib/offline";
import { api, errorText, type Episode, type Show, type ShowHit } from "../lib/api";
import { date, duration, monthLabel } from "../lib/format";
import { PICKS, TOPICS, deepPickFor, isSubscribed, pickName, pickSub, sameFeed, showName, subscribedShow, subscriptionVisible, type Pick, type Topic } from "../lib/picks";
import { isChineseUi, t, useLang } from "../lib/i18n";
import { scheduleAutoSync } from "../lib/sync";
import { HOME_SHOW_FEED, showLinks } from "../lib/showLinks";
import { AlsoOn, HomeShowBanner, YtButton, usePromo } from "./YtPromo";
import { useShowName, useZh } from "../lib/zh";
import { showAliasPatch } from "../lib/settings";
import { useStore } from "../store";
import { SkeletonFeed } from "./Skeleton";
import { EmptyState, FlatLineArt, HeadphonesArt, NightNote, TrayArt } from "./Illustrations";
import { MonthFilter, useMonthFilter } from "./MonthFilter";
import { clearQueue, playAll, useLibrary } from "../lib/library";
import { recommend, type Recommendation } from "../lib/recommend";
import { isInProgress, isWatched, markShowAllWatched, markWatched, useEpisodeWatch, useWatched } from "../lib/watched";
import {
  getEpisodesCached,
  getHomeEpisodes,
  getRecentEpisodes,
  getShowEpisodes,
  invalidateEpisodes,
  readScroll,
  saveScroll,
  setHomeEpisodes,
  setRecentEpisodes,
  setShowEpisodes,
} from "../lib/cache";

function useScrollRestore(key: string, ready: boolean) {
  const ref = useRef<HTMLDivElement>(null);
  const restored = useRef(false);
  useEffect(() => {
    restored.current = false;
    const el = ref.current;
    if (!el) return;
    const onScroll = () => saveScroll(key, el.scrollTop);
    el.addEventListener("scroll", onScroll, { passive: true });
    return () => {
      saveScroll(key, el.scrollTop);
      el.removeEventListener("scroll", onScroll);
    };
  }, [key]);
  useEffect(() => {
    if (!ready || restored.current) return;
    restored.current = true;
    const top = readScroll(key);
    if (top > 0 && ref.current) ref.current.scrollTop = top;
  }, [key, ready]);
  return ref;
}

// ---- episode rows -------------------------------------------------------------------------------

function ShowName({ title, showId }: { title?: string; showId?: string }) {
  if (!title) return null;
  const { zh, orig } = useShowName(title, showId);
  return <span className="show-name">{zh ?? orig}</span>;
}

/** One episode, built for scanning: who and when on a small first line, the title in full (up to two lines),
 *  the original title muted underneath, and status only when it means something. */
/** The query's words marked in a piece of text. */
function Marked({ text, query }: { text: string; query: string }) {
  const words = query.split(/\s+/).filter(Boolean).map((w) => w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&"));
  if (!words.length) return <>{text}</>;
  const re = new RegExp(`(${words.join("|")})`, "gi");
  return <>{text.split(re).map((part, i) => (i % 2 ? <mark key={i}>{part}</mark> : part))}</>;
}

/** Opens an episode and plays it from a subtitle match. */
function openAt(ep: Episode, time: number) {
  const s = useStore.getState();
  const at = { ...ep, progress: Math.max(0, time - 1) };
  void api.setProgress(ep.id, at.progress);
  s.load(at);
  s.openEpisode(at);
  playSoon(ep.id);
}

function ProgressRing({ pct }: { pct: number }) {
  const r = 4.5, c = 2 * Math.PI * r;
  const fill = Math.max(8, Math.min(100, pct));
  return (
    <svg className="prog-ring" viewBox="0 0 12 12" width="12" height="12" aria-hidden>
      <circle cx="6" cy="6" r={r} className="prog-ring-track" />
      <circle cx="6" cy="6" r={r} className="prog-ring-fill" strokeDasharray={c} strokeDashoffset={c * (1 - fill / 100)} />
    </svg>
  );
}

// Memoised, and watching only whether this row is the one playing and its own watch patch: a show page can list hundreds.
const EpisodeRow = memo(function EpisodeRow({ ep, showShow, hit, query }: { ep: Episode; showShow: boolean; hit?: ShowHit; query?: string }) {
  const openEpisode = useStore((s) => s.openEpisode);
  const open = () => (hit?.time != null ? openAt(ep, hit.time) : openEpisode(ep));
  const isPlaying = useStore((s) => s.current?.id === ep.id);
  useLang((s) => s.lang); // memoised: follow a language change itself
  const zh = useZh(ep.title);
  const { status, pct, leftMin, progress } = useEpisodeWatch(ep);
  const deep = !!ep.deep;
  const saved = useOffline((s) => !!s.saved[ep.id]);
  const loading = useOffline((s) => s.active[ep.id]);
  const showBar = status === "in-progress" && pct > 0;
  const toggleWatch = (e: React.MouseEvent) => {
    e.stopPropagation();
    void markWatched(ep, status !== "watched");
  };
  return (
    <div className={`ep-item ${isPlaying ? "playing" : ""} state-${status} ${status === "watched" ? "played" : ""}`} role="button" tabIndex={0} onClick={open} onKeyDown={(e) => e.key === "Enter" && open()}>
      <div className={`ep-thumb ${ep.videoId ? "video" : "square"}`}>
        {ep.cover ? <img className="thumb-img" src={ep.cover} alt="" loading="lazy" decoding="async" onError={(e) => {
          const i = e.currentTarget;
          if (ep.videoId && !i.dataset.f) { i.dataset.f = "1"; i.src = `https://i.ytimg.com/vi/${ep.videoId}/mqdefault.jpg`; } else i.style.visibility = "hidden";
        }} /> : <div className="cover-fallback" />}
        {(ep.audioUrl || ep.videoId) && (
          <button
            type="button"
            className="row-play"
            title={t("播放")}
            aria-label={t("播放")}
            onClick={(e) => { e.stopPropagation(); if (hit?.time != null) openAt(ep, hit.time); else playAll([ep]); }}
          >
            <Play size={14} fill="currentColor" />
          </button>
        )}
        {status === "watched" && <span className="thumb-done" aria-label={t("已看")}><Check size={11} strokeWidth={3} /></span>}
        {ep.videoId && ep.duration ? <span className={`thumb-dur ${showBar ? "lifted" : ""}`}>{clockShort(ep.duration)}</span> : null}
        {showBar && <span className="thumb-bar"><span style={{ width: `${pct}%` }} /></span>}
      </div>
      <div className="ep-body-col">
        <p className="ep-kicker">
          {status === "unseen" && <span className="unseen-dot" aria-hidden />}
          {status === "in-progress" && (
            <span className="kicker-prog">
              <ProgressRing pct={pct || 25} />
              {pct > 0
                ? (leftMin > 0 ? t("看到 {pct}% · 剩 {n} 分钟", { pct, n: leftMin }) : t("看到 {pct}%", { pct }))
                : t("看到 {at}", { at: clockShort(progress) })}
            </span>
          )}
          {showShow && ep.showId !== "single" && <ShowName title={ep.showTitle} showId={ep.showId} />}
          <span>{date(ep.published)}</span>
          {ep.duration ? <span>{duration(ep.duration)}</span> : null}
        </p>
        <p className="ep-title2">{query ? <Marked text={zh ?? ep.title} query={query} /> : zh ?? ep.title}</p>
        {zh && zh !== ep.title && <p className="ep-orig">{query ? <Marked text={ep.title} query={query} /> : ep.title}</p>}
        {hit && hit.field !== "title" && (
          <p className="ep-hit">
            <span className="ep-hit-where">{hit.field === "transcript" ? <><Play size={10} /> {t("字幕")} {clockShort(hit.time ?? 0)}</> : t("简介")}</span>
            <span className="ep-hit-text"><Marked text={hit.snippet} query={query ?? ""} /></span>
          </p>
        )}
        {(deep || ep.hasTranscript || ep.videoId || saved || loading !== undefined) && (
          <p className="ep-tags">
            {saved && <span className="tag saved"><ArrowDownToLine size={11} />{t("已下载")}</span>}
            {loading !== undefined && <span className="tag saved">{loading == null ? t("下载中") : t("下载中 {pct}%", { pct: Math.round(loading * 100) })}</span>}
            {deep && <span className="tag deep">{t("精读")}</span>}
            {ep.hasTranscript && <span className="tag">{t("字幕")}</span>}
            {ep.videoId && <span className="tag">{t("视频")}</span>}
          </p>
        )}
      </div>
      <button
        type="button"
        className="icon-btn sm row-watch-btn"
        title={status === "watched" ? t("标为未看") : t("标为已看")}
        aria-label={status === "watched" ? t("标为未看") : t("标为已看")}
        onClick={toggleWatch}
      >
        {status === "watched" ? <RotateCcw size={14} /> : <Check size={15} />}
      </button>
    </div>
  );
});

function groupLabel(ms?: number | null): string {
  if (!ms) return t("更早");
  const d = new Date(ms), now = new Date();
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const diff = Math.round((day(now) - day(d)) / 86400000);
  if (diff <= 0) return t("今天");
  if (diff === 1) return t("昨天");
  if (diff < 7) return t("本周");
  if (d.getFullYear() === now.getFullYear() && d.getMonth() === now.getMonth()) return t("本月");
  return monthLabel(d.getMonth() + 1, d.getFullYear() === now.getFullYear() ? undefined : d.getFullYear());
}

const clockShort = (s: number) => { const h = Math.floor(s / 3600), m = Math.floor((s % 3600) / 60), x = Math.floor(s % 60); return h ? `${h}:${String(m).padStart(2, "0")}:${String(x).padStart(2, "0")}` : `${m}:${String(x).padStart(2, "0")}`; };

function EpisodeList({ episodes, showShow, grouped = true }: { episodes: Episode[] | null; showShow: boolean; grouped?: boolean }) {
  if (episodes == null) return <SkeletonFeed />;
  if (!episodes.length) return <EmptyState art={<FlatLineArt />} title={t("这里还没有内容")} note={t("新的内容到了，会出现在这里")}><NightNote /></EmptyState>;
  if (!grouped) return <div className="ep-feed">{episodes.map((e) => <EpisodeRow key={e.id} ep={e} showShow={showShow} />)}</div>;
  const groups: { label: string; items: Episode[] }[] = [];
  for (const e of episodes) {
    const label = groupLabel(e.published);
    if (groups[groups.length - 1]?.label !== label) groups.push({ label, items: [] });
    groups[groups.length - 1].items.push(e);
  }
  return (
    <div className="ep-feed">
      {groups.map((g) => (
        <section key={g.label} className="ep-group">
          <h3 className="ep-group-h">{g.label}</h3>
          {g.items.map((e) => <EpisodeRow key={e.id} ep={e} showShow={showShow} />)}
        </section>
      ))}
    </div>
  );
}

// ---- home ---------------------------------------------------------------------------------------

function ContinueHero({ ep }: { ep: Episode }) {
  const openEpisode = useStore((s) => s.openEpisode);
  const zh = useZh(ep.title);
  const { pct, leftMin: left } = useEpisodeWatch(ep);
  // The whole card resumes: open the episode and start playing where it stopped.
  const resume = () => { openEpisode(ep); playSoon(ep.id); };
  return (
    <div className="hero" role="button" tabIndex={0} title={t("继续播放")} onClick={resume} onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), e.stopPropagation(), resume())}>
      <div className={`hero-cover ${ep.videoId ? "wide" : ""}`}>
        {ep.cover ? <img src={ep.cover} alt="" /> : <div className="cover-fallback" />}
        {pct > 0 && <span className="hero-bar" aria-hidden><span style={{ width: `${pct}%` }} /></span>}
      </div>
      <div className="hero-meta">
        <p className="meta-line">{t("继续收听")}{ep.showTitle && ep.showId !== "single" ? <> · <ShowName title={ep.showTitle} showId={ep.showId} /></> : null}</p>
        <p className="hero-title">{zh ?? ep.title}</p>
        {zh && zh !== ep.title && <p className="orig-line clamp1">{ep.title}</p>}
        <div className="hero-foot">
          <span className="btn primary hero-play" aria-hidden><Play size={15} fill="currentColor" /> {t("继续播放")}</span>
          {left > 0 && <span className="hero-left">{t("还剩 {left} 分钟", { left })}</span>}
        </div>
      </div>
    </div>
  );
}

function ShowTile({ show }: { show: Show }) {
  const go = useStore((s) => s.go);
  const { zh, orig } = useShowName(show.title, show.id);
  return (
    <div className="show-tile" role="button" tabIndex={0} onClick={() => go({ kind: "show", id: show.id })} onKeyDown={(e) => e.key === "Enter" && go({ kind: "show", id: show.id })}>
      <div className="tile-cover">{show.cover ? <img src={show.cover} alt="" loading="lazy" decoding="async" /> : <div className="cover-fallback">{orig.slice(0, 1)}</div>}</div>
      <p className="tile-name clamp1">{zh ?? orig}</p>
      {zh && <p className="tile-sub clamp1">{orig}</p>}
    </div>
  );
}

/** A compact episode card for the home rows: picture, show and date, title, and a play button on hover. */
/** Subtitles open with the episode, no transcription: already made here, the publisher's (YouTube captions,
 *  a feed transcript), a 精读 transcript, or one someone shared (asked for in one batch, see checkShared). */
const ownSubs = (e: Episode) => !!(e.hasTranscript || e.transcriptUrl || e.deep || e.videoId);
const useSharedHits = create<{ ids: Record<string, true> }>(() => ({ ids: {} }));
/** Episodes already asked about this session (an answer doesn't change often enough to ask again). */
const askedShared = new Set<string>();
function checkShared(eps: Episode[]) {
  const ids = eps.filter((e) => !ownSubs(e) && !askedShared.has(e.id)).map((e) => e.id).slice(0, 100);
  if (!ids.length) return;
  ids.forEach((id) => askedShared.add(id));
  void api.sharedExists(ids).then((hit) => {
    if (hit.length) useSharedHits.setState((s) => ({ ids: { ...s.ids, ...Object.fromEntries(hit.map((id) => [id, true as const])) } }));
  }).catch(() => {});
}

function EpisodeCard({ ep, wide }: { ep: Episode; wide?: boolean }) {
  const openEpisode = useStore((s) => s.openEpisode);
  const zh = useZh(ep.title);
  const { status, pct, leftMin } = useEpisodeWatch(ep);
  const shared = useSharedHits((s) => !!s.ids[ep.id]);
  const showBar = status === "in-progress" && pct > 0;
  const play = (e: React.MouseEvent) => { e.stopPropagation(); playAll([ep]); };
  return (
    <div className={`ep-card ${wide ? "wide" : ""} state-${status}`} role="button" tabIndex={0} onClick={() => openEpisode(ep)} onKeyDown={(e) => e.key === "Enter" && openEpisode(ep)}>
      <div className={`ep-thumb ${ep.videoId ? "video" : "square"}`}>
        {ep.cover ? <img className="thumb-img" src={ep.cover} alt="" loading="lazy" decoding="async" /> : <div className="cover-fallback" />}
        {status === "watched" && <span className="thumb-done" aria-label={t("已看")}><Check size={11} strokeWidth={3} /></span>}
        {showBar && <span className="thumb-bar"><span style={{ width: `${pct}%` }} /></span>}
        {(ownSubs(ep) || shared) && <span className="thumb-subs">{t("有字幕")}</span>}
        <button type="button" className="card-play" title={t("播放")} aria-label={t("播放")} onClick={play}><Play size={16} fill="currentColor" /></button>
      </div>
      <p className="ep-kicker">
        {status === "unseen" && <span className="unseen-dot" aria-hidden />}
        {ep.showId !== "single" && <ShowName title={ep.showTitle} showId={ep.showId} />}
        <span>{status === "in-progress" && leftMin ? t("看到 {pct}% · 剩 {n} 分钟", { pct, n: leftMin }) : date(ep.published)}</span>
      </p>
      <p className="card-title">{zh ?? ep.title}</p>
    </div>
  );
}

/** One recommended show in the side column. */
function RecRow({ r, onAdd, adding, done }: { r: Recommendation; onAdd: () => void; adding: boolean; done: boolean }) {
  const go = useStore((s) => s.go);
  return (
    <div className="rec-row" role="button" tabIndex={0} onClick={() => go({ kind: "channel", input: r.pick.input })} onKeyDown={(e) => e.key === "Enter" && go({ kind: "channel", input: r.pick.input })}>
      <img src={r.pick.cover} alt="" loading="lazy" decoding="async" />
      <div className="rec-text">
        <p className="rec-name clamp1">{pickName(r.pick)}</p>
        <p className="muted small clamp1">{r.because ? t("因为你在听「{name}」", { name: showName(r.because) }) : t(r.pick.intro)}</p>
      </div>
      {done ? <span className="tile-done"><Check size={14} /></span>
        : <button className="icon-btn sm rec-add" title={t("订阅")} aria-label={t("订阅")} disabled={adding} onClick={(e) => { e.stopPropagation(); onAdd(); }}>{adding ? <Loader2 size={14} className="spin" /> : <Plus size={16} />}</button>}
    </div>
  );
}

export function LatestView() {
  const { shows, set, go, reloadShows, notify } = useStore(useShallow((s) => ({ shows: s.shows, set: s.set, go: s.go, reloadShows: s.reloadShows, notify: s.notify })));
  const favorites = useLibrary((s) => s.favorites);
  const savedIds = useOffline((s) => Object.keys(s.saved).join());
  const [eps, setEps] = useState<Episode[] | null>(() => getHomeEpisodes());
  const [recent, setRecent] = useState<Episode[]>(() => getRecentEpisodes() ?? []);
  const [favEps, setFavEps] = useState<Episode[]>([]);
  const [dlEps, setDlEps] = useState<Episode[]>([]);
  const [all, setAll] = useState(false);
  const [hideWatched, setHideWatched] = useState(false);
  const [limit, setLimit] = useState(40);
  const [adding, setAdding] = useState<string | null>(null);
  const patches = useWatched((s) => s.patches);
  const pageRef = useScrollRestore("latest", eps != null);
  useEffect(() => {
    let live = true;
    api.listEpisodes(undefined, 200).then((list) => { if (live) { setHomeEpisodes(list); setEps(list); } });
    api.recentEpisodes().then((list) => { if (live) { setRecentEpisodes(list); setRecent(list); } });
    return () => { live = false; };
  }, [shows]);
  useEffect(() => {
    let live = true;
    void getEpisodesCached(favorites.slice(0, 30).map((f) => f.id)).then((r) => live && setFavEps(r));
    return () => { live = false; };
  }, [favorites]);
  useEffect(() => {
    let live = true;
    const ids = savedIds ? savedIds.split(",") : [];
    void getEpisodesCached(ids.slice(0, 30)).then((r) => live && setDlEps(r));
    return () => { live = false; };
  }, [savedIds]);
  const unfinished = (e: Episode) => isInProgress(e, patches[e.id]);
  const unwatched = (e: Episode) => !isWatched(e, patches[e.id]);
  const hero = recent.find(unfinished) ?? null;
  // Up next: other unfinished episodes, then saved ones not finished yet.
  const upNext = [...recent.filter((e) => unfinished(e) && e.id !== hero?.id), ...favEps.filter((e) => !unfinished(e) && unwatched(e))]
    .filter((e, i, a) => a.findIndex((x) => x.id === e.id) === i).slice(0, 12);
  // Keep initial home card order stable: shared subtitle checks only add badges, never reshuffle cards under the cursor.
  const unheard = (eps ?? []).filter(unwatched);
  const allList = hideWatched ? unheard : (eps ?? []);
  useEffect(() => { checkShared(unheard.slice(0, 18)); }, [eps]);
  const fresh = [...unheard.slice(0, 18).filter(ownSubs), ...unheard.slice(0, 18).filter((e) => !ownSubs(e)), ...unheard.slice(18)];
  const recs = useMemo(() => recommend({ shows, recent, favorites: favEps, downloads: dlEps }, 6), [shows, recent, favEps, dlEps]);
  const subscribe = async (input: string, name: string) => {
    setAdding(input);
    try {
      const r = await api.addSubscription(input);
      await reloadShows();
      if (!subscriptionVisible(input, r.show, useStore.getState().shows)) { notify(t("没有订上，再试一次")); return; }
      notify(t("已订阅「{name}」", { name }));
    }
    catch (err) { notify(errorText(err)); } finally { setAdding(null); }
  };
  const welcome = useStore((s) => s.welcome);
  const settingUp = useStore((s) => s.settingUp);
  return (
    <div className="page home" ref={pageRef}>
      {welcome && <WelcomeCard />}
      {hero && <ContinueHero ep={hero} />}
      {upNext.length > 0 && (
        <section>
          <div className="module-head">
            <h2 className="module-title">{t("接下来听")}</h2>
            <button className="link-btn" onClick={() => go({ kind: "recent" })}>{favorites.length ? t("全部 · 收藏 {n}", { n: favorites.length }) : t("全部")} <ChevronRight size={13} /></button>
          </div>
          <div className="card-strip">{upNext.map((e) => <EpisodeCard key={e.id} ep={e} />)}</div>
        </section>
      )}
      <div className="home-grid">
        <section className="home-main">
          <div className="module-head">
            <h2 className="module-title">{t("最新单集")}</h2>
            <div className="module-head-actions">
              {all && (
                <button className={`chip sm ${hideWatched ? "on" : ""}`} aria-pressed={hideWatched} onClick={() => setHideWatched(!hideWatched)}>
                  {t("隐藏已看")}
                </button>
              )}
              {(fresh.length > 9 || (eps?.length ?? 0) > 9) && <button className="link-btn" onClick={() => setAll(!all)}>{all ? t("收起") : t("查看全部")}</button>}
            </div>
          </div>
          {!shows.length && settingUp ? <SkeletonFeed /> : !shows.length ? <EmptyState art={<HeadphonesArt />} title={t("还没有订阅节目")} note={t("从精选里挑几档好节目，字幕会自动准备好")}><button className="btn primary" onClick={() => go({ kind: "discover" })}>{t("去发现节目")}</button></EmptyState>
            : eps == null ? <SkeletonFeed />
            : all ? <>
                <EpisodeList episodes={allList.slice(0, limit)} showShow />
                {allList.length > limit && <div className="center"><button className="btn" onClick={() => setLimit(limit + 40)}>{t("加载更多")}</button></div>}
              </>
            : <div className="card-grid">{fresh.slice(0, 9).map((e) => <EpisodeCard key={e.id} ep={e} wide />)}</div>}
        </section>
        <aside className="home-side">
          {recs.length > 0 && (
            <section>
              <div className="module-head">
                <h2 className="module-title">{t("为你推荐")}</h2>
                <button className="link-btn" onClick={() => go({ kind: "discover" })}>{t("更多")}</button>
              </div>
              <div className="rec-list">{recs.map((r) => <RecRow key={r.pick.id} r={r} adding={adding === r.pick.input} done={isSubscribed(r.pick, shows)} onAdd={() => void subscribe(r.pick.input, pickName(r.pick))} />)}</div>
            </section>
          )}
          {shows.length > 0 && (
            <section>
              <div className="module-head">
                <h2 className="module-title">{t("订阅节目")}</h2>
                <div className="module-head-actions">
                  {shows.length > 16 && <button className="link-btn" onClick={() => set({ manageOpen: true })}>{t("管理")}</button>}
                  <button className="link-btn" onClick={() => set({ addOpen: true, addTab: "search" })}><Plus size={13} /> {t("添加")}</button>
                </div>
              </div>
              <div className="show-grid">{shows.slice(0, 16).map((s) => <ShowTile key={s.id} show={s} />)}</div>
            </section>
          )}
        </aside>
      </div>
    </div>
  );
}

export function RecentView() {
  const [tab, setTab] = useState<"recent" | "saved">(() => (useLibrary.getState().favorites.length && !useStore.getState().current ? "saved" : "recent"));
  const favorites = useLibrary((s) => s.favorites);
  const queued = useLibrary((s) => s.queue.length);
  const [eps, setEps] = useState<Episode[] | null>(() => getRecentEpisodes());
  const [saved, setSaved] = useState<Episode[] | null>(null);
  const pageRef = useScrollRestore(`recent:${tab}`, (tab === "recent" ? eps : saved) != null);
  useEffect(() => {
    let live = true;
    api.recentEpisodes().then((list) => { if (live) { setRecentEpisodes(list); setEps(list); } });
    return () => { live = false; };
  }, []);
  useEffect(() => {
    let live = true;
    void getEpisodesCached(favorites.map((f) => f.id)).then((r) => live && setSaved(r));
    return () => { live = false; };
  }, [favorites]);
  const patches = useWatched((s) => s.patches);
  // "Play all" skips what's already finished.
  const playable = (saved ?? []).filter((e) => !isWatched(e, patches[e.id]));
  return (
    <div className="page" ref={pageRef}>
      <div className="page-head">
        <h1 className="page-title">{t("接下来听")}</h1>
        <div className="seg-control" role="tablist">
          <button role="tab" aria-selected={tab === "recent"} className={tab === "recent" ? "on" : ""} onClick={() => setTab("recent")}>{t("最近播放")}</button>
          <button role="tab" aria-selected={tab === "saved"} className={tab === "saved" ? "on" : ""} onClick={() => setTab("saved")}>{t("收藏")}{favorites.length ? ` ${favorites.length}` : ""}</button>
        </div>
        <span className="spacer" />
        {tab === "saved" && playable.length > 0 && (
          <button className="btn primary sm" onClick={() => playAll(playable)}><Play size={13} fill="currentColor" /> {t("全部播放（{n} 集）", { n: playable.length })}</button>
        )}
        {tab === "saved" && queued > 0 && <button className="btn ghost sm" onClick={clearQueue}>{t("清空待播（{n}）", { n: queued })}</button>}
      </div>
      {tab === "recent"
        ? <EpisodeList episodes={eps} showShow grouped={false} />
        : saved && !saved.length
          ? <EmptyState art={<TrayArt />} title={t("还没有收藏")} note={t("在单集页点书签按钮收藏，之后可以在这里按顺序连着听")} />
          : <EpisodeList episodes={saved} showShow grouped={false} />}
    </div>
  );
}

export function DownloadsView() {
  const saved = useOffline((s) => s.saved);
  const active = useOffline((s) => s.active);
  const ids = [...new Set([...Object.keys(active), ...Object.values(saved).sort((a, b) => b.savedAt - a.savedAt).map((x) => x.episodeId)])];
  const [eps, setEps] = useState<Episode[] | null>(null);
  const pageRef = useScrollRestore("downloads", !ids.length || eps != null);
  useEffect(() => {
    let live = true;
    void getEpisodesCached(ids).then((r) => live && setEps(r));
    return () => { live = false; };
  }, [ids.join()]);
  const total = Object.values(saved).reduce((n, x) => n + x.bytes, 0);
  const n = Object.keys(saved).length;
  const clear = async () => {
    if (!(await ask({ title: t("删除全部已下载的音频？"), message: t("字幕、精读和收听进度都会保留。"), ok: t("全部删除"), danger: true }))) return;
    await clearDownloads();
  };
  return (
    <div className="page" ref={pageRef}>
      <div className="page-head">
        <h1 className="page-title">{t("已下载")}</h1>
        <div className="row-actions">
          <button className="btn ghost sm" onClick={() => void api.openDataFolder("downloads")}><FolderOpen size={13} /> {t("打开文件夹")}</button>
          {n > 0 && <button className="btn ghost sm" onClick={clear}><Trash2 size={13} /> {t("全部删除")}</button>}
        </div>
      </div>
      <p className="muted page-sub">{n ? t("{n} 集 · 占用 {size} · 没有网络也能听", { n, size: bytes(total) }) : t("在单集页点下载按钮，就能离线收听")}</p>
      {!ids.length ? <EmptyState art={<TrayArt />} title={t("还没有下载")} note={t("喜欢的单集存到本机，出门没网也能接着听")} /> : <EpisodeList episodes={eps} showShow grouped={false} />}
    </div>
  );
}

// ---- show page ----------------------------------------------------------------------------------

type Filter = "all" | "unwatched" | "watched" | "subs";

export function ShowView({ id }: { id: string }) {
  const { shows, reloadShows, notify, go } = useStore(useShallow((s) => ({ shows: s.shows, reloadShows: s.reloadShows, notify: s.notify, go: s.go })));
  const ytPromo = useStore((s) => s.settings.youtubePromo);
  const patches = useWatched((s) => s.patches);
  const show = shows.find((s) => s.id === id);
  const [eps, setEps] = useState<Episode[] | null>(() => getShowEpisodes(id));
  const pageRef = useScrollRestore(`show:${id}`, eps != null);
  const [busy, setBusy] = useState(false);
  const [filter, setFilter] = useState<Filter>("all");
  const [sortAsc, setSortAsc] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [showOrig, setShowOrig] = useState(false);
  const [menu, setMenu] = useState(false);
  const menuBox = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!menu) return;
    const away = (e: PointerEvent) => { if (!menuBox.current?.contains(e.target as Node)) setMenu(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); setMenu(false); } };
    document.addEventListener("pointerdown", away);
    window.addEventListener("keydown", esc, true);
    return () => { document.removeEventListener("pointerdown", away); window.removeEventListener("keydown", esc, true); };
  }, [menu]);
  const [renaming, setRenaming] = useState(false);
  const [aliasDraft, setAliasDraft] = useState("");
  const names = useShowName(show?.title ?? "", id);
  const desc = (show?.description ?? "").replace(/<[^>]+>/g, "").trim().slice(0, 1500);
  const descZh = useZh(desc);
  useEffect(() => {
    let live = true;
    setEps(getShowEpisodes(id));
    api.listEpisodes(id).then((list) => { if (live) { setShowEpisodes(id, list); setEps(list); } });
    return () => { live = false; };
  }, [id]);
  // Search inside this show: titles (with their Chinese), show notes and subtitles, done by the backend.
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<ShowHit[] | null>(null);
  const searchBox = useRef<HTMLInputElement>(null);
  useEffect(() => { setQuery(""); setHits(null); }, [id]);
  useEffect(() => {
    const q = query.trim();
    if (!q) { setHits(null); return; }
    const timer = window.setTimeout(() => void api.searchShow(id, q).then(setHits).catch(() => setHits([])), 180);
    return () => window.clearTimeout(timer);
  }, [query, id]);
  useEffect(() => {
    const k = (e: KeyboardEvent) => { if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "f") { e.preventDefault(); searchBox.current?.focus(); searchBox.current?.select(); } };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, []);
  const byFilter = useMemo(
    () =>
      eps?.filter((e) => {
        if (filter === "all") return true;
        if (filter === "unwatched") return !isWatched(e, patches[e.id]);
        if (filter === "watched") return isWatched(e, patches[e.id]);
        return !!(e.hasTranscript || e.transcriptUrl || e.deep || e.videoId);
      }) ?? null,
    [eps, filter, patches],
  );
  const unwatchedCount = useMemo(() => (eps ?? []).filter((e) => !isWatched(e, patches[e.id])).length, [eps, patches]);
  const { month, setMonth, filtered: shownRaw } = useMonthFilter(byFilter);
  const shown = useMemo(() => (shownRaw && sortAsc ? [...shownRaw].reverse() : shownRaw), [shownRaw, sortAsc]);
  // "Play all" follows the list as filtered & sorted, skipping finished and unplayable episodes.
  const playable = (shown ?? []).filter((e) => (e.audioUrl || e.videoId) && !isWatched(e, patches[e.id]));
  if (!show) return (
    <div className="page">
      <EmptyState art={<HeadphonesArt />} title={t("还没有订阅节目")} note={t("新的内容到了，会出现在这里")}>
        <button className="btn primary" onClick={() => go({ kind: "latest" })}>{t("首页")}</button>
      </EmptyState>
    </div>
  );
  const refresh = async () => {
    setBusy(true);
    try {
      await api.refreshShow(id);
      const list = await api.listEpisodes(id);
      setShowEpisodes(id, list);
      setEps(list);
      await reloadShows();
    } catch (e) { notify(errorText(e)); } finally { setBusy(false); }
  };
  const markAll = async () => {
    setMenu(false);
    if (!unwatchedCount) return;
    if (!(await ask({ title: t("把这档节目的 {n} 期全部标为已看？", { n: unwatchedCount }), message: t("之后仍可以随时把单集改回未看。"), ok: t("全部标为已看") }))) return;
    try {
      const n = await markShowAllWatched(id, eps ?? []);
      setEps(await api.listEpisodes(id));
      notify(t("已将 {n} 期标为已看", { n }));
    } catch (e) { notify(errorText(e)); }
  };
  const remove = async () => {
    setMenu(false);
    if (!(await ask({ title: t("取消订阅「{name}」？", { name: show.title }), message: t("已生成的字幕、精读和收听进度会保留。"), ok: t("取消订阅"), danger: true }))) return;
    await api.removeShow(id);
    invalidateEpisodes(id);
    await reloadShows();
    scheduleAutoSync();
    go({ kind: "latest" });
  };
  const startRename = () => {
    setMenu(false);
    setAliasDraft(names.aliased ? (names.zh ?? names.orig) : "");
    setRenaming(true);
  };
  const commitRename = () => {
    const st = useStore.getState();
    void st.saveSettings(showAliasPatch(st.settings, id, aliasDraft));
    setRenaming(false);
  };
  const descText = showOrig || !descZh ? desc : descZh;
  const links = showLinks(show.feedUrl);
  return (
    <div className="page" ref={pageRef}>
      <div className="show-head">
        {show.cover ? <img src={show.cover} alt="" /> : <div className="cover-fallback" />}
        <div className="show-info">
          <p className="meta-line">{show.kind === "youtube" ? t("YouTube 频道") : show.kind === "xiaoyuzhou" ? t("小宇宙") : t("播客")}{show.author && show.author !== show.title ? ` · ${show.author}` : ""}</p>
          {renaming ? (
            <div className="show-rename-row">
              <input
                className="show-rename-input"
                value={aliasDraft}
                autoFocus
                placeholder={names.orig}
                aria-label={t("自定义节目名")}
                onChange={(e) => setAliasDraft(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") { e.preventDefault(); commitRename(); }
                  if (e.key === "Escape") { e.stopPropagation(); setRenaming(false); }
                }}
              />
              <button type="button" className="btn primary sm" onClick={commitRename}>{t("保存")}</button>
              <button type="button" className="btn ghost sm" onClick={() => setRenaming(false)}>{t("取消")}</button>
              {names.aliased && (
                <button type="button" className="link-btn small" onClick={() => { const st = useStore.getState(); void st.saveSettings(showAliasPatch(st.settings, id, "")); setRenaming(false); }}>{t("恢复原名")}</button>
              )}
            </div>
          ) : (
            <div className="show-title-row">
              <h1 className="show-title-h">{names.zh ?? names.orig}</h1>
              <button type="button" className="icon-btn sm show-rename-btn" title={t("重命名")} aria-label={t("重命名")} onClick={startRename}><Pencil size={14} /></button>
            </div>
          )}
          {(names.zh || names.orig !== show.title) && <span className="orig-line clamp1">{show.title}</span>}
          {desc && (
            <>
              <p className={`show-desc ${expanded ? "" : "clamp3"}`}>{descText}</p>
              <div className="row-actions tight">
                <button className="link-btn" onClick={() => setExpanded(!expanded)}>{expanded ? t("收起") : t("展开")}<ChevronDown size={12} className={expanded ? "flip" : ""} /></button>
                {descZh && <button className="link-btn" onClick={() => setShowOrig(!showOrig)}>{showOrig ? t("看中文") : t("查看原文")}</button>}
              </div>
            </>
          )}
          <div className="row-actions">
            {ytPromo && links.youtube && <YtButton channel={links.youtube} small />}
            <button className="btn sm" onClick={refresh} disabled={busy}>{busy ? <Loader2 size={13} className="spin" /> : <RefreshCw size={13} />} {t("检查更新")}</button>
            {playable.length > 1 && <button className="btn sm" title={t("按列表顺序连着播放没听完的 {n} 集", { n: playable.length })} onClick={() => playAll(playable)}><ListVideo size={14} /> {t("播放全部")}</button>}
            <div className="menu-wrap" ref={menuBox}>
              <button className="btn ghost sm" onClick={() => setMenu(!menu)} aria-haspopup="menu" aria-expanded={menu}><MoreHorizontal size={14} /> {t("更多")}</button>
              {menu && (
                <div className="menu" role="menu">
                  <button role="menuitem" onClick={startRename}><Pencil size={14} /> {t("重命名…")}</button>
                  <button role="menuitem" disabled={unwatchedCount === 0} onClick={markAll}><CheckCircle2 size={14} /> {t("全部标为已看")}</button>
                  <button role="menuitem" className="danger" onClick={remove}>{t("取消订阅")}</button>
                </div>
              )}
            </div>
            {ytPromo && links.youtube && <AlsoOn feedUrl={show.feedUrl} />}
          </div>
        </div>
      </div>
      {show.kind !== "qiaomu" && deepPickFor(show.title) && (
        <div className="deep-hint" role="button" tabIndex={0} onClick={() => go({ kind: "channel", input: deepPickFor(show.title)!.input })}>
          <BookOpen size={16} />
          <div><p className="deep-hint-t">{t("这档节目有乔木精读版")}</p><p className="muted small">{t("中文标题、每期精读长文和原文文稿，点击查看")}</p></div>
          <ChevronRight size={16} className="muted" />
        </div>
      )}
      <div className="list-head">
        <h2 className="section-title flush">{hits ? t("搜索结果") : t("全部单集")}</h2>
        <label className={`show-search ${query ? "on" : ""}`}>
          <Search size={14} />
          <input ref={searchBox} value={query} spellCheck={false} placeholder={t("搜索本节目（⌘F）")} aria-label={t("搜索本节目的单集、简介和字幕")}
            onChange={(e) => setQuery(e.target.value)} onKeyDown={(e) => { if (e.key === "Escape") { setQuery(""); e.currentTarget.blur(); } }} />
          {query && <button className="icon-btn sm" onClick={() => { setQuery(""); searchBox.current?.focus(); }} aria-label={t("清除搜索")}><X size={13} /></button>}
        </label>
        {!hits && (
          <div className="list-head-right">
            <button
              type="button"
              className={`btn ghost sm sort-btn${sortAsc ? " on" : ""}`}
              aria-pressed={sortAsc}
              title={sortAsc ? t("当前最早在前，点击切回最新在前") : t("当前最新在前，点击切到最早在前")}
              onClick={() => setSortAsc((v) => !v)}
            >
              <ArrowDownUp size={13} /> {sortAsc ? t("最早在前") : t("最新在前")}
            </button>
            <div className="seg-control">
              {([["all", t("全部")], ["unwatched", t("未看完")], ["watched", t("已看")], ["subs", t("有字幕")]] as [Filter, string][]).map(([k, label]) => (
                <button key={k} className={filter === k ? "on" : ""} onClick={() => setFilter(k)}>{label}</button>
              ))}
            </div>
          </div>
        )}
      </div>
      {hits ? (
        hits.length ? (
          <>
            <p className="muted small search-count">{t("找到 {n} 集", { n: hits.length })}{hits.some((h) => h.field === "transcript") ? ` · ${t("点字幕匹配会从那一句开始播放")}` : ""}</p>
            <div className="ep-feed">{hits.map((h) => <EpisodeRow key={h.episode.id} ep={h.episode} showShow={false} hit={h} query={query.trim()} />)}</div>
          </>
        ) : <EmptyState art={<FlatLineArt />} title={t("没有找到「{q}」", { q: query.trim() })} note={t("搜索范围是标题、简介和已有字幕的单集")} />
      ) : (
        <>
          <MonthFilter episodes={byFilter} month={month} onChange={setMonth} />
          <EpisodeList episodes={shown} showShow={false} />
        </>
      )}
    </div>
  );
}

// ---- picks: onboarding and discover --------------------------------------------------------------

function PickCard({ p, selected, subscribed, onClick, onOpen, action }: { p: Pick; selected?: boolean; subscribed: boolean; onClick?: () => void; onOpen?: () => void; action?: React.ReactNode }) {
  const press = onClick ?? onOpen;
  return (
    <div className={`pick-tile ${selected ? "selected" : ""} ${subscribed ? "done" : ""}`} role={press ? "button" : undefined} tabIndex={press ? 0 : undefined} aria-pressed={onClick ? selected : undefined}
      onClick={press} onKeyDown={(e) => press && (e.key === "Enter" || e.key === " ") && (e.preventDefault(), e.stopPropagation(), press())}>
      <div className="tile-cover">
        <img src={p.cover} alt="" loading="lazy" decoding="async" />
        {p.deep ? <span className="tile-badge deep">{t("精读")}</span> : p.video && <span className="tile-badge"><Youtube size={11} /> {t("视频")}</span>}
        {onClick ? <span className={`pick-check ${selected ? "on" : ""}`}>{selected && <Check size={13} strokeWidth={3} />}</span> : <div className="tile-action" onClick={(e) => e.stopPropagation()}>{action}</div>}
      </div>
      <p className="tile-name clamp1">{pickName(p)}</p>
      <p className="tile-sub clamp1">{pickSub(p)}</p>
      <p className="pick-intro">{t(p.intro)}</p>
    </div>
  );
}

function TopicChips({ topics, setTopics }: { topics: Topic[]; setTopics: (t: Topic[]) => void }) {
  const toggle = (x: Topic) => setTopics(topics.includes(x) ? topics.filter((y) => y !== x) : [...topics, x]);
  return (
    <div className="chips left">
      <button className={`chip ${topics.length === 0 ? "on" : ""}`} onClick={() => setTopics([])}>{t("全部")}</button>
      {TOPICS.map((x) => <button key={x} className={`chip ${topics.includes(x) ? "on" : ""}`} onClick={() => toggle(x)}>{t(x)}</button>)}
    </div>
  );
}

const visible = (topics: Topic[]) => PICKS.filter((p) => !topics.length || p.topics.some((x) => topics.includes(x)));

/** First run: no picking screen. A few curated shows are subscribed in the background and the home page fills in
 *  as they arrive; a welcome card says what happened and how to bring in your own. */
const STARTER = ["nexttoken", "no-priors", "lex", "zhang", "latetalk"];
export async function firstRunSetup() {
  const { settings, shows, saveSettings, reloadShows } = useStore.getState();
  if (settings.onboarded || shows.length) return;
  await saveSettings({ onboarded: true });
  useStore.setState({ welcome: true, settingUp: true });
  const queue = PICKS.filter((p) => STARTER.includes(p.id));
  await Promise.all(Array.from({ length: 3 }, async () => {
    for (let p = queue.shift(); p; p = queue.shift()) {
      try { await api.addSubscription(p.input); await reloadShows(); } catch { /* the rest still come */ }
    }
  }));
  useStore.setState({ settingUp: false });
}

function WelcomeCard() {
  const { set, shows, settingUp } = useStore(useShallow((s) => ({ set: s.set, shows: s.shows, settingUp: s.settingUp })));
  return (
    <div className="welcome-card">
      <div className="welcome-text">
        <p className="welcome-title">{t("欢迎使用 SubCast")}</p>
        <p className="muted small">{settingUp ? t("正在为你准备几档精选节目…") : t("已经为你订阅了 {n} 档精选节目，点任意一集就有双语字幕。", { n: shows.length })}{" "}{t("想听自己的？")}</p>
      </div>
      <div className="row-actions">
        <button className="btn sm primary" onClick={() => set({ addOpen: true, addTab: "import" })}><Youtube size={14} /> {t("导入 YouTube / 小宇宙订阅")}</button>
        <button className="btn sm" onClick={() => useStore.getState().go({ kind: "discover" })}>{t("发现更多节目")}</button>
      </div>
      <button className="icon-btn sm welcome-close" aria-label={t("关闭")} onClick={() => set({ welcome: false })}><X size={14} /></button>
    </div>
  );
}

const addInput = (prev: ReadonlySet<string>, input: string) => { const next = new Set(prev); next.add(input); return next; };
const dropInput = (prev: ReadonlySet<string>, input: string) => { const next = new Set(prev); next.delete(input); return next; };

export function DiscoverView() {
  const { shows, reloadShows, notify, go } = useStore(useShallow((s) => ({ shows: s.shows, reloadShows: s.reloadShows, notify: s.notify, go: s.go })));
  const [busyInputs, setBusyInputs] = useState<ReadonlySet<string>>(new Set());
  const [topics, setTopics] = useState<Topic[]>([]);
  const promo = usePromo();
  const bannerHidden = useStore((s) => s.settings.homeBannerHidden);
  const showBanner = promo && !bannerHidden;
  // The check appears before the feed finishes. It comes off again if the library does not actually have the show.
  const [optimistic, setOptimistic] = useState<ReadonlySet<string>>(new Set());
  const inflight = useRef(new Set<string>());
  const subscribed = (p: Pick) => optimistic.has(p.input) || isSubscribed(p, shows);
  const home = PICKS.find((p) => p.input === HOME_SHOW_FEED);
  const add = async (input: string, title: string) => {
    if (inflight.current.has(input)) return;
    inflight.current.add(input);
    setOptimistic((prev) => addInput(prev, input));
    setBusyInputs((prev) => addInput(prev, input));
    try {
      const r = await api.addSubscription(input);
      await reloadShows();
      const showsNow = useStore.getState().shows;
      if (!subscriptionVisible(input, r.show, showsNow)) {
        setOptimistic((prev) => dropInput(prev, input));
        notify(t("没有订上，再试一次"));
        return;
      }
      setOptimistic((prev) => dropInput(prev, input));
      scheduleAutoSync();
      notify(t("已订阅「{name}」", { name: title }));
    } catch (err) {
      setOptimistic((prev) => dropInput(prev, input));
      notify(errorText(err));
    } finally { inflight.current.delete(input); setBusyInputs((prev) => dropInput(prev, input)); }
  };
  const drop = async (p: Pick) => {
    const sub = subscribedShow(p, useStore.getState().shows);
    if (!sub?.id) { setOptimistic((prev) => dropInput(prev, p.input)); return; }
    if (!(await ask({ title: t("取消订阅「{name}」？", { name: pickName(p) }), message: t("已生成的字幕、精读和收听进度会保留。"), ok: t("取消订阅"), danger: true }))) return;
    setBusyInputs((prev) => addInput(prev, p.input));
    try {
      await api.removeShow(sub.id);
      await reloadShows();
      scheduleAutoSync();
      setOptimistic((prev) => dropInput(prev, p.input));
      notify(t("已取消订阅「{name}」", { name: pickName(p) }));
    } catch (err) { notify(errorText(err)); } finally { setBusyInputs((prev) => dropInput(prev, p.input)); }
  };
  const subBtn = (p: Pick, done: boolean) => {
    const cardBusy = busyInputs.has(p.input);
    return done
      ? <button type="button" className="tile-done" title={t("再点一次，取消订阅")} aria-label={t("再点一次，取消订阅")} onClick={(e) => { e.stopPropagation(); void drop(p); }} disabled={cardBusy}><Check size={16} className="when-idle" /><X size={16} className="when-hover" /></button>
      : <button className="tile-add" title={t("订阅")} onClick={(e) => { e.stopPropagation(); void add(p.input, pickName(p)); }} disabled={cardBusy}>{cardBusy ? <Loader2 size={16} className="spin" /> : <Plus size={18} />}</button>;
  };
  return (
    <div className="page">
      <h1 className="page-title">{t("发现")}</h1>
      <p className="muted page-sub">{t("精选海外与中文好节目，订阅后字幕和中文翻译开箱即用。想找别的节目，用上方搜索框。")}</p>
      {showBanner && home && !topics.length && <HomeShowBanner onHide={() => { void useStore.getState().saveSettings({ homeBannerHidden: true }); notify(t("好的，不再显示。可以在设置 › 通用里重新打开")); }} subscribed={subscribed(home)} busy={busyInputs.has(home.input)} onSubscribe={() => void add(home.input, pickName(home))} onOpen={() => go({ kind: "channel", input: home.input })} />}
      <TopicChips topics={topics} setTopics={setTopics} />
      <div className="tile-grid">
        {visible(topics).filter((p) => !(showBanner && p === home && !topics.length)).map((p) => <PickCard key={p.id} p={p} subscribed={subscribed(p)} onOpen={() => go({ kind: "channel", input: p.input })} action={subBtn(p, subscribed(p))} />)}
      </div>
    </div>
  );
}

/** A channel before subscribing: what it is, its latest episodes (playable), and one clear subscribe button. */
export function ChannelView({ input }: { input: string }) {
  const { shows, reloadShows, notify, go } = useStore(useShallow((s) => ({ shows: s.shows, reloadShows: s.reloadShows, notify: s.notify, go: s.go })));
  const pick = PICKS.find((p) => sameFeed(p.input, input));
  const sub = (pick ? subscribedShow(pick, shows) : undefined) ?? shows.find((s) => sameFeed(s.feedUrl, input));
  const [data, setData] = useState<{ show: Show; episodes: Episode[] } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const names = useShowName(data?.show.title ?? pick?.title ?? "");
  const { month, setMonth, filtered: shownCh } = useMonthFilter(data?.episodes ?? null);
  const [refreshing, setRefreshing] = useState(false);
  const [attempt, setAttempt] = useState(0);
  useEffect(() => {
    let live = true;
    setData(null); setError(null);
    // Stale-while-revalidate: show the saved copy at once, refresh from the network if it is older than 30 minutes.
    api.cachedShow(input).then((cached) => {
      if (!live) return;
      if (cached) setData(cached);
      if (cached && Date.now() - cached.fetchedAt < 30 * 60_000) return;
      setRefreshing(true);
      api.previewShow(input).then((d) => live && setData(d)).catch((e) => live && !cached && setError(errorText(e))).finally(() => live && setRefreshing(false));
    });
    return () => { live = false; };
  }, [input, attempt]);
  const subscribe = async () => {
    setBusy(true);
    try {
      const r = await api.addSubscription(input);
      await reloadShows();
      if (!r.show || !subscriptionVisible(input, r.show, useStore.getState().shows)) { notify(t("没有订上，再试一次")); return; }
      scheduleAutoSync();
      notify(t("已订阅「{name}」", { name: pick ? pickName(pick) : r.show.title }));
      go({ kind: "show", id: r.show.id });
    } catch (e) { notify(errorText(e)); } finally { setBusy(false); }
  };
  const cover = pick?.cover ?? data?.show.cover;
  const promo = usePromo();
  const links = showLinks(pick?.input ?? input);
  const desc = pick ? t(pick.intro) : (data?.show.description ?? "").replace(/<[^>]+>/g, "").trim().slice(0, 300);
  return (
    <div className="page">
      <div className="show-head">
        {cover ? <img src={cover} alt="" /> : <div className="cover-fallback" />}
        <div className="show-info">
          <p className="meta-line">{pick?.deep ? t("乔木精读 · 中文标题、精读长文与原文文稿") : pick?.by ?? data?.show.author ?? t("频道")}</p>
          <h1 className="show-title-h">{pick ? pickName(pick) : names.zh ?? names.orig}</h1>
          {(pick ? isChineseUi() && pick.zh !== pick.title : names.zh) && <span className="orig-line clamp1">{pick?.title ?? data?.show.title}</span>}
          {desc && <p className="show-desc clamp3">{desc}</p>}
          <div className="row-actions">
            {sub
              ? <button className="btn sm" onClick={() => go({ kind: "show", id: sub.id })}><Check size={13} /> {t("已订阅 · 查看")}</button>
              : <button className="btn primary" onClick={subscribe} disabled={busy}>{busy ? <Loader2 size={14} className="spin" /> : <Plus size={15} />} {t("订阅")}</button>}
            {promo && links.youtube && <YtButton channel={links.youtube} small={!!sub} />}
            {promo && links.youtube && <AlsoOn feedUrl={pick?.input ?? input} />}
            {data && <span className="muted small">{t("{n} 期节目 · 不订阅也可以直接收听", { n: data.episodes.length })}{refreshing ? ` · ${t("正在更新…")}` : ""}</span>}
          </div>
        </div>
      </div>
      {error ? <div className="panel-empty"><p className="error-text">{error}</p><button className="btn sm" onClick={() => setAttempt((n) => n + 1)}><RefreshCw size={13} /> {t("重试")}</button></div> : <>
        <MonthFilter episodes={data?.episodes ?? null} month={month} onChange={setMonth} />
        <EpisodeList episodes={shownCh} showShow={false} />
      </>}
    </div>
  );
}

export function showIcon(show: Show) {
  return show.cover ? <img src={show.cover} alt="" loading="lazy" decoding="async" /> : <div className="cover-fallback">{show.title.slice(0, 1)}</div>;
}
