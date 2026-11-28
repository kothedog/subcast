import { ask } from "../lib/confirm";
import { useShallow } from "zustand/react/shallow";
import { TopBar } from "./Chrome";
import { Component, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { t, tk } from "../lib/i18n";
import { scheduleAutoSync } from "../lib/sync";
import { rememberEpisodes, updateCachedProgress } from "../lib/cache";

class PanelBoundary extends Component<{ children: ReactNode }, { error: string | null }> {
  state = { error: null as string | null };
  static getDerivedStateFromError(e: Error) { return { error: e.message }; }
  render() {
    return this.state.error ? (
      <div className="panel-empty"><p className="lead">{t("这里出了点问题")}</p><p className="muted small">{this.state.error}</p>
        <button className="btn" onClick={() => this.setState({ error: null })}>{t("重试")}</button></div>
    ) : this.props.children;
  }
}
import { ArrowDownToLine, BookOpen, Captions, Check, ChevronDown, ScrollText, ExternalLink, MessageCircle, MoreHorizontal, PanelRightClose, PanelRightOpen, Play, Plus, FolderOpen, Trash2, Youtube, Bookmark, Pause, Loader2 } from "lucide-react";
import { BookArt, DoneCheck, EmptyState } from "./Illustrations";
import { Splitter } from "./Splitter";
import { DeepRead } from "./DeepRead";
import { SkeletonEpisode } from "./Skeleton";
import { PANEL, useLayout } from "../lib/layout";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errorText, type Episode } from "../lib/api";
import { date, duration, notesText } from "../lib/format";
import { sameFeed, showName } from "../lib/picks";
import { isZh, useZh, useZhParagraphs } from "../lib/zh";
import { autoTranslate, loadTranscript, useJobs } from "../lib/jobs";
import { seekSoon, useStore } from "../store";
import { ChatPanel, SummaryPanel } from "./AiPanels";
import { Controls, playSoon } from "./Players";
import { VideoSlot } from "./VideoDock";
import { bytes, cancelDownload, downloadEpisode, isSaved, removeDownload, useOffline } from "../lib/offline";
import { showLinks } from "../lib/showLinks";
import { toggleFavorite, useLibrary } from "../lib/library";
import { markWatched, useEpisodeWatch } from "../lib/watched";
import { YtVideoStrip } from "./YtPromo";
import { SubtitleTune } from "./SubtitleTune";
import { pieceAt, pieces } from "../lib/pieces";
import { setSubLift } from "../lib/reading";
import { subtitleModeFor } from "../lib/settings";
import { activeIndex, TranscriptPanel } from "./TranscriptPanel";
import { LearnText, WordCard, useSubCard, useSubtitleMarks, type LearnMarks } from "./Learn";

type Tab = "transcript" | "deep" | "chat" | "summary";
const TAB_NAMES: Record<Tab, string> = { transcript: tk("字幕"), deep: tk("乔木精读"), chat: tk("AI 问答"), summary: tk("AI 导读") };

/** Which piece (sentence-sized) of a block is being spoken, assuming speech is spread evenly over it. */
function pieceIndex(ps: string[], progress: number): number {
  const total = ps.reduce((n, p) => n + p.length, 0) || 1;
  let acc = 0;
  for (let k = 0; k < ps.length; k++) { acc += ps[k].length; if (acc / total >= progress) return k; }
  return ps.length - 1;
}

/** Audio episodes: the sentence being spoken, and the next one already there, faded, so reading runs ahead of
 *  the voice. Both are sentence pieces, not whole blocks, so it stays two short lines (four when bilingual). */
function AudioSubtitles({ ep }: { ep: Episode }) {
  const transcript = useJobs((s) => s.transcripts[ep.id]);
  const items = useJobs((s) => s.translations[ep.id]?.items);
  const mode = useStore((s) => subtitleModeFor(s.settings, ep));
  // Something only this audio has (an ad, most likely): say so instead of holding a line that isn't being said.
  const gapLeft = useStore((s) => (s.current?.id === ep.id && s.gapEnd && s.rawTime != null ? Math.ceil(s.gapEnd - s.rawTime) : 0));
  const segs = transcript?.segments;
  const livePos = useStore(useShallow((s) => {
    if (!segs?.length) return { i: -1, k: 0 };
    const t = s.current?.id === ep.id ? s.time : ep.progress;
    const i = Math.max(0, activeIndex(segs, t));
    const seg = segs[i];
    if (!seg) return { i: -1, k: 0 };
    const progress = Math.min(1, Math.max(0, (t - seg.start) / Math.max(0.5, seg.end - seg.start)));
    const cjk = /[一-鿿]/.test(seg.text);
    const origPs = pieces(seg.text, cjk);
    return { i, k: pieceIndex(origPs, progress) };
  }));
  const marks = useSubtitleMarks(ep.id);
  const { card, pick, close } = useSubCard(ep.id);
  // While a word card is open the lines hold still, so the word stays under it.
  const held = useRef(livePos);
  if (!card) held.current = livePos;
  const pos = held.current;
  if (!transcript || mode === "off") return null;
  if (gapLeft > 0) return (
    <div className="asub asub-gap" aria-live="off">
      <p>{t("这一段只在你的音频里，多半是广告，{n} 秒后回到正文", { n: gapLeft })}</p>
      <button className="btn ghost sm" onClick={() => { const s = useStore.getState(); if (s.gapEnd) s.player?.seekRaw?.(s.gapEnd); }}>{t("跳过")}</button>
    </div>
  );
  if (!segs?.length || pos.i < 0) return null;
  const { i, k } = pos;
  const seg = segs[i];
  if (!seg) return null;
  const cjk = (x: string) => /[\u4e00-\u9fff]/.test(x);
  const origPs = pieces(seg.text, cjk(seg.text));
  const zh = items?.[i];
  const zhPs = zh ? pieces(zh, true) : [];
  // The translation has its own pieces; the matching one is at the same share of the block.
  const kz = zhPs.length ? Math.min(zhPs.length - 1, Math.floor((k / origPs.length) * zhPs.length + 1e-9)) : 0;
  const nextSeg = segs[i + 1];
  const nextZh = items?.[i + 1];
  const nowLine = { orig: origPs[k], zh: zhPs.length ? (zhPs.length === origPs.length ? zhPs[k] : zhPs[kz]) : undefined };
  const nextLine = k + 1 < origPs.length
    ? { orig: origPs[k + 1], zh: zhPs.length ? (zhPs.length === origPs.length ? zhPs[k + 1] : kz + 1 < zhPs.length ? zhPs[kz + 1] : undefined) : undefined }
    : nextSeg ? { orig: pieces(nextSeg.text, cjk(nextSeg.text))[0], zh: nextZh ? pieces(nextZh, true)[0] : undefined } : null;
  const line = (l: { orig: string; zh?: string }, role: "now" | "next", key: string, at: number) => (
    <div key={key} className={`asub-line ${role}`} aria-hidden={role === "next"} data-i={at}>
      {l.zh && mode !== "original" && <p className="sub-zh">{l.zh}</p>}
      {(mode !== "translation" || !l.zh) && <p className="sub-orig"><SubText text={l.orig} marks={marks} /></p>}
    </div>
  );
  const onClick = (e: React.MouseEvent) => {
    const el = marks && (e.target as HTMLElement).closest<HTMLElement>(".lw");
    const at = Number(el?.closest<HTMLElement>("[data-i]")?.dataset.i);
    if (!el || !segs[at]) return;
    e.stopPropagation();
    pick(el, segs[at], items?.[at]);
  };
  return (
    <div className={`asub${marks ? " sub-learn" : ""}`} aria-live="off" onClick={onClick}>
      {line(nowLine, "now", `${i}-${k}`, i)}
      {nextLine && line(nextLine, "next", `${i}-${k}-n`, k + 1 < origPs.length ? i : i + 1)}
      {card && <WordCard target={card} ep={ep} onClose={close} />}
    </div>
  );
}

/** A subtitle line with its English words clickable when the learning mode is on, plain text otherwise. */
function SubText({ text, marks }: { text: string; marks: LearnMarks | null }) {
  return marks ? <LearnText text={text} marks={marks} /> : <>{text}</>;
}

function SubtitleOverlay({ ep }: { ep: Episode }) {
  const has = useJobs((s) => !!s.transcripts[ep.id]);
  if (!has) return null;
  return <><SubtitleLine ep={ep} /><SubtitleTune ep={ep} /></>;
}

/** The line being spoken, over the video. Dragging it moves the subtitles up or down (kept for every video); a
 *  click without moving plays or pauses like the rest of the picture. */
function SubtitleLine({ ep }: { ep: Episode }) {
  const transcript = useJobs((s) => s.transcripts[ep.id]);
  const items = useJobs((s) => s.translations[ep.id]?.items);
  const mode = useStore((s) => subtitleModeFor(s.settings, ep));
  const drag = useRef<{ y: number; lift: number; h: number; moved: boolean; word: HTMLElement | null; hadCard: boolean } | null>(null);
  const marks = useSubtitleMarks(ep.id);
  const { card, pick, close, busy } = useSubCard(ep.id);
  /** The word last clicked and when, so the second click of a double-click doesn't close its card again. */
  const lastWord = useRef<{ el: HTMLElement; at: number } | null>(null);
  const [dragging, setDragging] = useState(false);
  // A click plays or pauses a moment later, so a double-click (back to the usual place) doesn't do both.
  const click = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(click.current), []);
  // Only the piece being shown is subscribed to, so the overlay re-renders when the words change, not every tick.
  const segs = transcript?.segments;
  const liveCue = useStore(useShallow((s) => {
    if (!segs?.length) return { i: -1, orig: "", zh: "" };
    const t = s.current?.id === ep.id ? s.time : ep.progress;
    const i = activeIndex(segs, t);
    const seg = segs[i];
    if (!seg || t > seg.end + 1.5) return { i: -1, orig: "", zh: "" };
    const progress = Math.min(1, Math.max(0, (t - seg.start) / Math.max(0.5, seg.end - seg.start)));
    const cjk = /[一-鿿]/.test(seg.text);
    const zhRaw = items?.[i];
    return {
      i,
      orig: pieceAt(seg.text, cjk, progress),
      zh: zhRaw ? pieceAt(zhRaw, true, progress) : "",
    };
  }));
  // While a word card is open the line holds still, so the word stays under it.
  const held = useRef(liveCue);
  if (!card) held.current = liveCue;
  const cue = held.current;
  if (mode === "off" || cue.i < 0) return null;
  const down = (e: React.PointerEvent<HTMLDivElement>) => {
    const video = e.currentTarget.closest<HTMLElement>(".video");
    if (e.button !== 0 || !video) return;
    // Pointer capture retargets the release to the box, so the word under the press is noted now.
    const word = marks ? (e.target as HTMLElement).closest<HTMLElement>(".lw") : null;
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { y: e.clientY, lift: useStore.getState().settings.subtitleLift ?? 0, h: video.clientHeight || 1, moved: false, word, hadCard: busy() };
  };
  const move = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d) return;
    const dy = d.y - e.clientY;
    if (!d.moved && Math.abs(dy) < 4) return;
    if (!d.moved) { d.moved = true; setDragging(true); }
    setSubLift(d.lift + (dy / d.h) * 100);
  };
  const up = () => {
    const d = drag.current;
    drag.current = null;
    setDragging(false);
    if (!d || d.moved) return;
    window.clearTimeout(click.current);
    const seg = segs?.[cue.i];
    if (d.word && seg) {
      // A click on a word opens its card instead of playing or pausing.
      const last = lastWord.current;
      lastWord.current = { el: d.word, at: performance.now() };
      if (last?.el === d.word && performance.now() - last.at < 350) return;
      pick(d.word, seg, items?.[cue.i]);
      return;
    }
    // A click that closed the card is just that; the card resumes playback itself.
    if (d.hadCard) return;
    click.current = window.setTimeout(() => useStore.getState().player?.toggle(), 260);
  };
  return (
    <div className="subtitle-overlay">
      <div className={`sub-box ${dragging ? "dragging" : ""}${marks ? " sub-learn" : ""}`} onPointerDown={down} onPointerMove={move} onPointerUp={up} onPointerCancel={up} onDoubleClick={(e) => { e.stopPropagation(); window.clearTimeout(click.current); if (!(e.target as HTMLElement).closest(".lw")) setSubLift(0); }}>
        {cue.zh && mode !== "original" && <p className="sub-zh">{cue.zh}</p>}
        {(mode !== "translation" || !cue.zh) && <p className="sub-orig"><SubText text={cue.orig} marks={marks} /></p>}
      </div>
      {card && <WordCard target={card} ep={ep} onClose={close} className="over-video" />}
    </div>
  );
}

/** Cover artwork play/pause trigger, isolated so `playing` changes do not re-render the episode body. */
function CoverPlay({ ep, isCurrent, onToggle }: { ep: Episode; isCurrent: boolean; onToggle: () => void }) {
  const playing = useStore((s) => isCurrent && s.playing);
  return (
    <button
      type="button"
      className={`cover-play ${playing ? "is-playing" : ""}`}
      onClick={onToggle}
      title={playing ? t("暂停（空格）") : t("播放（空格）")}
      aria-label={playing ? t("暂停") : t("播放")}
    >
      {ep.cover ? <img src={ep.cover} alt="" /> : <span className="cover-fallback" />}
      <span className={`cover-play-btn ${playing ? "playing" : ""}`}>
        {playing ? <Pause size={30} fill="currentColor" /> : <Play size={30} fill="currentColor" />}
      </span>
    </button>
  );
}

/** Feed for this episode: the subscribed show, a channel opened before subscribing, or the channel behind a pasted video. */
async function subscriptionInput(ep: Episode): Promise<string | null> {
  if (ep.showId !== "single") {
    const live = useStore.getState().shows.find((s) => s.id === ep.showId);
    if (live?.feedUrl) return live.feedUrl;
    const raw = await api.getBlob(ep.showId, "show-json");
    if (raw) {
      try {
        const feed = (JSON.parse(raw) as { feedUrl?: string }).feedUrl;
        if (feed) return feed;
      } catch { /* not a show */ }
    }
  }
  if (ep.videoId) return api.youtubeChannelFeed(ep.videoId);
  return null;
}

/** Subscription state and actions for an episode's show (used by inline subscribe pill and overflow menu). */
function useShowSubscription(ep: Episode) {
  const [input, setInput] = useState<string | null>(null);
  const show = useStore((s) => s.shows.find((x) => x.id === ep.showId || (input && (
    sameFeed(x.feedUrl, input) ||
    (input.startsWith("https://www.youtube.com/feeds/videos.xml?playlist_id=UULF") &&
      x.feedUrl === input.replace("playlist_id=UULF", "channel_id=UC"))
  ))));
  const reloadShows = useStore((s) => s.reloadShows);
  const notify = useStore((s) => s.notify);
  const [busy, setBusy] = useState(false);
  const canSub = ep.showId !== "single" || !!ep.videoId;
  const subscribed = !!show;

  useEffect(() => {
    if (!canSub || show || input) return;
    let active = true;
    void subscriptionInput(ep).then((feed) => { if (active) setInput(feed); }).catch(() => {});
    return () => { active = false; };
  }, [canSub, ep, show, input]);

  const subscribe = async () => {
    setBusy(true);
    try {
      const feed = input ?? await subscriptionInput(ep);
      if (!feed) { notify(t("找不到这个节目的订阅源")); return; }
      const r = await api.addSubscription(feed);
      if (!r.show) { notify(t("找不到这个节目的订阅源")); return; }
      setInput(r.show.feedUrl);
      await reloadShows();
      scheduleAutoSync();
      notify(t("已订阅「{name}」", { name: showName(r.show.title) }));
    } catch (e) { notify(errorText(e)); }
    finally { setBusy(false); }
  };
  const unsubscribe = async () => {
    if (!show) return;
    const label = showName(show.title);
    setBusy(true);
    try {
      if (!(await ask({ title: t("取消订阅「{name}」？", { name: label }), message: t("已生成的字幕、精读和收听进度会保留。"), ok: t("取消订阅"), danger: true }))) return;
      await api.removeShow(show.id, ep.id);
      setInput(show.feedUrl);
      await reloadShows();
      scheduleAutoSync();
      notify(t("已取消订阅「{name}」", { name: label }));
    } catch (e) { notify(errorText(e)); }
    finally { setBusy(false); }
  };
  return { canSub, subscribed, busy, subscribe, unsubscribe };
}

/** Overflow menu on the episode header so secondary actions never crowd the title. */
function EpisodeMoreMenu({ ep, isCurrent, watched, sub }: {
  ep: Episode;
  isCurrent: boolean;
  watched: boolean;
  sub: ReturnType<typeof useShowSubscription>;
}) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); setOpen(false); } };
    document.addEventListener("pointerdown", away);
    window.addEventListener("keydown", esc, true);
    return () => { document.removeEventListener("pointerdown", away); window.removeEventListener("keydown", esc, true); };
  }, [open]);
  const openExternal = () => {
    setOpen(false);
    if (ep.videoId) {
      const s = Math.floor(isCurrent ? useStore.getState().time : ep.progress ?? 0);
      void openUrl(`https://www.youtube.com/watch?v=${ep.videoId}${s > 5 ? `&t=${s}s` : ""}`);
    } else if (ep.link) {
      void openUrl(ep.link);
    }
  };
  return (
    <div className="menu-wrap" ref={box}>
      <button type="button" className="round-btn" aria-haspopup="menu" aria-expanded={open} title={t("更多")} aria-label={t("更多")} onClick={() => setOpen((v) => !v)}>
        <MoreHorizontal size={15} />
      </button>
      {open && (
        <div className="menu right" role="menu">
          <button role="menuitem" onClick={() => { setOpen(false); void markWatched(ep, !watched); }}>
            <Check size={14} strokeWidth={watched ? 2.6 : 2} /> {watched ? t("标为未看") : t("标为已看")}
          </button>
          {sub.canSub && (
            <button role="menuitem" disabled={sub.busy} className={sub.subscribed ? "danger" : undefined} onClick={() => { setOpen(false); void (sub.subscribed ? sub.unsubscribe() : sub.subscribe()); }}>
              {sub.busy ? <Loader2 size={14} className="spin" /> : sub.subscribed ? <Check size={14} /> : <Plus size={14} />}
              {sub.subscribed ? t("取消订阅") : t("订阅这个节目")}
            </button>
          )}
          {(ep.videoId || ep.link) && (
            <button role="menuitem" onClick={openExternal}>
              {ep.videoId ? <Youtube size={14} /> : <ExternalLink size={14} />}
              {ep.videoId ? t("在 YouTube 打开（从当前位置继续）") : t("节目网页")}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

export function EpisodeView({ id }: { id: string }) {
  const { load } = useStore(useShallow((s) => ({ load: s.load })));
  const [ep, setEp] = useState<Episode | null>(null);
  const [tab, setTab] = useState<Tab>("transcript");
  const [startAudio, setStartAudio] = useState(false);

  useEffect(() => {
    let live = true;
    api.getEpisode(id).then(async (e) => {
      if (!e || !live) return;
      // 乔木精读 episodes learn their audio address from their page the first time they are opened.
      if (e.transcriptType === "qiaomu" && !e.audioUrl) {
        try { await api.qiaomuDetail(e.id); e = (await api.getEpisode(id)) ?? e; } catch { /* plays video, or shows the error later */ }
      }
      if (!live) return;
      rememberEpisodes([e]);
      // Offline with the audio saved: a YouTube episode starts in audio mode instead of a player that cannot load.
      // A saved video (mp4) still plays as video offline; only a saved audio track needs audio mode.
      const savedPath = useOffline.getState().saved[e.id]?.path ?? "";
      const offline = !!e.videoId && !navigator.onLine && isSaved(e.id) && !/\.(mp4|m4v|mov|webm)$/i.test(savedPath);
      setStartAudio(offline);
      setEp(e);
      if (e.deep) setTab((t) => (t === "transcript" ? "deep" : t));
      void loadTranscript(e);
      // Opening an episode loads it into the player, unless something else is playing.
      const s = useStore.getState();
      if (s.current?.id !== e.id && !s.playing) load(offline ? { ...e, videoId: undefined } : e);
    });
    return () => {
      live = false;
      const s = useStore.getState();
      if (s.current?.id === id) updateCachedProgress(id, s.time);
    };
  }, [id]);

  if (!ep) return <SkeletonEpisode />;
  return <EpisodeBody ep={ep} tab={tab} setTab={setTab} startAudio={startAudio} />;
}

/** Chapter times in show notes ("00:00 开场", "1:02:30") become buttons that seek; links open in the browser. */
function withStamps(text: string, seek: (s: number) => void): ReactNode[] {
  return text.split(/(https?:\/\/[^\s<>"'）)】]+|\b(?:\d{1,2}:)?\d{1,2}:\d{2}\b)/g).map((part, i) => {
    if (i % 2 === 0) return part;
    if (/^https?:/.test(part)) return <a key={i} href={part} className="notes-link" onClick={(e) => { e.preventDefault(); e.stopPropagation(); void openUrl(part); }}>{part.replace(/^https?:\/\/(www\.)?/, "").slice(0, 60)}</a>;
    const sec = part.split(":").map(Number).reduce((a, b) => a * 60 + b, 0);
    return <button key={i} type="button" className="stamp" title={t("跳到这里播放")} onClick={(e) => { e.stopPropagation(); seek(sec); }}>{part}</button>;
  });
}

function Notes({ ep, text, defaultOpen = false }: { ep: Episode; text: string; defaultOpen?: boolean }) {
  const [open, setOpen] = useState(defaultOpen);
  const [orig, setOrig] = useState(false);
  const zhAlready = isZh(text);
  // Translated with Google in the background as soon as the page opens, so it is ready before the card is.
  const google = useZhParagraphs(text, !zhAlready);
  const [model, setModel] = useState<{ paras: string[]; by: string; src?: string } | null>(null);
  const paras = text.split(/\n{2,}/).map((p) => p.trim()).filter(Boolean);
  const srcKey = `${text.length}:${text.slice(0, 40)}`;
  const seek = (sec: number) => {
    const s = useStore.getState();
    if (s.current?.id === ep.id) s.player?.seek(sec);
    else { s.load({ ...ep, progress: sec }); seekSoon(sec); }
  };

  useEffect(() => {
    setModel(null);
    api.getBlob(ep.id, "notes-zh").then((raw) => { if (raw) try { const m = JSON.parse(raw); if (!m.src || m.src === srcKey) setModel(m); } catch { /* ignore */ } });
  }, [ep.id, srcKey]);

  const zh = model?.paras.length ? model.paras : google;
  const shown = zhAlready || orig || !zh ? paras : zh;
  return (
    <div className={`notes ${open ? "open" : ""}`}>
      <div className="notes-head" role="button" tabIndex={0} onClick={() => setOpen(!open)} onKeyDown={(e) => e.key === "Enter" && setOpen(!open)}>
        <span>{t("本集简介")}</span>
        {open && zh && !zhAlready && <span className="notes-by">{model?.paras.length ? t("{name} 翻译", { name: t(model.by) }) : t("谷歌翻译")}</span>}
        {open && zh && !zhAlready && <button className="link-btn" onClick={(e) => { e.stopPropagation(); setOrig(!orig); }}>{orig ? t("看中文") : t("原文")}</button>}
        <ChevronDown size={14} className={open ? "flip" : ""} />
      </div>
      {open && (
        <div className="notes-body">
          {!zh && !orig && !zhAlready && <p className="muted small">{t("正在翻译…")}</p>}
          {shown.map((p, i) => <p key={i}>{withStamps(p, seek)}</p>)}
        </div>
      )}
    </div>
  );
}
/** Save for offline listening; shows progress while saving and the saved state after. */
function DownloadButton({ ep }: { ep: Episode }) {
  const [menu, setMenu] = useState(false);
  const menuBox = useRef<HTMLDivElement>(null);
  const notify = useStore((s) => s.notify);
  const go = useStore((s) => s.go);
  const saved = useOffline((s) => s.saved[ep.id]);
  const progress = useOffline((s) => s.active[ep.id]);
  useEffect(() => {
    if (!menu) return;
    const away = (e: PointerEvent) => { if (!menuBox.current?.contains(e.target as Node)) setMenu(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); setMenu(false); } };
    document.addEventListener("pointerdown", away);
    window.addEventListener("keydown", esc, true);
    return () => { document.removeEventListener("pointerdown", away); window.removeEventListener("keydown", esc, true); };
  }, [menu]);
  if (!ep.audioUrl && !ep.videoId) return null;
  if (progress !== undefined) {
    const r = 7, c = 2 * Math.PI * r;
    return (
      <button className="round-btn dl-busy" title={progress == null ? t("正在下载，点一下取消") : t("已下载 {pct}%，点一下取消", { pct: Math.round(progress * 100) })} onClick={() => cancelDownload(ep.id)}>
        <svg viewBox="0 0 20 20" width="18" height="18" className={progress == null ? "dl-ring spin" : "dl-ring"} aria-hidden>
          <circle cx="10" cy="10" r={r} className="dl-track" />
          <circle cx="10" cy="10" r={r} className="dl-fill" strokeDasharray={c} strokeDashoffset={c * (1 - (progress ?? 0.25))} />
        </svg>
      </button>
    );
  }
  if (saved) {
    // A download that just finished draws its tick once; older saves show it already drawn.
    const fresh = Date.now() - saved.savedAt < 5000;
    const close = () => setMenu(false);
    return (
      <div className="menu-wrap" ref={menuBox}>
        <button className="round-btn dl-done" title={t("已下载 · {size}", { size: bytes(saved.bytes) })} aria-haspopup="menu" aria-expanded={menu} onClick={() => setMenu(!menu)}><DoneCheck fresh={fresh} /></button>
        {menu && (
          <div className="menu right" role="menu">
            <p className="menu-note">{t("已下载 · {size}", { size: bytes(saved.bytes) })}</p>
            <button role="menuitem" onClick={() => { close(); void api.revealDownload(ep.id).catch((e) => notify(errorText(e))); }}><FolderOpen size={14} /> {t("在文件夹中显示")}</button>
            {ep.videoId && !/\.(mp4|m4v|mov|webm)$/i.test(saved.path) && (
              <button role="menuitem" onClick={() => { close(); void removeDownload(ep.id).then(() => downloadEpisode(ep)); }}><ArrowDownToLine size={14} /> {t("改为下载视频")}</button>
            )}
            <button role="menuitem" onClick={() => { close(); go({ kind: "downloads" }); }}><ArrowDownToLine size={14} /> {t("管理全部下载")}</button>
            <button role="menuitem" className="danger" onClick={() => { close(); void ask({ title: ep.videoId ? t("删除这一集已下载的文件？") : t("删除这一集已下载的音频？"), message: t("之后在线播放，字幕和进度都会保留。"), ok: t("删除"), danger: true }).then((yes) => { if (yes) void removeDownload(ep.id); }); }}><Trash2 size={14} /> {t("删除下载")}</button>
          </div>
        )}
      </div>
    );
  }
  return <button className="round-btn" title={ep.audioUrl ? t("下载，离线也能听") : t("下载视频，打开就播，离线也能看")} onClick={() => void downloadEpisode(ep)}><ArrowDownToLine size={15} /></button>;
}

function EpisodeBody({ ep, tab, setTab, startAudio }: { ep: Episode; tab: Tab; setTab: (t: Tab) => void; startAudio: boolean }) {
  // Feeds sometimes mark a deep-read that isn't there: then the tab goes away.
  const [noDeep, setNoDeep] = useState<string | null>(null);
  const { current, load, otherPlaying } = useStore(useShallow((s) => ({ current: s.current, load: s.load, otherPlaying: !!s.current && s.current.id !== ep.id && s.playing })));
  const go = useStore((s) => s.go);
  // Only a subscribed show has a page to go back to; others (opened from search or Discover) stay plain text.
  const hasShow = useStore((s) => s.shows.some((x) => x.id === ep.showId));
  const titleZh = useZh(ep.title);
  const isCurrent = current?.id === ep.id;
  const fav = useLibrary((s) => s.favorites.some((f) => f.id === ep.id));
  const watched = useEpisodeWatch(ep).status === "watched";
  // Translate foreign-language subtitles whichever tab is open: the video's subtitles need the Chinese too.
  const transcript = useJobs((s) => s.transcripts[ep.id]);
  const mode = useStore((s) => subtitleModeFor(s.settings, ep));
  useEffect(() => { if (transcript) autoTranslate(ep); }, [transcript, mode]);
  const coverToggle = () => {
    if (isCurrent) { useStore.getState().player?.toggle(); return; }
    load(ep); playSoon(ep.id);
  };
  // When the player controls scroll out of view, a copy docks at the bottom of the column.
  const leftRef = useRef<HTMLElement>(null);
  const controlsRef = useRef<HTMLDivElement>(null);
  const [docked, setDocked] = useState(false);
  useEffect(() => {
    const root = leftRef.current, el = controlsRef.current;
    if (!root || !el) { setDocked(false); return; }
    const io = new IntersectionObserver(([e]) => setDocked(!e.isIntersecting && e.boundingClientRect.top < (e.rootBounds?.top ?? 0)), { root, threshold: 0 });
    io.observe(el);
    return () => io.disconnect();
  }, [isCurrent, ep.id]);
  // rss.qiaomu.ai channels only carry a ~300-character summary; fetch the full notes once (YouTube or 小宇宙).
  const [fullNotes, setFullNotes] = useState<string | null>(null);
  useEffect(() => {
    setFullNotes(null);
    const short = notesText(ep.notes).length < 800;
    if (short && (ep.videoId || ep.link?.includes("xiaoyuzhoufm.com/episode/"))) void api.fullNotes(ep.id).then((n) => n && setFullNotes(n)).catch(() => {});
  }, [ep.id]);
  const notes = notesText(fullNotes ?? ep.notes);
  const { panel, panelOpen, put, below } = useLayout(useShallow((l) => ({ panel: l.panel, panelOpen: l.panelOpen, put: l.put, below: l.transcriptBelow })));
  const hasDeep = !!ep.deep && noDeep !== ep.id;
  // Subtitles under the player: the right column keeps the other tabs and opens on the first of them.
  const shownTab: Tab = below && tab === "transcript" ? (hasDeep ? "deep" : "summary") : tab;
  // Video first; the audio file is the fallback (and a choice, for listening without the picture).
  const [audioMode, setAudioMode] = useState(startAudio);
  const saved = useOffline((s) => !!s.saved[ep.id]);
  const both = !!ep.videoId && (!!ep.audioUrl || saved);
  const showVideo = !!ep.videoId && !audioMode;
  const ytPromo = useStore((s) => s.settings.youtubePromo);
  const epShow = useStore((s) => s.shows.find((x) => x.id === ep.showId));
  const showAlias = useStore((s) => (ep.showId && ep.showId !== "single" ? s.settings.showAliases?.[ep.showId]?.trim() : undefined));
  const showDisplay = showAlias || (ep.showTitle ? showName(ep.showTitle) : "");
  const ytChannel = showLinks(epShow?.feedUrl).youtube;
  const switchTo = (audio: boolean, why?: string) => {
    const s = useStore.getState();
    const t = s.current?.id === ep.id ? s.time : ep.progress;
    setAudioMode(audio);
    // Another episode is playing (this page was only opened): switch this page, leave the player alone.
    if (s.current?.id !== ep.id) return;
    // Play was pressed and is still waiting: the other kind of player starts when it is ready.
    const wanted = !!s.loadingSince;
    s.set({ current: audio ? { ...ep, videoId: undefined, progress: t } : { ...ep, progress: t }, time: t, player: null, playing: false, loadingSince: null });
    if (wanted) playSoon(ep.id);
    if (why) s.notify(why);
  };

  const sub = useShowSubscription(ep);
  // Stable, so the video (kept at the app's root, see VideoDock) isn't told about a change every render.
  const videoOverlay = useMemo(() => <SubtitleOverlay ep={ep} />, [ep.id]);
  const switchRef = useRef(switchTo);
  switchRef.current = switchTo;
  const onUnplayable = useMemo(() => () => switchRef.current(true, t("这个视频无法在应用内播放，已切换为音频")), [ep.id]);

  return (
    <div className="episode">
      <div className={`ep-body ${panelOpen ? "" : "panel-collapsed"}`} style={{ "--panel-w": `${panelOpen ? panel : 0}px` } as React.CSSProperties}>
        <div className="ep-leftcol">
        <TopBar />
        <section className={`ep-left ${below ? "transcript-below" : ""}`} ref={leftRef}>
          <div className="ep-title-block">
            <div className="ep-title">
              <p className="meta-line">
                {[ep.showTitle && ep.showId !== "single" ? (hasShow ? <button key="s" className="show-link" title={t("查看这档节目的全部单集")} onClick={() => go({ kind: "show", id: ep.showId })}>{showDisplay}</button> : <span key="s">{showDisplay}</span>) : null, date(ep.published), duration(ep.duration)].filter(Boolean).map((x, i) => <span key={i}>{i > 0 && " · "}{x}</span>)}
                {sub.canSub && !sub.subscribed && (
                  <button type="button" className="ep-sub-inline" disabled={sub.busy} onClick={() => void sub.subscribe()} title={t("订阅这个节目")}>
                    {sub.busy ? <Loader2 size={11} className="spin" /> : <Plus size={11} />} {t("订阅")}
                  </button>
                )}
                {watched && <span className="ep-watched-tag"><Check size={11} strokeWidth={2.5} /> {t("已看")}</span>}
              </p>
              <h1>{titleZh ?? ep.title}</h1>
              {titleZh && titleZh !== ep.title && <p className="orig-line clamp1">{ep.title}</p>}
            </div>
            {!panelOpen && <button className="round-btn" title={t("显示字幕栏（⌘\\）")} onClick={() => put({ panelOpen: true })}><PanelRightOpen size={15} /></button>}
            <button className={`round-btn fav-btn ${fav ? "on" : ""}`} aria-pressed={fav} title={fav ? t("取消收藏") : t("收藏这一集")} onClick={() => toggleFavorite(ep)}><Bookmark size={15} fill={fav ? "currentColor" : "none"} /></button>
            <DownloadButton ep={ep} />
            <EpisodeMoreMenu ep={ep} isCurrent={isCurrent} watched={watched} sub={sub} />
          </div>
          {both && (
            <div className="media-switch seg-control" role="radiogroup" aria-label={t("播放方式")}>
              <button role="radio" aria-checked={!audioMode} className={!audioMode ? "on" : ""} onClick={() => audioMode && switchTo(false)}>{t("视频")}</button>
              <button role="radio" aria-checked={audioMode} className={audioMode ? "on" : ""} onClick={() => !audioMode && switchTo(true)}>{t("仅音频")}</button>
            </div>
          )}
          {showVideo ? (
            otherPlaying ? (
              <div className="video-slot video-standby" role="button" tabIndex={0} onClick={() => { load(ep); playSoon(ep.id); }} onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && (e.preventDefault(), load(ep), playSoon(ep.id))} title={t("播放这一集")}>
                {ep.cover ? <img src={ep.cover} alt="" /> : <img src={`https://i.ytimg.com/vi/${ep.videoId}/hqdefault.jpg`} alt="" />}
                <span className="big-play"><Play size={28} fill="currentColor" /></span>
              </div>
            ) : (
              <VideoSlot ep={ep} overlay={videoOverlay} onUnplayable={ep.audioUrl || saved ? onUnplayable : undefined} />
            )
          ) : (
            <div className="audio-stage">
              <CoverPlay ep={ep} isCurrent={isCurrent} onToggle={coverToggle} />
              <div className="audio-sub">
                <AudioSubtitles ep={ep} />
                {transcript && <SubtitleTune ep={ep} />}
              </div>
            </div>
          )}
          {isCurrent ? <div className="controls-slot" ref={controlsRef}><Controls video={showVideo} /></div> : (
            <button className="btn primary" onClick={() => { load(ep); playSoon(ep.id); }}><Play size={14} /> {t("播放这一集")}</button>
          )}
          {below && <section className="ep-transcript" aria-label={t("字幕")}><TranscriptPanel ep={ep} /></section>}
          {ytPromo && ytChannel && <YtVideoStrip channel={ytChannel} name={showName(epShow?.title ?? "")} cover={epShow?.cover} />}
          {notes && <Notes ep={ep} text={notes} defaultOpen />}
          {isCurrent && docked && <div className="controls-dock"><Controls video={showVideo} /></div>}
        </section>
        </div>
        {panelOpen && <Splitter value={panel} min={PANEL.min} max={PANEL.max} def={PANEL.def} snap={PANEL.snap} edge="left" label={t("字幕栏宽度")} onChange={(w) => put({ panel: w })} onCollapse={() => put({ panelOpen: false })} />}
        {panelOpen && <section className="ep-right">
          <div className="tabs" role="tablist" data-tauri-drag-region>
            {!below && <button role="tab" title={t(TAB_NAMES.transcript)} aria-label={t(TAB_NAMES.transcript)} aria-selected={shownTab === "transcript"} className={shownTab === "transcript" ? "on" : ""} onClick={() => setTab("transcript")}><Captions size={15} /><span className="tab-label">{t("字幕")}</span></button>}
            {hasDeep && <button role="tab" title={t(TAB_NAMES.deep)} aria-label={t(TAB_NAMES.deep)} aria-selected={shownTab === "deep"} className={shownTab === "deep" ? "on" : ""} onClick={() => setTab("deep")}><BookOpen size={15} /><span className="tab-label">{t("精读")}</span></button>}
            <button role="tab" title={t(TAB_NAMES.summary)} aria-label={t(TAB_NAMES.summary)} aria-selected={shownTab === "summary"} className={shownTab === "summary" ? "on" : ""} onClick={() => setTab("summary")}><ScrollText size={15} /><span className="tab-label">{t("导读")}</span></button>
            <button role="tab" title={t(TAB_NAMES.chat)} aria-label={t(TAB_NAMES.chat)} aria-selected={shownTab === "chat"} className={shownTab === "chat" ? "on" : ""} onClick={() => setTab("chat")}><MessageCircle size={15} /><span className="tab-label">{t("问答")}</span></button>
            <span className="spacer" data-tauri-drag-region />
            <button className="icon-btn tabs-close" title={t("收起字幕栏（⌘\\）")} onClick={() => put({ panelOpen: false })}><PanelRightClose size={16} /></button>
          </div>
          <div className="tab-body">
            <PanelBoundary key={shownTab}>
              {shownTab === "transcript" && <TranscriptPanel ep={ep} />}
              {shownTab === "chat" && <ChatPanel ep={ep} />}
              {shownTab === "summary" && <SummaryPanel ep={ep} />}
              {shownTab === "deep" && <DeepRead ep={ep} onMissing={() => { setNoDeep(ep.id); setTab("transcript"); }}><EmptyState art={<BookArt />} title={t("这一集还没有精读")} note={t("这期没有精读长文，字幕和 AI 导读依然可以帮你")} /></DeepRead>}

            </PanelBoundary>
          </div>
        </section>}
      </div>
    </div>
  );
}
