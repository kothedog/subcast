// 跟读模式. The player stops after each sentence (sentence-sized pieces of the subtitle lines, src/lib/pieces.ts) and
// waits for the listener to say it back:
//  - 跟读打分 (default when a local speech model is installed): the microphone is heard by that model
//    (src-tauri/src/shadow.rs) about once a second while they speak, the words they got light up, and the next
//    sentence plays once enough of them were said. Three misses in a row and it moves on anyway.
//  - 定时停顿: the old way, a pause about as long as the sentence, without a microphone.
import { useEffect, useMemo, useRef, useState } from "react";
import { create } from "zustand";
import { listen } from "@tauri-apps/api/event";
import { Check, Mic, MicOff, RotateCcw, SkipForward, Volume2 } from "lucide-react";
import { api, errorText, type Segment } from "../lib/api";
import { timedPieces } from "../lib/pieces";
import { useStore } from "../store";
import { t, useLang } from "../lib/i18n";
import { compare } from "./Learn";
import { Seg } from "./ReadingSettings";
import { rightSound } from "../lib/sfx";

interface Piece { start: number; end: number; text: string }
type Phase = "playing" | "listening" | "passed" | "missed" | "waiting";

interface Shadow {
  phase: Phase;
  piece: Piece | null;
  words: { word: string; ok: boolean }[] | null;
  level: number;
  tries: number;
  /** Why scoring is off this time (no model, no microphone…); timed pauses are used instead. */
  fallback: string | null;
  /** The microphone gives only silence: most likely access is turned off in the system settings. */
  silent: boolean;
  said: number;
  scoreSum: number;
  /** Timed pause countdown: total ms and end timestamp. */
  waitTotal: number;
  waitUntil: number;
}
const initial: Shadow = { phase: "playing", piece: null, words: null, level: 0, tries: 0, fallback: null, silent: false, said: 0, scoreSum: 0, waitTotal: 0, waitUntil: 0 };
export const useShadow = create<Shadow>(() => initial);

/** Louder than this counts as speaking. */
const VOICE = 0.05;
/** Speech is over after this much quiet. */
const QUIET_MS = 1200;
/** The speaker's last syllable is still in the room for a moment after the pause. */
const DEAF_MS = 350;
const MAX_TRIES = 3;

/** Enough of the sentence was said: everything for very short ones, about 70% otherwise. */
function passes(words: { ok: boolean }[]): boolean {
  const n = words.length;
  if (!n) return true;
  const got = words.filter((w) => w.ok).length;
  return n <= 3 ? got === n : got >= Math.ceil(n * 0.7);
}

function piecesOf(segments: Segment[]): Piece[] {
  const cjk = (s: string) => /[一-鿿]/.test(s);
  return segments.flatMap((s) => timedPieces(s.text, cjk(s.text), s.start, s.end));
}

export function useShadowing(on: boolean, epId: string, segments: Segment[], language: string) {
  const score = useStore((s) => s.settings.shadowScore !== false);
  const model = useStore((s) => s.settings.asrModel);
  const pieces = useMemo(() => piecesOf(segments), [segments]);

  useEffect(() => {
    if (!on) return;
    let alive = true;
    let resume: number | undefined;
    let lastEnd = -1;
    let scoring = false;
    // Listening state, reset at each turn.
    let turnAt = 0, lastVoice = 0, spoke = false, inflight: Promise<void> | null = null, lastHeard = 0, tick: number | undefined, quietSince = 0, loudest = 0;
    useShadow.setState({ ...initial });

    const play = () => { const st = useStore.getState(); if (st.current?.id === epId && !st.playing) st.player?.play(); };
    const heard = (text: string) => { const p = useShadow.getState().piece; return p ? compare(p.text, text) : []; };

    const stopListening = () => { window.clearInterval(tick); tick = undefined; };
    const next = () => { stopListening(); useShadow.setState({ phase: "playing", words: null, tries: 0 }); play(); };
    const newTurn = () => {
      void api.shadowClear();
      turnAt = Date.now(); spoke = false; lastHeard = 0; quietSince = Date.now();
      useShadow.setState({ phase: "listening", words: null });
      window.clearInterval(tick);
      tick = window.setInterval(onTick, 200);
    };
    const hear = (final: boolean) => {
      const run = api.shadowHear(model, language).then((text) => {
        if (!alive || useShadow.getState().phase !== "listening") return;
        const words = heard(text);
        if (!final) { if (text) useShadow.setState({ words }); return; }
        stopListening();
        const got = words.length ? words.filter((w) => w.ok).length / words.length : 0;
        if (passes(words)) {
          rightSound();
          useShadow.setState((s) => ({ phase: "passed", words, said: s.said + 1, scoreSum: s.scoreSum + got }));
          resume = window.setTimeout(() => { if (alive && useShadow.getState().phase === "passed") next(); }, 700);
        } else {
          const tries = useShadow.getState().tries + 1;
          useShadow.setState((s) => ({ phase: "missed", words, tries, ...(tries >= MAX_TRIES ? { said: s.said + 1, scoreSum: s.scoreSum + got } : {}) }));
          if (tries >= MAX_TRIES) resume = window.setTimeout(() => { if (alive && useShadow.getState().phase === "missed") next(); }, 1600);
          else { void api.shadowClear(); spoke = false; tick = window.setInterval(onMissedTick, 200); }
        }
      }).catch((e) => {
        if (!alive) return;
        stopListening();
        useShadow.setState({ phase: "waiting", fallback: errorText(e) });
      });
      inflight = run.finally(() => { inflight = null; });
    };
    function onTick() {
      if (useShadow.getState().phase !== "listening") return;
      const now = Date.now();
      if (!spoke) return;
      // Partial results while speaking, one at a time.
      if (!inflight && now - lastHeard > 1000 && now - lastVoice < QUIET_MS) { lastHeard = now; hear(false); }
      if (now - lastVoice >= QUIET_MS) {
        stopListening();
        const finish = () => { if (alive && useShadow.getState().phase === "listening") hear(true); };
        if (inflight) void inflight.then(finish); else finish();
      }
    }
    // After a miss, speaking again starts the next try (what was said before is already cleared).
    function onMissedTick() {
      if (useShadow.getState().phase !== "missed") { window.clearInterval(tick); return; }
      if (spoke) { useShadow.setState({ phase: "listening", words: null }); turnAt = Date.now() - DEAF_MS; lastHeard = 0; window.clearInterval(tick); tick = window.setInterval(onTick, 200); }
    }

    const un = listen<number>("shadow-level", (e) => {
      const lv = e.payload;
      const now = Date.now();
      if (now - turnAt < DEAF_MS) return;
      useShadow.setState({ level: lv });
      loudest = Math.max(loudest, lv);
      if (lv > VOICE) { lastVoice = now; spoke = true; }
      // Ten seconds of exactly nothing, not even room noise: the system isn't letting us hear.
      const phase = useShadow.getState().phase;
      if ((phase === "listening" || phase === "missed") && loudest === 0 && now - quietSince > 10000 && !useShadow.getState().silent) useShadow.setState({ silent: true });
    });

    const start = async () => {
      if (!score) return;
      if (!(await api.shadowReady(model).catch(() => false))) { useShadow.setState({ fallback: t("还没有本机语音模型，先用定时停顿。在 设置 › 语音识别 下载一个模型后就能跟读打分。") }); return; }
      try { await api.shadowStart(); scoring = true; } catch (e) { useShadow.setState({ fallback: t("麦克风打不开：{error}", { error: errorText(e) }) }); }
    };
    void start();

    const unsub = useStore.subscribe((s, prev) => {
      if (s.current?.id !== epId) return;
      // Played on by hand (space bar, the play button): the turn is over.
      if (s.playing && !prev.playing) {
        const ph = useShadow.getState().phase;
        if (ph !== "playing") { stopListening(); window.clearTimeout(resume); useShadow.setState({ phase: "playing", words: null, tries: 0 }); }
      }
      if (!s.playing || s.time === prev.time) return;
      // The sentence that just finished: playback crossed its end since the last update.
      const p = pieces.find((x) => prev.time < x.end && s.time >= x.end && s.time - x.end < 1.5);
      if (!p || p.end === lastEnd) return;
      lastEnd = p.end;
      s.player?.toggle();
      useShadow.setState({ piece: p, tries: 0, words: null });
      if (scoring && !useShadow.getState().fallback) { newTurn(); return; }
      const gap = Math.min(12, Math.max(1.5, (p.end - p.start) * 1.1 + 0.6));
      const ms = Math.round(gap * 1000);
      useShadow.setState({ phase: "waiting", waitTotal: ms, waitUntil: Date.now() + ms });
      resume = window.setTimeout(() => { if (alive && useShadow.getState().phase === "waiting") next(); }, ms);
    });

    // The buttons in the bar.
    controls = {
      again: () => { window.clearTimeout(resume); newTurn(); },
      replay: () => {
        const p = useShadow.getState().piece;
        window.clearTimeout(resume); stopListening();
        if (!p) return;
        lastEnd = -1;
        useShadow.setState({ phase: "playing", words: null });
        const st = useStore.getState();
        st.player?.seek(p.start);
        play();
      },
      skip: () => { window.clearTimeout(resume); next(); },
    };

    return () => {
      alive = false;
      controls = null;
      unsub(); stopListening(); window.clearTimeout(resume);
      void un.then((f) => f());
      if (scoring) void api.shadowStop();
      useShadow.setState({ ...initial });
    };
  }, [on, epId, pieces, score, model, language]);
}

let controls: { again: () => void; replay: () => void; skip: () => void } | null = null;

export function ShadowBar({ onExit }: { onExit: () => void }) {
  useLang((s) => s.lang);
  const { phase, piece, words, level, tries, fallback, silent, said, scoreSum, waitTotal, waitUntil } = useShadow();
  const score = useStore((s) => s.settings.shadowScore !== false);
  const save = useStore((s) => s.saveSettings);
  const scoring = score && !fallback;
  const lastPhase = useRef(phase);
  lastPhase.current = phase;
  const [waitLeft, setWaitLeft] = useState(0);
  useEffect(() => {
    if (phase !== "waiting" || !waitUntil) { setWaitLeft(0); return; }
    const update = () => setWaitLeft(Math.max(0, waitUntil - Date.now()));
    update();
    const id = window.setInterval(update, 100);
    return () => window.clearInterval(id);
  }, [phase, waitUntil]);
  const waitPct = waitTotal > 0 ? Math.min(100, Math.max(0, (waitLeft / waitTotal) * 100)) : 0;
  const status =
    !scoring ? (phase === "waiting" ? t("轮到你了，跟着说一遍（{s}s 后继续）", { s: Math.max(1, Math.ceil(waitLeft / 1000)) }) : t("跟读模式：每句说完会停一下，跟着说一遍"))
    : phase === "listening" ? t("轮到你了，跟着说这一句")
    : phase === "passed" ? t("说得好")
    : phase === "missed" ? (tries >= MAX_TRIES ? t("先往下听，之后再回来练") : t("还差一点，再说一遍，或者重听原句"))
    : t("听原声，说完会停下来等你跟读");
  return (
    <div className={`shadow-bar ${scoring ? "scoring" : ""}`}>
      <div className="shadow-head">
        {phase === "passed" ? <Check size={13} /> : silent ? <MicOff size={13} /> : <Mic size={13} />}
        <span className="shadow-status">{status}</span>
        {scoring && phase === "listening" && <span className="shadow-level" aria-hidden><span style={{ transform: `scaleX(${Math.min(1, level * 4)})` }} /></span>}
        {scoring && said > 0 && <span className="shadow-score num" title={t("这次跟读的句数和平均说中的单词比例")}>{t("{n} 句 · {p}%", { n: said, p: Math.round((scoreSum / said) * 100) })}</span>}
        <Seg value={score ? "score" : "timed"} options={[["score", t("打分")], ["timed", t("定时")]]} onChange={(v) => void save({ shadowScore: v === "score" })} />
        <button className="btn ghost sm" onClick={onExit}>{t("退出")}</button>
      </div>
      {scoring && piece && phase !== "playing" && (
        <p className="shadow-line">
          {(words ?? compare(piece.text, "")).map((w, i) => (
            <span key={i} className={w.ok ? "ok" : phase === "missed" || (phase === "passed" && words) ? "miss" : ""}>{w.word} </span>
          ))}
        </p>
      )}
      {!scoring && piece && phase === "waiting" && (
        <>
          <p className="shadow-line">{piece.text}</p>
          <div className="shadow-wait-bar" aria-hidden><span style={{ width: `${waitPct}%` }} /></div>
          <div className="shadow-actions">
            <button className="btn ghost sm" onClick={() => controls?.replay()}><Volume2 size={13} /> {t("重听原句")}</button>
            <button className="btn ghost sm" onClick={() => controls?.skip()}><SkipForward size={13} /> {t("下一句")}</button>
          </div>
        </>
      )}
      {scoring && piece && (phase === "missed" || phase === "listening") && (
        <div className="shadow-actions">
          {phase === "missed" && tries < MAX_TRIES && <button className="btn ghost sm" onClick={() => controls?.again()}><RotateCcw size={13} /> {t("再说一遍")}</button>}
          <button className="btn ghost sm" onClick={() => controls?.replay()}><Volume2 size={13} /> {t("重听原句")}</button>
          <button className="btn ghost sm" onClick={() => controls?.skip()}><SkipForward size={13} /> {t("跳过")}</button>
        </div>
      )}
      {silent && <p className="shadow-note">{t("麦克风没有声音。可能是系统没允许 SubCast 使用麦克风。")} <button className="link-btn" onClick={() => void api.openMicSettings()}>{t("打开系统设置")}</button></p>}
      {score && fallback && <p className="shadow-note">{fallback}</p>}
    </div>
  );
}
