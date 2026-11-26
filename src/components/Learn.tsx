// English learning mode: marked words and the word card in English subtitles, the word book with spaced review,
// this episode's words, shadowing and dictation. Everything here is off until 设置 › 学英语 turns it on.
import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useShallow } from "zustand/react/shallow";
import { ArrowUpRight, BookMarked, Check, ChevronLeft, ChevronRight, Download, GraduationCap, Loader2, Pause, Play, Plus, Search, Trash2, Volume2, X } from "lucide-react";
import { create } from "zustand";
import { api, errorText, llmChat, type AnkiNote, type Episode, type Segment } from "../lib/api";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { revealItemInDir } from "@tauri-apps/plugin-opener";
import { WORD_RE, isNew, loadDict, looksEnglish, lookup, tagLabel, useDict, type Entry } from "../lib/dict";
import { clipKey, playClip, prefetchClip, prepareClip, speak, stopClip, toggleClip, useClipState } from "../lib/clip";
import { Rating, intervals, isMastered, remindReview, reviewQueue, toCsv, useDueCount, useWords, type Grade, type Word } from "../lib/words";
import { llmReady, useJobs } from "../lib/jobs";
import { providerFor, seekSoon, useStore } from "../store";
import { clock } from "../lib/format";
import { t, tk, useLang } from "../lib/i18n";
import { Modal } from "./Chrome";
import { playSoon } from "./Players";
import { LearnStats } from "./LearnStats";
import { ask } from "../lib/confirm";
import type confetti from "canvas-confetti";
import { doneSound, keySound, rightSound, wrongSound } from "../lib/sfx";
import { EmptyState } from "./Illustrations";
import type { LearnLevel } from "../lib/settings";

// ---- shared ----------------------------------------------------------------------------------------------

/** The learning mode is on, and the dictionary and word book are loading or loaded. */
export function useLearnOn(): boolean {
  const on = useStore((s) => s.settings.learnOn);
  useEffect(() => { if (on) { void loadDict(); void useWords.getState().load(); } }, [on]);
  return on;
}

const fmtWait = (ms: number) => {
  const m = ms / 60_000;
  if (m < 60) return t("{n} 分钟", { n: Math.max(1, Math.round(m)) });
  const h = m / 60;
  if (h < 24) return t("{n} 小时", { n: Math.round(h) });
  const d = h / 24;
  if (d < 31) return t("{n} 天", { n: Math.round(d) });
  return t("{n} 个月", { n: Math.round(d / 30) });
};

const dueLabel = (w: Word, now: number) => {
  if (w.card.state === 0) return { text: t("新词"), now: false };
  if (w.card.due <= now) return { text: t("今天"), now: true };
  return { text: t("{wait}后", { wait: fmtWait(w.card.due - now) }), now: false };
};

// ---- subtitles: marked words -----------------------------------------------------------------------------

export interface LearnMarks { data: NonNullable<ReturnType<typeof useDict.getState>["data"]>; level: LearnLevel; mark: boolean; saved: Record<string, Word>; known: Record<string, number> }

/** The marks for English subtitle lines, or null when the learning mode does not apply. Stable between renders. */
export function useLearnMarks(english: boolean): LearnMarks | null {
  const on = useLearnOn();
  const { level, mark, card } = useStore(useShallow((s) => ({ level: s.settings.learnLevel, mark: s.settings.learnMark, card: s.settings.learnCard })));
  const data = useDict((s) => s.data);
  const { saved, known } = useWords(useShallow((s) => ({ saved: s.words, known: s.known })));
  return useMemo(() => (on && english && data && (mark || card) ? { data, level, mark, saved, known } : null), [on, english, data, level, mark, card, saved, known]);
}

type ParsedPart = string | { raw: string; lemma: string | null; isNewBase: boolean };
const LEARN_CACHE_MAX = 6000;
let learnCacheData: LearnMarks["data"] | null = null;
const learnCache = new Map<string, ParsedPart[]>();

function tokenizeLine(text: string, data: LearnMarks["data"], level: LearnLevel): ParsedPart[] {
  if (learnCacheData !== data) {
    learnCache.clear();
    learnCacheData = data;
  }
  const key = `${level}|${text}`;
  const hit = learnCache.get(key);
  if (hit) return hit;
  const parts: ParsedPart[] = [];
  let last = 0;
  for (const m of text.matchAll(WORD_RE)) {
    const i = m.index ?? 0;
    if (i > last) parts.push(text.slice(last, i));
    const e = lookup(data, m[0]);
    // Capitalised mid-sentence, or at the start but on no exam list: most likely a name (Chang, Jensen).
    const proper = /^[A-Z]/.test(m[0]) && ((i > 0 && !/[.?!]["”]?\s*$/.test(text.slice(0, i))) || !e?.tags);
    parts.push({ raw: m[0], lemma: e?.lemma ?? null, isNewBase: !!(e && !proper && isNew(e, level)) });
    last = i + m[0].length;
  }
  if (last < text.length) parts.push(text.slice(last));
  if (learnCache.size >= LEARN_CACHE_MAX) {
    const oldest = learnCache.keys().next().value;
    if (oldest !== undefined) learnCache.delete(oldest);
  }
  learnCache.set(key, parts);
  return parts;
}

/** One English line with its words wrapped, so they can be marked and clicked. */
export const LearnText = memo(function LearnText({ text, marks }: { text: string; marks: LearnMarks }) {
  const parts = tokenizeLine(text, marks.data, marks.level);
  return (
    <>
      {parts.map((part, idx) => {
        if (typeof part === "string") return part;
        // In the word book: learning (tinted) or mastered (just coloured); otherwise new for this level, unless known.
        const w = part.lemma ? marks.saved[part.lemma] : undefined;
        const cls = w
          ? (isMastered(w) ? " got" : " saved")
          : part.lemma && marks.mark && part.isNewBase && !marks.known[part.lemma]
            ? " new"
            : "";
        return <span key={idx} className={`lw${cls}`}>{part.raw}</span>;
      })}
    </>
  );
});

export interface CardTarget { el: HTMLElement; token: string; seg: Segment; zh?: string | null }

/** The word card for a clicked word: meaning, what it means in this sentence, the speaker's voice, save. */
export function WordCard({ target, ep, onClose, className }: { target: CardTarget; ep: Episode; onClose: () => void; className?: string }) {
  useLang((s) => s.lang);
  const data = useDict((s) => s.data);
  const entry = useMemo(() => (data ? lookup(data, target.token) : null), [data, target.token]);
  const id = entry?.lemma ?? target.token.toLowerCase();
  const saved = useWords((s) => s.words[id]);
  const known = useWords((s) => !!s.known[id]);
  const aiOn = useStore((s) => s.settings.learnAi);
  const [ctx, setCtx] = useState<string | null>(saved?.ctx ?? null);
  const [ctxState, setCtxState] = useState<"idle" | "loading" | "none" | "error">("idle");
  const [ctxTry, setCtxTry] = useState(0);
  const box = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState<React.CSSProperties>({ visibility: "hidden" });

  useEffect(() => {
    target.el.classList.add("open");
    return () => target.el.classList.remove("open");
  }, [target.el]);

  // Below the word, or above it when there is no room; kept inside the window.
  const place = useCallback(() => {
    // A subtitle line that moved on took the word with it: stay where the card is.
    if (!target.el.isConnected) { setPos((p) => (p.visibility === "hidden" ? { left: 12, bottom: 12 } : p)); return; }
    const r = target.el.getBoundingClientRect();
    const h = box.current?.offsetHeight ?? 240;
    const w = 320;
    const left = Math.min(Math.max(12, r.left - 20), window.innerWidth - w - 12);
    const below = r.bottom + 8 + h < window.innerHeight - 12;
    setPos(below ? { left, top: r.bottom + 8 } : { left, top: Math.max(12, r.top - 8 - h) });
  }, [target.el]);

  useEffect(() => {
    place();
    const ro = new ResizeObserver(place);
    if (box.current) ro.observe(box.current);
    return () => ro.disconnect();
  }, [place, ctxState, ctx]);

  useEffect(() => {
    const down = (e: PointerEvent) => { const n = e.target as Node; if (!box.current?.contains(n) && !target.el.contains(n)) onClose(); };
    const key = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    const scroll = (e: Event) => {
      if (box.current?.contains(e.target as Node)) return;
      const scroller = e.target instanceof HTMLElement ? e.target : null;
      // A list scrolling somewhere else (the transcript following along while a word over the video is open).
      if (scroller && !scroller.contains(target.el)) return;
      // Ignore programmatic auto-scroll (and tiny scroll jitter) while the word is still visible: follow it instead.
      const autoUntil = Number(scroller?.dataset.autoScrollUntil ?? 0);
      const r = target.el.getBoundingClientRect();
      const sr = scroller?.getBoundingClientRect();
      const visible = !sr || (r.bottom > sr.top + 4 && r.top < sr.bottom - 4);
      if (autoUntil >= performance.now() && visible) { place(); return; }
      if (!visible) { onClose(); return; }
      onClose();
    };
    document.addEventListener("pointerdown", down, true);
    window.addEventListener("keydown", key);
    window.addEventListener("scroll", scroll, true);
    return () => { document.removeEventListener("pointerdown", down, true); window.removeEventListener("keydown", key); window.removeEventListener("scroll", scroll, true); };
  }, [onClose, target.el, place]);

  // What the word means here, from the default model (cached on the saved word).
  useEffect(() => {
    if (!aiOn || saved?.ctx) return;
    let live = true;
    void (async () => {
      if (!(await llmReady("chat"))) { if (live) setCtxState("none"); return; }
      const p = providerFor(useStore.getState().settings, "chat");
      if (!p) { if (live) setCtxState("none"); return; }
      setCtxState("loading");
      try {
        const r = await llmChat({ provider: p, maxTokens: 400, system: CTX_PROMPT, messages: [{ role: "user", content: `单词：${target.token}\n句子：${target.seg.text}` }] }); // i18n-ignore (prompt)
        if (!live) return;
        const text = r.trim().replace(/^["“]|["”]$/g, "").slice(0, 120);
        setCtx(text); setCtxState("idle");
        if (useWords.getState().words[id]) useWords.getState().update(id, { ctx: text });
      } catch { if (live) setCtxState("error"); }
    })();
    return () => { live = false; };
  }, [aiOn, target.token, target.seg.text, ctxTry]);

  const add = () => {
    useWords.getState().add({
      id, word: target.token, ipa: entry?.ipa ?? "", meaning: entry?.meaning ?? ctx ?? "", ctx: ctx ?? undefined,
      tag: entry ? tagLabel(entry.tags) : "", sentence: target.seg.text, sentenceZh: target.zh ?? undefined,
      episodeId: ep.id, episodeTitle: ep.title, showTitle: ep.showTitle, start: target.seg.start, end: target.seg.end,
    });
    useStore.getState().notify(t("已加入生词本：{word}", { word: entry?.lemma ?? target.token }));
    // The sentence's own audio, cut now, so review plays it at once.
    prefetchClip(ep.id, target.seg.start, target.seg.end);
  };
  const label = entry ? tagLabel(entry.tags) : "";

  return createPortal(
    <div ref={box} className={`word-card${className ? ` ${className}` : ""}`} style={pos} role="dialog" aria-label={entry?.lemma ?? target.token}>
      <div className="wc-head">
        <span className="wc-word">{entry?.lemma ?? target.token}</span>
        {label && <span className="pill">{t(label)}</span>}
        <button className="icon-btn sm wc-say" title={t("读单词")} aria-label={t("读单词")} onClick={() => speak(entry?.lemma ?? target.token, 0.85)}><Volume2 size={15} /></button>
      </div>
      {entry?.ipa && <p className="wc-ipa">/{entry.ipa}/</p>}
      {entry ? <Meaning text={entry.meaning} /> : <p className="wc-none muted small">{t("词典里没有这个词")}</p>}
      {aiOn && ctxState !== "none" && (
        <div className="wc-ctx">
          <span className="wc-ctx-h">{t("在这句里")}</span>
          {ctx ? ctx : ctxState === "loading" ? <span className="skeleton wc-skel" /> : ctxState === "error" ? <span className="muted">{t("暂时没能解释，稍后再试")} <button type="button" className="link-btn" onClick={() => setCtxTry((n) => n + 1)}>{t("重试")}</button></span> : null}
        </div>
      )}
      <div className="wc-foot">
        {saved
          ? <button className="btn sm ghost wc-saved" onClick={() => { onClose(); openWord(id); }}><Check size={13} />{t("已在生词本")}</button>
          : <span className="wc-acts">
              {known
                ? <button className="btn sm ghost wc-saved" onClick={() => useWords.getState().unknow(id)} title={t("点一下撤销")}><Check size={13} />{t("已认识")}</button>
                : <button className="btn sm ghost" onClick={() => { useWords.getState().know([id]); onClose(); }} title={t("以后不再标成生词")}>{t("认识")}</button>}
              <button className="btn sm primary" onClick={add}><Plus size={13} />{t("加入生词本")}</button>
            </span>}
      </div>
    </div>,
    document.body,
  );
}

/** Marks for the subtitles over the video or under the cover, which have no transcript panel of their own. */
export function useSubtitleMarks(epId: string): LearnMarks | null {
  const english = useJobs((s) => {
    const tr = s.transcripts[epId];
    return tr ? looksEnglish(tr.language, tr.segments.slice(0, 20).map((x) => x.text).join(" ")) : false;
  });
  const marks = useLearnMarks(english);
  const card = useStore((s) => s.settings.learnCard);
  return card ? marks : null;
}

/** The word card opened from those subtitles. It always pauses (the line would move on and take the word with
 *  it, which is why Language Reactor and asbplayer pause on lookup), and plays again on close if it paused. */
export function useSubCard(epId: string) {
  const [card, setCard] = useState<CardTarget | null>(null);
  const paused = useRef(false);
  const open = !!card;
  const resume = useRef<number | undefined>(undefined);
  useEffect(() => {
    const s = useStore.getState();
    window.clearTimeout(resume.current);
    if (open) {
      if (s.current?.id === epId && s.playing) { s.player?.toggle(); paused.current = true; }
    } else if (paused.current) {
      // A moment later, so clicking from one word straight to the next doesn't play a beat in between.
      resume.current = window.setTimeout(() => {
        paused.current = false;
        const now = useStore.getState();
        if (now.current?.id === epId && !now.playing) now.player?.toggle();
      }, 300);
    }
  }, [open, epId]);
  useEffect(() => () => window.clearTimeout(resume.current), []);
  const closedAt = useRef(0);
  const close = useCallback(() => { closedAt.current = performance.now(); setCard(null); }, []);
  /** A card is open, or closed a moment ago by the press now under way (the card closes itself on pointerdown,
   *  before the subtitle's own handler runs), so that press shouldn't also play or pause. */
  const busy = useCallback(() => open || performance.now() - closedAt.current < 400, [open]);
  /** Opens the card for a clicked word, or closes it when that word's card is already open. */
  const pick = useCallback((el: HTMLElement, seg: Segment, zh?: string | null) => {
    setCard((c) => (c?.el === el ? null : { el, token: el.textContent ?? "", seg, zh }));
  }, []);
  return { card, pick, close, busy };
}

const CTX_PROMPT = "你是耐心的英语老师。用户给出一个英文单词和它所在的句子。用一句简短的中文（不超过 40 个字）说明这个词在这句话里的具体意思，可以点出它和句中其他部分的关系。只输出这句解释，不要重复单词，不要加引号。"; // i18n-ignore (prompt)

/** Dictionary senses, one per line, with the part of speech set apart. */
function Meaning({ text }: { text: string }) {
  return (
    <div className="wc-def">
      {text.split("\n").map((l, i) => {
        const m = l.match(/^([a-z]+\.(?:\s*&\s*[a-z]+\.)?|\[[^\]]+\])\s*(.*)$/);
        return <p key={i}>{m ? <><span className="wc-pos">{m[1]}</span>{m[2]}</> : l}</p>;
      })}
    </div>
  );
}

/** Shown once, at the top of the first English transcript: offers the learning mode. */
export function LearnTip() {
  const { asked, on, save } = useStore(useShallow((s) => ({ asked: s.settings.learnAsked, on: s.settings.learnOn, save: s.saveSettings })));
  if (asked || on) return null;
  return (
    <div className="learn-tip">
      <span className="learn-tip-ic"><GraduationCap size={16} /></span>
      <div className="learn-tip-text"><p className="learn-tip-t">{t("想顺便学英语吗？")}</p><p className="learn-tip-s">{t("点字幕里的单词就能查，生词进生词本。")}</p></div>
      <div className="learn-tip-acts">
        <button className="btn sm ghost" onClick={() => void save({ learnAsked: true })}>{t("不用了")}</button>
        <button className="btn sm primary" onClick={() => { void save({ learnAsked: true, learnOn: true }); useStore.getState().notify(t("已打开英语学习模式，可以在 设置 › 学英语 里调整")); }}>{t("打开")}</button>
      </div>
    </div>
  );
}

// ---- this episode's words --------------------------------------------------------------------------------

interface Found { entry: Entry; token: string; seg: Segment; idx: number; count: number }

/** Every word above the listener's level in this episode, rarest first. */
function episodeWords(segments: Segment[], data: LearnMarks["data"], level: LearnLevel, known: Record<string, number>): Found[] {
  const found = new Map<string, Found>();
  segments.forEach((seg, idx) => {
    for (const m of seg.text.matchAll(WORD_RE)) {
      const e = lookup(data, m[0]);
      if (!e || known[e.lemma] || !isNew(e, level)) continue;
      const i = m.index ?? 0;
      if (/^[A-Z]/.test(m[0]) && (i > 0 || !e.tags)) continue;
      const f = found.get(e.lemma);
      if (f) f.count++; else found.set(e.lemma, { entry: e, token: m[0], seg, idx, count: 1 });
    }
  });
  const weight = (f: Found) => (f.entry.rank || 60000) / Math.sqrt(f.count);
  return [...found.values()].sort((a, b) => weight(a) - weight(b));
}

export function EpisodeWords({ ep, segments, zh, onClose }: { ep: Episode; segments: Segment[]; zh: (number | string | null | undefined)[] | undefined; onClose: () => void }) {
  useLang((s) => s.lang);
  const data = useDict((s) => s.data);
  const level = useStore((s) => s.settings.learnLevel);
  const words = useWords((s) => s.words);
  // Known words as of opening: marking the rest known shouldn't empty the list under you.
  const [known] = useState(() => useWords.getState().known);
  const list = useMemo(() => (data ? episodeWords(segments, data, level, known) : []), [data, segments, level, known]);
  const fresh = list.filter((f) => !words[f.entry.lemma]);
  const [picked, setPicked] = useState<Set<string>>(() => new Set());
  const [saying, setSaying] = useState<string | null>(null);
  useEffect(() => { setPicked(new Set(fresh.slice(0, 15).map((f) => f.entry.lemma))); }, [list.length]);
  const toggle = (id: string) => setPicked((p) => { const n = new Set(p); if (n.has(id)) n.delete(id); else n.add(id); return n; });
  const sayWord = async (e: React.MouseEvent, word: string) => {
    e.preventDefault();
    e.stopPropagation();
    setSaying(word);
    try { await speak(word, 0.9); } finally { setSaying((cur) => (cur === word ? null : cur)); }
  };
  const addPicked = () => {
    const add = useWords.getState().add;
    let n = 0;
    for (const f of fresh) if (picked.has(f.entry.lemma)) {
      prefetchClip(ep.id, f.seg.start, f.seg.end);
      add({ id: f.entry.lemma, word: f.token, ipa: f.entry.ipa, meaning: f.entry.meaning, tag: tagLabel(f.entry.tags), sentence: f.seg.text, sentenceZh: typeof zh?.[f.idx] === "string" ? (zh[f.idx] as string) : undefined, episodeId: ep.id, episodeTitle: ep.title, showTitle: ep.showTitle, start: f.seg.start, end: f.seg.end });
      n++;
    }
    useStore.getState().notify(t("已加入 {n} 个词到生词本", { n }));
    onClose();
  };
  // Lute's "mark the rest known": the words left unticked stop being marked as new.
  const rest = fresh.filter((f) => !picked.has(f.entry.lemma)).map((f) => f.entry.lemma);
  const knowRest = () => {
    useWords.getState().know(rest);
    if (picked.size) addPicked();
    else { useStore.getState().notify(t("已把 {n} 个词标为认识", { n: rest.length })); onClose(); }
  };
  return (
    <Modal title={t("本期生词")} onClose={onClose}>
      {!data ? <div className="panel-empty"><Loader2 className="spin" size={18} /></div> : !list.length ? <EmptyState title={t("这一期没有超出你水平的词")} note={t("可以在 设置 › 学英语 里调高水平")} /> : <>
        <p className="muted small ew-note">{t("按难度从高到低排列，默认勾选前 15 个还没收藏的词。认识的词可以取消勾选，再标为认识。")}</p>
        <div className="ew-list">
          {list.map((f) => {
            const have = !!words[f.entry.lemma];
            return (
              <label key={f.entry.lemma} className={`ew-item${have ? " have" : ""}`}>
                <input type="checkbox" checked={have || picked.has(f.entry.lemma)} disabled={have} onChange={() => toggle(f.entry.lemma)} />
                <span className="ew-w">{f.entry.lemma}</span>
                {tagLabel(f.entry.tags) && <span className="pill">{t(tagLabel(f.entry.tags))}</span>}
                <span className="ew-d">{f.entry.meaning.split("\n")[0]}</span>
                {f.count > 1 && <span className="muted small ew-n" title={t("在本期字幕里出现了 {n} 次", { n: f.count })}>{t("{n} 次", { n: f.count })}</span>}
                <button
                  type="button"
                  className="icon-btn sm ew-say"
                  title={t("读单词")}
                  aria-label={t("读单词")}
                  onClick={(e) => void sayWord(e, f.entry.lemma)}
                >
                  {saying === f.entry.lemma ? <Loader2 size={13} className="spin" /> : <Volume2 size={14} />}
                </button>
              </label>
            );
          })}
        </div>
        <div className="modal-foot">
          <button className="btn ghost ew-know" disabled={!rest.length} onClick={knowRest} title={t("没勾选的词以后不再标成生词，可以在 设置 › 学英语 里清空")}>{picked.size ? t("加入，其余 {n} 个标为认识", { n: rest.length }) : t("{n} 个都标为认识", { n: rest.length })}</button>
          <span className="spacer" />
          <button className="btn" onClick={onClose}>{t("取消")}</button><button className="btn primary" disabled={!picked.size} onClick={addPicked}>{t("加入 {n} 个词", { n: [...picked].filter((id) => !words[id]).length })}</button></div>
      </>}
    </Modal>
  );
}

// ---- dictation --------------------------------------------------------------------------------------------

const norm = (s: string) => s.toLowerCase().replace(/[’‘]/g, "'").match(/[a-z0-9']+/g) ?? [];

/** Marks each word of the original as heard (typed) or missed, by longest common subsequence. */
export function compare(original: string, typed: string): { word: string; ok: boolean }[] {
  const a = original.match(/\S+/g) ?? [];
  const an = a.map((w) => norm(w).join(""));
  const b = norm(typed);
  const dp = Array.from({ length: an.length + 1 }, () => new Array<number>(b.length + 1).fill(0));
  for (let i = an.length - 1; i >= 0; i--) for (let j = b.length - 1; j >= 0; j--) dp[i][j] = an[i] && an[i] === b[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
  const out: { word: string; ok: boolean }[] = [];
  let i = 0, j = 0;
  while (i < an.length) {
    if (!an[i]) { out.push({ word: a[i], ok: true }); i++; continue; }
    if (j < b.length && an[i] === b[j]) { out.push({ word: a[i], ok: true }); i++; j++; }
    else if (j < b.length && dp[i][j + 1] >= dp[i + 1][j]) j++;
    else { out.push({ word: a[i], ok: false }); i++; }
  }
  return out;
}

export function Dictation({ ep, segments, from, onClose }: { ep: Episode; segments: Segment[]; from: number; onClose: () => void }) {
  useLang((s) => s.lang);
  const usable = useMemo(() => segments.map((_, i) => i).filter((i) => (segments[i].text.match(/\S+/g)?.length ?? 0) >= 3), [segments]);
  const startAt = Math.max(0, usable.findIndex((i) => i >= from));
  const [k, setK] = useState(startAt);
  const [typed, setTyped] = useState("");
  const [result, setResult] = useState<{ word: string; ok: boolean }[] | null>(null);
  const [score, setScore] = useState({ right: 0, total: 0 });
  const input = useRef<HTMLTextAreaElement>(null);
  const seg = segments[usable[k]];
  const play = useCallback(() => { if (seg) void playClip(ep, seg.start, seg.end).then((ok) => { if (!ok) speak(seg.text); }); }, [ep, seg]);
  useEffect(() => { setTyped(""); setResult(null); play(); input.current?.focus(); }, [k]);
  useEffect(() => () => stopClip(), []);
  if (!seg) return <Modal title={t("听写练习")} onClose={onClose}><EmptyState title={t("这一期没有适合听写的句子")} /></Modal>;
  const check = () => {
    const r = compare(seg.text, typed);
    setResult(r);
    setScore((s) => ({ right: s.right + r.filter((x) => x.ok).length, total: s.total + r.length }));
  };
  const next = () => setK((x) => Math.min(usable.length - 1, x + 1));
  return (
    <Modal title={t("听写练习")} onClose={onClose}>
      <div className="dict">
        <div className="dict-top">
          <span className="muted small">{t("第 {n} 句", { n: k + 1 })} · {clock(seg.start)}</span>
          {score.total > 0 && <span className="muted small">{t("正确率 {p}%", { p: Math.round((score.right / score.total) * 100) })}</span>}
        </div>
        <button className="btn play dict-play" onClick={play}><Play size={13} className="fill-ico" />{t("再听一遍")}</button>
        <textarea ref={input} className="dict-input" rows={3} spellCheck={false} value={typed} placeholder={t("听到什么就写什么，按 Enter 检查")} disabled={!!result}
          onChange={(e) => setTyped(e.target.value)} onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); if (!result) check(); } }} />
        {result && (
          <div className="dict-result">
            <p className="dict-ans">{result.map((w, i) => <span key={i} className={w.ok ? "" : "miss"}>{w.word} </span>)}</p>
            <p className="muted small">{t("听出 {a}/{b} 个词", { a: result.filter((x) => x.ok).length, b: result.length })}</p>
          </div>
        )}
        <div className="modal-foot">
          <button className="btn ghost" onClick={() => { setResult(compare(seg.text, "")); }} disabled={!!result}>{t("看答案")}</button>
          <span className="spacer" />
          {result ? <button className="btn primary" onClick={next} disabled={k >= usable.length - 1} autoFocus>{t("下一句")}</button> : <button className="btn primary" onClick={check} disabled={!typed.trim()}>{t("检查")}</button>}
        </div>
      </div>
    </Modal>
  );
}

// ---- word book and review ---------------------------------------------------------------------------------

type Filter = "due" | "all" | "mastered";

/** The word open in the word book's detail card (set from anywhere, e.g. a subtitle's word card). */
const useWordDetail = create<{ id: string | null }>(() => ({ id: null }));
export function openWord(id: string) {
  useWordDetail.setState({ id });
  if (useStore.getState().view.kind !== "words") useStore.getState().go({ kind: "words" });
}

/** The built-in show that singly opened links belong to: not worth naming as a source. */
const SINGLE_SHOW = "单独打开"; // i18n-ignore (stored name, compared, never shown)
const sourceOf = (w: Word) => [w.showTitle && w.showTitle !== SINGLE_SHOW ? w.showTitle : "", w.episodeTitle].filter(Boolean).join(" · ");

/** Plays a saved word's sentence (or stops it, when `toggle` and it is playing); reads it aloud when there is no audio. */
async function playWord(w: Word, toggle = false) {
  if (!(await (toggle ? toggleClip : playClip)({ id: w.episodeId }, w.start, w.end))) speak(w.sentence);
}

/** Opens the episode a word came from and plays it from just before its sentence. */
export async function openSource(w: Pick<Word, "episodeId" | "start">) {
  stopClip();
  const s = useStore.getState();
  const at = Math.max(0, w.start - 1);
  if (s.current?.id === w.episodeId && s.player) { s.player.seek(at); s.player.play(); }
  else {
    const ep = await api.getEpisode(w.episodeId).catch(() => null);
    if (!ep) { s.notify(t("这一集已经不在了")); return; }
    s.load({ ...ep, progress: at });
    seekSoon(at);
    playSoon(ep.id);
  }
  useWordDetail.setState({ id: null });
  s.go({ kind: "episode", id: w.episodeId });
}

const clipOf = (k: string) => (s: { key: string | null; loading: boolean }) => (s.key !== k ? null : s.loading ? "loading" : "playing");

/** Play; a spinner while the sentence's clip is being cut; pause while it plays. */
function ClipIcon({ k, size = 12 }: { k: string; size?: number }) {
  const st = useClipState(clipOf(k));
  return st === "loading" ? <Loader2 size={size} className="spin" /> : st === "playing" ? <Pause size={size} className="fill-ico" /> : <Play size={size} className="fill-ico" />;
}
/** The icon and the button's words, which say 暂停 while the sentence plays. */
function ClipLabel({ k, label }: { k: string; label: string }) {
  const st = useClipState(clipOf(k));
  return <><ClipIcon k={k} />{st === "playing" ? t("暂停") : label}</>;
}

/** The sentence with the word in it set in bold. */
function Marked({ sentence, word }: { sentence: string; word: string }) {
  const parts = sentence.split(new RegExp(`(${wordRe(word).source})`, "i"));
  return <>{parts.map((p, k) => (k % 2 ? <mark key={k}>{p}</mark> : p))}</>;
}

export function WordsView() {
  useLang((s) => s.lang);
  useLearnOn();
  const { words, loaded, day, newToday } = useWords(useShallow((s) => ({ words: s.words, loaded: s.loaded, day: s.day, newToday: s.newToday })));
  const dailyNew = useStore((s) => s.settings.learnDaily);
  const on = useStore((s) => s.settings.learnOn);
  const detail = useWordDetail((s) => s.id);
  const [filter, setFilter] = useState<Filter>("due");
  const [query, setQuery] = useState("");
  const [reviewing, setReviewing] = useState(false);
  useEffect(() => { void useWords.getState().load(); return () => { stopClip(); useWordDetail.setState({ id: null }); }; }, []);
  const now = Date.now();
  const all = useMemo(() => Object.values(words).sort((a, b) => b.added - a.added), [words]);
  const queue = useMemo(() => reviewQueue({ words, day, newToday }, dailyNew), [words, day, newToday, dailyNew, reviewing]);
  const mastered = all.filter(isMastered);
  // The first cards of today's review get their audio ready while you look at the list.
  useEffect(() => { for (const w of queue.slice(0, 4)) prefetchClip(w.episodeId, w.start, w.end); }, [queue]);
  const q = query.trim().toLowerCase();
  const listed = filter === "due" ? queue : filter === "mastered" ? mastered : all;
  const shown = q ? listed.filter((w) => [w.id, w.word, w.meaning, w.ctx ?? "", w.sentence, w.episodeTitle].some((x) => x.toLowerCase().includes(q))) : listed;
  // The detail card steps through the list it was opened from (or all words, when it was opened from elsewhere).
  const detailList = detail && shown.some((w) => w.id === detail) ? shown : all;
  const closeDetail = useCallback(() => { stopClip(); useWordDetail.setState({ id: null }); }, []);

  if (reviewing) return <Review queue={queue} onDone={() => setReviewing(false)} />;
  return (
    <div className="page words-page">
      <div className="wb-head">
        <h1 className="page-title">{t("生词本")}</h1>
        {all.length > 0 && <span className="muted wb-count">{t("{n} 个词", { n: all.length })}</span>}
        <div className="wb-acts">
          {all.length > 0 && <ExportButtons words={all} />}
          <button className="btn sm primary" disabled={!queue.length} onClick={() => setReviewing(true)}>{queue.length ? t("复习 {n} 个", { n: queue.length }) : t("今天复习完了")}</button>
        </div>
      </div>
      {!on && <p className="wb-off muted small">{t("英语学习模式已关闭，生词本仍然保留。")} <button className="link-btn small" onClick={() => useStore.getState().openSettings("learn")}>{t("去打开")}</button></p>}
      {!loaded ? null : !all.length ? (
        <EmptyState art={<span className="wb-empty-ic"><BookMarked size={28} /></span>} title={t("生词本还是空的")} note={t("在英文节目的字幕里点一个单词，再点「加入生词本」。")} />
      ) : <>
        <LearnStats onOpen={(id) => useWordDetail.setState({ id })} />
        <div className="wb-bar">
          <div className="chips left">
            {([["due", t("待复习"), queue.length], ["all", t("全部"), all.length], ["mastered", t("已掌握"), mastered.length]] as const).map(([k, label, n]) => (
              <button key={k} className={`chip ${filter === k ? "on" : ""}`} onClick={() => setFilter(k)}>{label}<span className="n">{n}</span></button>
            ))}
          </div>
          {all.length > 6 && (
            <label className="wb-search"><Search size={13} /><input value={query} onChange={(e) => setQuery(e.target.value)} placeholder={t("搜索单词、释义或例句")} aria-label={t("搜索生词本")} /></label>
          )}
        </div>
        {!shown.length ? <p className="muted small wb-none">{q ? t("没有找到「{q}」", { q: query.trim() }) : filter === "due" ? t("今天没有要复习的词了。") : t("还没有掌握的词。连续答对，间隔超过 3 周就算掌握。")}</p> : (
          <div className="wb-list">
            {shown.map((w) => <WordRow key={w.id} w={w} now={now} />)}
          </div>
        )}
      </>}
      {detail && <WordDetail id={detail} list={detailList} onClose={closeDetail} />}
    </div>
  );
}

function WordRow({ w, now }: { w: Word; now: number }) {
  const due = dueLabel(w, now);
  const open = () => useWordDetail.setState({ id: w.id });
  return (
    <div className="wb-item" role="button" tabIndex={0} onClick={open} onKeyDown={(e) => { if (e.key === "Enter" && e.target === e.currentTarget) open(); }}>
      <div className="wb-main">
        <div className="wb-l1"><span className="wb-w">{w.id}</span>{w.ipa && <span className="wb-ipa">/{w.ipa}/</span>}{w.tag && <span className="pill">{t(w.tag)}</span>}</div>
        <p className="wb-d">{(w.ctx || w.meaning.split("\n")[0])}</p>
        <p className="wb-src">
          <button className="ts-link" onClick={(e) => { e.stopPropagation(); void playWord(w, true); }} title={t("播放原句")}><ClipIcon k={clipKey(w.episodeId, w.start, w.end)} size={10} />{clock(w.start)}</button>
          <span className="clamp1">{sourceOf(w)}</span>
        </p>
      </div>
      <span className={`due ${due.now ? "now" : ""}`}>{due.text}</span>
      <ChevronRight size={16} className="wb-go" />
    </div>
  );
}

/** A saved word in full: meanings, its sentence in the speaker's voice, where it came from, how review is going. */
function WordDetail({ id, list, onClose }: { id: string; list: Word[]; onClose: () => void }) {
  useLang((s) => s.lang);
  const w = useWords((s) => s.words[id]);
  const idx = list.findIndex((x) => x.id === id);
  const step = useCallback((d: number) => { const n = list[idx + d]; if (n) { stopClip(); useWordDetail.setState({ id: n.id }); } }, [list, idx]);
  useEffect(() => {
    for (const n of [list[idx], list[idx + 1], list[idx - 1]]) if (n) prefetchClip(n.episodeId, n.start, n.end);
  }, [id]);
  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.closest?.("input, textarea, select, button")) return;
      if (e.key === "ArrowLeft") { e.preventDefault(); step(-1); }
      else if (e.key === "ArrowRight") { e.preventDefault(); step(1); }
      else if (e.key === " " && w) { e.preventDefault(); void playWord(w, true); }
      else if (e.key.toLowerCase() === "w" && w) { e.preventDefault(); speak(w.id, 0.85); }
    };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [step, w]);
  if (!w) return null;
  const due = dueLabel(w, Date.now());
  const remove = async () => {
    if (!(await ask({ title: t("从生词本删除「{word}」？", { word: w.id }), message: t("复习记录会一起删除。"), ok: t("删除"), danger: true }))) return;
    const next = list[idx + 1] ?? list[idx - 1];
    stopClip();
    useWords.getState().remove(w.id);
    useWordDetail.setState({ id: next && next.id !== w.id ? next.id : null });
  };
  const k = clipKey(w.episodeId, w.start, w.end);
  return (
    <Modal title={w.id} onClose={onClose} head={
      <div className="wd-nav">
        <button className="icon-btn sm" disabled={idx <= 0} onClick={() => step(-1)} title={t("上一个（←）")} aria-label={t("上一个")}><ChevronLeft size={16} /></button>
        <span className="wd-num">{idx >= 0 ? `${idx + 1} / ${list.length}` : ""}</span>
        <button className="icon-btn sm" disabled={idx < 0 || idx >= list.length - 1} onClick={() => step(1)} title={t("下一个（→）")} aria-label={t("下一个")}><ChevronRight size={16} /></button>
      </div>
    }>
      <div className="wd">
        <div className="wd-head">
          <span className="wd-word">{w.id}</span>
          <button className="icon-btn sm rv-say" title={t("读单词（W）")} aria-label={t("读单词")} onClick={() => speak(w.id, 0.85)}><Volume2 size={16} /></button>
          {w.tag && <span className="pill">{t(w.tag)}</span>}
        </div>
        {w.ipa && <p className="wc-ipa">/{w.ipa}/</p>}
        {w.meaning ? <Meaning text={w.meaning} /> : <p className="wc-none muted small">{t("词典里没有这个词")}</p>}
        {w.ctx && <div className="wc-ctx"><span className="wc-ctx-h">{t("在这句里")}</span>{w.ctx}</div>}
        <div className="wd-ex">
          <span className="wd-h">{t("原句")}</span>
          <p className="wd-sent"><Marked sentence={w.sentence} word={w.word} /></p>
          {w.sentenceZh && <p className="wd-zh">{w.sentenceZh}</p>}
          <div className="wd-ex-foot">
            <button className="btn sm play" onClick={() => void playWord(w, true)}><ClipLabel k={k} label={t("听原声")} /><span className="kbd">␣</span></button>
            <button className="wd-src" onClick={() => void openSource(w)} title={t("打开这一集，从这句开始播放")}>
              <span className="clamp1">{sourceOf(w)}</span><span className="wd-at">{clock(w.start)}<ArrowUpRight size={13} /></span>
            </button>
          </div>
        </div>
        <div className="wd-foot">
          <span className="muted small">
            {w.card.reps ? t("复习 {n} 次", { n: w.card.reps }) : t("还没复习过")}
            {w.card.lapses > 0 && <> · {t("忘记 {n} 次", { n: w.card.lapses })}</>}
            {" · "}{w.card.state === 0 ? t("新词") : due.now ? t("今天要复习") : t("{wait}后复习", { wait: fmtWait(w.card.due - Date.now()) })}
            {" · "}{t("{date} 加入", { date: new Date(w.added).toLocaleDateString() })}
          </span>
          <button className="btn sm ghost danger" onClick={() => void remove()}><Trash2 size={13} />{t("删除")}</button>
        </div>
      </div>
    </Modal>
  );
}

function ExportButtons({ words }: { words: Word[] }) {
  return <>
    <button className="btn sm" onClick={() => void exportWords("anki", words)}><Download size={13} />{t("导出 Anki")}</button>
    <button className="btn sm ghost" onClick={() => void exportWords("csv", words)}>CSV</button>
  </>;
}

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const wordRe = (w: string, flags = "i") => new RegExp(`\\b${w.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\b`, flags);

let exporting = false;
/** Asks where to save, then writes the word book: an Anki package with each sentence's audio, or a CSV. */
export async function exportWords(kind: "anki" | "csv", words: Word[]) {
  if (exporting || !words.length) return;
  const notify = useStore.getState().notify;
  const name = `SubCast ${t("生词本")}`;
  const path = await saveDialog({ defaultPath: `${name}.${kind === "anki" ? "apkg" : "csv"}`, filters: [kind === "anki" ? { name: "Anki", extensions: ["apkg"] } : { name: "CSV", extensions: ["csv"] }] }).catch(() => null);
  if (!path) return;
  exporting = true;
  try {
    if (kind === "csv") {
      await api.saveText(path, toCsv(words, [t("单词"), t("音标"), t("释义"), t("在这句里"), t("原句"), t("译文"), t("节目"), t("单集"), t("时间"), t("加入日期")]));
    } else {
      // Every card carries its sentence in the speaker's voice: clips not cut yet are made now, a few at a time.
      const clips = new Map<string, string | null>();
      let done = 0;
      const todo = [...words];
      notify(t("正在准备 {n} 个词的原声…", { n: words.length }));
      await Promise.all([0, 1, 2].map(async () => {
        for (let w = todo.shift(); w; w = todo.shift()) {
          clips.set(w.id, await prepareClip(w.episodeId, w.start, w.end));
          if (++done % 10 === 0 && done < words.length) notify(t("正在准备原声 {done}/{n}…", { done, n: words.length }));
        }
      }));
      const notes: AnkiNote[] = words.map((w) => ({
        id: w.id, word: esc(w.id), ipa: esc(w.ipa), meaning: esc(w.meaning).replace(/\n/g, "<br>"), context: esc(w.ctx ?? ""),
        sentence: esc(w.sentence).replace(wordRe(esc(w.word)), (m) => `<b>${m}</b>`), translation: esc(w.sentenceZh ?? ""),
        source: esc([w.showTitle, w.episodeTitle, clock(w.start)].filter(Boolean).join(" · ")),
        tags: ["subcast", w.tag].filter(Boolean).map((x) => x.replace(/\s+/g, "_")).join(" "), clip: clips.get(w.id) ?? null,
      }));
      const withAudio = await api.exportAnki(path, name, `SubCast ${t("单词")}`, notes);
      notify(withAudio < words.length
        ? t("已导出 {n} 个词（{m} 个带原声）：{file}", { n: words.length, m: withAudio, file: path.split(/[\\/]/).pop() ?? "" })
        : t("已导出 {n} 个词：{file}", { n: words.length, file: path.split(/[\\/]/).pop() ?? "" }));
      void revealItemInDir(path).catch(() => {});
      return;
    }
    notify(t("已导出 {n} 个词：{file}", { n: words.length, file: path.split(/[\\/]/).pop() ?? "" }));
    void revealItemInDir(path).catch(() => {});
  } catch (e) { notify(t("导出失败：{why}", { why: errorText(e) })); }
  finally { exporting = false; }
}

const GRADES: [Grade, string, string][] = [[Rating.Again, "again", tk("忘了")], [Rating.Hard, "hard", tk("困难")], [Rating.Good, "good", tk("记得")], [Rating.Easy, "easy", tk("简单")]];

type Mode = "recall" | "spell";
const MODE_KEY = "subcast-review-mode";
const readMode = (): Mode => { try { return localStorage.getItem(MODE_KEY) === "spell" ? "spell" : "recall"; } catch { return "recall"; } };
const letter = (x: string) => x.toLowerCase().replace(/[’‘]/g, "'");

/** Which letters of a and of b belong to their longest common subsequence (the rest were added or missed). */
function common(a: string, b: string): [boolean[], boolean[]] {
  const A = letter(a), B = letter(b);
  const L = Array.from({ length: A.length + 1 }, () => new Array<number>(B.length + 1).fill(0));
  for (let x = A.length - 1; x >= 0; x--) for (let y = B.length - 1; y >= 0; y--) L[x][y] = A[x] === B[y] ? L[x + 1][y + 1] + 1 : Math.max(L[x + 1][y], L[x][y + 1]);
  const ma = new Array<boolean>(A.length).fill(false), mb = new Array<boolean>(B.length).fill(false);
  for (let x = 0, y = 0; x < A.length && y < B.length;) {
    if (A[x] === B[y]) { ma[x++] = true; mb[y++] = true; } else if (L[x + 1][y] >= L[x][y + 1]) x++; else y++;
  }
  return [ma, mb];
}

/** A wrong spelling against the answer: extra letters in what was typed, and missed ones in the answer. */
function Diff({ typed, answer }: { typed: string; answer: string }) {
  const [mt, ma] = common(typed, answer);
  return <>
    <span className="sp-typed">{[...typed].map((c, k) => <span key={k} className={mt[k] ? "" : "x"}>{c}</span>)}</span>
    <span className="sp-arrow">→</span>
    <span className="sp-ans">{[...answer].map((c, k) => <span key={k} className={ma[k] ? "" : "x"}>{c}</span>)}</span>
  </>;
}

/** What is being typed, letter by letter against the answer. */
function Letters({ typed, answer }: { typed: string; answer: string }) {
  const cells = [];
  for (let k = 0; k < Math.max(answer.length, typed.length); k++) {
    const c = typed[k];
    const cls = c == null ? (k === typed.length ? "cur" : "") : letter(c) === letter(answer[k] ?? "") ? "ok" : "bad";
    cells.push(<span key={k} className={`sp-c ${cls}`}>{c ?? ""}</span>);
  }
  return <span className="sp-row">{cells}</span>;
}

/** The spelling front: type the blanked word, each letter checked as you go (after qwerty-learner). */
// Confetti on our own canvas (no worker: blob workers aren't something to rely on in every web view).
let fire: confetti.CreateTypes | null = null;
let fireLoading: Promise<confetti.CreateTypes> | null = null;
const CONFETTI = ["#e8582a", "#f2a679", "#f2d091", "#5d8c7b", "#3a6df0"];
function getFire(): Promise<confetti.CreateTypes> {
  if (fire) return Promise.resolve(fire);
  if (!fireLoading) {
    fireLoading = import("canvas-confetti").then((mod) => {
      const c = document.createElement("canvas");
      c.className = "confetti";
      document.body.appendChild(c);
      fire = mod.default.create(c, { resize: true, useWorker: false });
      return fire;
    });
  }
  return fireLoading;
}
function confettiAt(opts: confetti.Options) {
  if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
  void getFire().then((f) => f({ colors: CONFETTI, shapes: ["square", "circle"], scalar: 0.9, ...opts }));
}
/** A burst from an element (a word spelled right). */
function burst(el: Element | null) {
  const r = el?.getBoundingClientRect();
  const origin = r ? { x: (r.left + r.width / 2) / window.innerWidth, y: (r.top + r.height / 2) / window.innerHeight } : { x: 0.5, y: 0.5 };
  confettiAt({ particleCount: 60, spread: 70, startVelocity: 30, ticks: 140, origin });
}
/** From both sides (a finished review), like qwerty-learner's chapter end. */
function celebrate() {
  window.setTimeout(() => confettiAt({ particleCount: 60, angle: 60, spread: 100, origin: { x: 0, y: 0.7 }, ticks: 260 }), 150);
  window.setTimeout(() => confettiAt({ particleCount: 60, angle: 120, spread: 100, origin: { x: 1, y: 0.7 }, ticks: 260 }), 300);
}

function Spell({ w, onCheck, onExit }: { w: Word; onCheck: (r: { ok: boolean; typed: string; hints: number }) => void; onExit: () => void }) {
  const [typed, setTyped] = useState("");
  const [hints, setHints] = useState(0);
  const [shake, setShake] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const row = useRef<HTMLSpanElement>(null);
  const answer = w.word;
  const finish = (v: string) => {
    const ok = letter(v) === letter(answer);
    if (ok) { rightSound(); burst(row.current); } else { wrongSound(); }
    onCheck({ ok, typed: v, hints });
  };
  // Each letter is checked as it is typed: a click when right, a buzz and a shake when wrong; the whole word
  // spelled right finishes at once (no Enter needed).
  const change = (raw: string) => {
    const v = raw.replace(/\s/g, "");
    if (v.length > typed.length) {
      const k = v.length - 1;
      if (letter(v[k]) === letter(answer[k] ?? "")) keySound(); else { wrongSound(); setShake((n) => n + 1); }
    }
    setTyped(v);
    if (letter(v) === letter(answer)) finish(v);
  };
  const hint = () => {
    let k = 0;
    while (k < typed.length && letter(typed[k]) === letter(answer[k])) k++;
    if (k >= answer.length) return;
    setTyped(answer.slice(0, k + 1)); setHints((h) => h + 1);
    input.current?.focus();
  };
  return (
    <div className="sp" onClick={() => input.current?.focus()}>
      <span ref={row} key={shake} className={shake ? "sp-shake" : ""}><Letters typed={typed} answer={answer} /></span>
      <input ref={input} className="sp-in" autoFocus value={typed} maxLength={answer.length + 3} spellCheck={false} autoCapitalize="off" autoComplete="off" aria-label={t("拼写这个词")}
        onChange={(e) => change(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") { e.preventDefault(); finish(typed); }
          else if (e.key === "Tab") { e.preventDefault(); hint(); }
          else if (e.key === " ") { e.preventDefault(); void playWord(w); }
          else if (e.key === "Escape") onExit();
        }} />
      <p className="sp-help">{t("{n} 个字母", { n: answer.length })} · <span className="kbd">Tab</span> {t("提示一个字母")} · <span className="kbd">␣</span> {t("重听")} · <span className="kbd">↵</span> {t("检查")}</p>
    </div>
  );
}

/** Spaced review: the front plays the sentence with the word blanked out (to recall, or to spell); the back
 *  shows it, then you rate it. */
function Review({ queue, onDone }: { queue: Word[]; onDone: () => void }) {
  useLang((s) => s.lang);
  const [list] = useState(queue);
  const [i, setI] = useState(0);
  const [flipped, setFlipped] = useState(false);
  const [mode, setModeState] = useState<Mode>(readMode);
  const [spelled, setSpelled] = useState<{ ok: boolean; typed: string; hints: number } | null>(null);
  const setMode = (m: Mode) => { setModeState(m); setFlipped(false); setSpelled(null); try { localStorage.setItem(MODE_KEY, m); } catch { /* ignore */ } };
  // The answer Enter takes: a spelling result decides it; otherwise 记得.
  const suggest: Grade = spelled ? (spelled.ok ? (spelled.hints ? Rating.Hard : Rating.Good) : Rating.Again) : Rating.Good;
  const w = list[i];
  const gaps = useMemo(() => (w ? intervals(useWords.getState().words[w.id] ?? w) : null), [w, flipped]);
  useEffect(() => {
    if (w) void playWord(w);
    for (const n of list.slice(i + 1, i + 4)) prefetchClip(n.episodeId, n.start, n.end);
    return () => stopClip();
  }, [i]);
  // The answer side says the word itself (system voice, instant).
  useEffect(() => { if (flipped && w) speak(w.id, 0.85); }, [flipped]);
  const rate = useCallback((g: Grade) => {
    if (!w) return;
    useWords.getState().rate(w.id, g);
    // A word you forgot comes back at the end of this session.
    if (g === Rating.Again) list.push(w);
    setFlipped(false);
    setSpelled(null);
    setI((x) => x + 1);
  }, [w, list]);
  useEffect(() => {
    const k = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.closest?.("input, textarea, select")) return;
      // Spelling: keys go to the letters, wherever focus wandered.
      if (mode === "spell" && !flipped && e.key !== "Escape" && !e.metaKey && !e.ctrlKey) { document.querySelector<HTMLInputElement>(".sp-in")?.focus(); return; }
      if (e.key === " " || e.key === "Enter") { e.preventDefault(); if (!flipped) setFlipped(true); else rate(suggest); }
      else if (flipped && ["1", "2", "3", "4"].includes(e.key)) { e.preventDefault(); rate(Number(e.key) as Grade); }
      else if (e.key.toLowerCase() === "r") { e.preventDefault(); if (w) void playWord(w); }
      else if (flipped && e.key.toLowerCase() === "w") { e.preventDefault(); if (w) speak(w.id, 0.85); }
      else if (e.key === "Escape") onDone();
    };
    window.addEventListener("keydown", k);
    return () => window.removeEventListener("keydown", k);
  }, [flipped, rate, w, onDone, suggest, mode]);

  const finished = !w && list.length > 0;
  useEffect(() => { if (finished) { doneSound(); celebrate(); } }, [finished]);
  if (!w) return (
    <div className="page review-page">
      <EmptyState art={<span className="wb-empty-ic done"><Check size={28} /></span>} title={t("今天的复习完成了")} note={t("复习了 {n} 个词，明天再来。", { n: new Set(list.map((x) => x.id)).size })}>
        <button className="btn primary" onClick={onDone}>{t("回到生词本")}</button>
      </EmptyState>
    </div>
  );
  const blank = (() => {
    const r = w.sentence.replace(wordRe(w.word), " _____ ");
    if (r !== w.sentence) return r;
    if (w.id && w.id !== w.word) return w.sentence.replace(wordRe(w.id), " _____ ");
    return r;
  })();
  return (
    <div className="page review-page">
      <div className="rv-top">
        <button className="icon-btn sm" onClick={onDone} title={t("结束复习（Esc）")} aria-label={t("结束复习")}><X size={16} /></button>
        <span className="rv-meter"><i style={{ width: `${(i / list.length) * 100}%` }} /></span>
        <span className="rv-num">{i + 1} / {list.length}</span>
        <div className="seg-control" role="radiogroup" aria-label={t("复习方式")}>
          {([["recall", t("回想")], ["spell", t("拼写")]] as const).map(([m, label]) => <button key={m} role="radio" aria-checked={mode === m} className={mode === m ? "on" : ""} onClick={() => setMode(m)}>{label}</button>)}
        </div>
      </div>
      <div className="rv-card">
        {flipped
          ? <button className="rv-src rv-src-link" onClick={() => void openSource(w)} title={t("打开这一集，从这句开始播放")}><span className="clamp1">{sourceOf(w)}</span><span className="wd-at">{clock(w.start)}<ArrowUpRight size={12} /></span></button>
          : <span className="rv-src clamp1">{sourceOf(w)} · {clock(w.start)}</span>}
        {flipped ? <>
          {spelled && (spelled.ok
            ? <p className="sp-result ok"><span className="sp-tick"><Check size={16} strokeWidth={3} /></span>{spelled.hints ? t("拼对了（用了 {n} 次提示）", { n: spelled.hints }) : t("拼对了！")}</p>
            : <p className="sp-result bad">{spelled.typed ? <Diff typed={spelled.typed} answer={w.word} /> : t("没拼出来")}</p>)}
          <p className="rv-word">{w.id}<button className="icon-btn sm rv-say" title={t("读单词（W）")} aria-label={t("读单词")} onClick={() => speak(w.id, 0.85)}><Volume2 size={16} /></button></p>
          {w.ipa && <p className="rv-ipa">/{w.ipa}/</p>}
          <div className="rv-def"><Meaning text={w.meaning} /></div>
          {w.ctx && <p className="rv-ctx"><span className="wc-ctx-h">{t("在这句里")}</span>{w.ctx}</p>}
          <p className="rv-sent"><Marked sentence={w.sentence} word={w.word} />{w.sentenceZh && <span className="rv-zh">{w.sentenceZh}</span>}</p>
        </> : <>
          <p className="rv-q">{mode === "spell" ? t("听原句，拼出空着的词") : t("这句话里空着的是哪个词？")}</p>
          <p className="rv-sent front">{blank}</p>
          {mode === "spell" && <>
            <p className="sp-mean">{w.ctx || w.meaning.split("\n")[0]}</p>
            <Spell key={`${i}-${w.id}`} w={w} onExit={onDone} onCheck={(r) => { setSpelled(r); setFlipped(true); }} />
          </>}
        </>}
        <button className="btn sm play rv-listen" onClick={() => void playWord(w, true)}><ClipLabel k={clipKey(w.episodeId, w.start, w.end)} label={t("再听原句")} /><span className="kbd">{mode === "spell" && !flipped ? "␣" : "R"}</span></button>
        {flipped ? (
          <div className="rv-rating">
            <p className="rv-rate-q">{t("记得怎么样？选一个，决定下次什么时候再考")}</p>
            <div className="rv-rate">
              {GRADES.map(([g, cls, label], k) => (
                <button key={g} className={`btn ${cls}${g === suggest ? " suggested" : ""}`} onClick={() => rate(g)} title={t("按 {k}", { k: k + 1 })}>
                  <span className="t">{t(label)}</span>
                  <span className="s">{gaps ? t("{wait}后再考", { wait: fmtWait(gaps[g]) }) : ""}</span>
                </button>
              ))}
            </div>
            <p className="rv-keys">{t("键盘 1–4 选择，空格选「{label}」", { label: t(GRADES.find(([g]) => g === suggest)![2]) })}</p>
          </div>
        ) : (
          mode === "spell"
            ? <button className="btn ghost rv-show" onClick={() => { setSpelled({ ok: false, typed: "", hints: 0 }); setFlipped(true); }}>{t("不会，看答案")}</button>
            : <button className="btn primary rv-show" onClick={() => setFlipped(true)}>{t("显示答案")}<span className="kbd">␣</span></button>
        )}
      </div>
    </div>
  );
}

export { useDueCount, remindReview };

