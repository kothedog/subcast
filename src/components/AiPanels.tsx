import { create } from "zustand";
import { useShallow } from "zustand/react/shallow";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ArrowDown, ArrowUp, ChevronDown, Loader2, RefreshCw, Settings2, Square, SquarePen } from "lucide-react";
import { api, errorText, rawErrorText, type ChatMessage, type Episode } from "../lib/api";
import { askEpisode, summaryPrompt, transcriptForModel } from "../lib/ai";
import { t, tk } from "../lib/i18n";
import { quickPrompts } from "../lib/settings";
import { ChatPrefsDialog, ModelPicker, PromptMenu, promptIcon, shortModel } from "./ChatTools";
import { llmReady, useJobs } from "../lib/jobs";
import { warmAgent } from "../lib/agents";
import { providerFor, seekSoon, useStore } from "../store";
import { useClipTools } from "../lib/clipTools";
import { Markdown } from "./Markdown";
import { AnswerActions, CopyMenu, HistoryMenu, QuestionActions, type ArchivedChat } from "./ChatActions";
import { ReadAloud } from "./ReadAloud";
import { AskArt, NightNote, PageArt, PlugArt } from "./Illustrations";
import { createManualClip, rangeFromClock } from "../lib/highlights";
import { openVideoClipDialog } from "./VideoClipDialog";

/** Whether a model is connected for Q&A; re-checked whenever settings change or the settings window closes. */
function useConnected() {
  const { chatProvider, chatModel, providers } = useStore(useShallow((s) => ({
    chatProvider: s.settings.chatProvider,
    chatModel: s.settings.chatModel,
    providers: s.settings.providers,
  })));
  const open = useStore((s) => s.settingsOpen);
  const [ok, setOk] = useState<boolean | null>(null);
  useEffect(() => { if (!open) void llmReady("chat").then(setOk); }, [chatProvider, chatModel, providers, open]);
  return ok;
}

function ConnectCard({ what }: { what: string }) {
  const [later, setLater] = useState(false);
  const connect = () => {
    const s = useStore.getState();
    void s.saveSettings({ chatProvider: s.settings.chatProvider || "deepseek" });
    s.openSettings("model");
  };
  if (later) {
    return (
      <div className="setup-note">
        <span>{t("{what}还差一个模型", { what: t(what) })}</span>
        <button className="btn primary sm" onClick={connect}>{t("连接模型")}</button>
      </div>
    );
  }
  return (
    <div className="connect-card">
      <PlugArt className="cc-art" />
      <p className="es-title">{t("{what}还差一个模型", { what: t(what) })}</p>
      <p className="muted">{t("推荐 DeepSeek，国内直连，一集约 ¥0.1。Key 只保存在本机。")}</p>
      <div className="row-actions center">
        <button className="btn primary" onClick={connect}>{t("连接模型")}</button>
        <button className="btn ghost" onClick={() => setLater(true)}>{t("稍后")}</button>
      </div>
    </div>
  );
}

function useReady(ep: Episode) {
  const transcript = useJobs((s) => s.transcripts[ep.id]);
  const translation = useJobs((s) => s.translations[ep.id]);
  const { chatProvider, chatModel, providers } = useStore(useShallow((s) => ({
    chatProvider: s.settings.chatProvider,
    chatModel: s.settings.chatModel,
    providers: s.settings.providers,
  })));
  const provider = useMemo(() => {
    const p = providers[chatProvider];
    return p && chatModel ? { ...p, model: chatModel } : p;
  }, [providers, chatProvider, chatModel]);
  const context = () => transcriptForModel(transcript!.segments, translation?.items);
  // Errors that the settings window fixes carry `setup`, so the panel can offer a way there whatever the language.
  const setupError = (message: string) => Object.assign(new Error(message), { setup: true });
  const check = async () => {
    if (!transcript) throw new Error(t("需要先有字幕才能提问"));
    if (!provider?.model) throw setupError(t("请先在设置里选择模型"));
    if (provider.needsKey && !(await api.hasApiKey(provider.id))) throw setupError(t("请先在设置里填写「{name}」的 API Key", { name: t(provider.name) }));
  };
  return { transcript, provider, context, check };
}

/** Stable per episode, so memoised answers don't re-render when the player changes. */
function useSeek(ep: Episode) {
  return useCallback((t: number) => {
    const s = useStore.getState();
    if (s.current?.id === ep.id) s.player?.seek(t);
    else { s.load({ ...ep, progress: t }); seekSoon(t); }
  }, [ep.id]);
}


// How often each quick question was used, on this computer; the most used come first.
const USE_KEY = "subcast-prompt-use";
function readUse(): Record<string, number> { try { return JSON.parse(localStorage.getItem(USE_KEY) || "{}"); } catch { return {}; } }
function countUse(id: string) { try { const u = readUse(); u[id] = (u[id] ?? 0) + 1; localStorage.setItem(USE_KEY, JSON.stringify(u)); } catch { /* storage off */ } }
/** Stable: unused prompts keep their configured order behind the used ones. */
function byUse<T extends { id: string }>(list: T[]): T[] {
  const u = readUse();
  return list.map((p, i) => ({ p, i })).sort((a, b) => (u[b.p.id] ?? 0) - (u[a.p.id] ?? 0) || a.i - b.i).map((x) => x.p);
}

/** Each episode's conversation and the answer being written, kept outside the panel: switching tabs or episodes
 *  mid-answer neither drops the streamed text nor shows a stale copy when the panel comes back. */
interface ChatState { messages: ChatMessage[]; streaming: string | null; loaded: boolean }
const useChats = create<Record<string, ChatState>>(() => ({}));
const chatAborts = new Map<string, AbortController>();
const putChat = (id: string, p: Partial<ChatState>) => useChats.setState((s) => ({ [id]: { ...(s[id] ?? { messages: [], streaming: null, loaded: false }), ...p } }));
const NO_MESSAGES: ChatMessage[] = [];

export function ChatPanel({ ep }: { ep: Episode }) {
  const { transcript, provider, context, check } = useReady(ep);
  const connected = useConnected();
  // A local agent (Claude Code…) is started in the background as the panel opens, so the first answer is quicker.
  const { chatProvider, chatPromptOn, chatPrompt, savedPrompts } = useStore(useShallow((s) => ({
    chatProvider: s.settings.chatProvider,
    chatPromptOn: s.settings.chatPromptOn,
    chatPrompt: s.settings.chatPrompt,
    savedPrompts: s.settings.quickPrompts,
  })));
  useEffect(() => { warmAgent(providerFor(useStore.getState().settings, "chat")); }, [chatProvider]);
  const seek = useSeek(ep);
  const translation = useJobs((s) => s.translations[ep.id]);
  const clipTools = useClipTools();
  const clipRange = useMemo(() => {
    if (!clipTools || !ep.videoId || !transcript?.segments.length) return undefined;
    return (start: number, end?: number) => {
      const { startIdx, endIdx } = rangeFromClock(transcript.segments, start, end);
      const clip = createManualClip(transcript.segments, translation?.items, startIdx, endIdx, ep);
      openVideoClipDialog({ ep, segments: transcript.segments, zh: translation?.items, clip });
    };
  }, [ep, transcript, translation?.items, clipTools]);
  const messages = useChats((s) => s[ep.id]?.messages ?? NO_MESSAGES);
  const streaming = useChats((s) => s[ep.id]?.streaming ?? null);
  const [input, setInput] = useState("");
  const [setupError, setSetupError] = useState<string | null>(null);
  const [prefsOpen, setPrefsOpen] = useState(false);
  const [archive, setArchive] = useState<ArchivedChat[]>([]);
  const [atBottom, setAtBottom] = useState(true);
  const scroller = useRef<HTMLDivElement>(null);
  const end = useRef<HTMLDivElement>(null);
  const box = useRef<HTMLTextAreaElement>(null);
  // The four the listener uses most (the first four by default) are shown; the rest open with "More".
  const [allPrompts, setAllPrompts] = useState(false);
  const prompts = useMemo(() => byUse(quickPrompts({ quickPrompts: savedPrompts }).filter((p) => p.enabled)), [savedPrompts]);
  const shownPrompts = allPrompts ? prompts : prompts.slice(0, 4);

  useEffect(() => {
    setArchive([]);
    if (!useChats.getState()[ep.id]?.loaded) {
      api.getBlob(ep.id, "chat").then((raw) => {
        if (useChats.getState()[ep.id]?.loaded) return;
        let list: ChatMessage[] = [];
        try { list = raw ? JSON.parse(raw) : []; } catch { /* ignore */ }
        putChat(ep.id, { messages: list, loaded: true });
      });
    }
    api.getBlob(ep.id, "chat-archive").then((raw) => { if (raw) try { setArchive(JSON.parse(raw)); } catch { /* ignore */ } });
  }, [ep.id]);
  // Follow new text only while the reader is at the bottom; scrolling up to reread is never interrupted.
  useEffect(() => { if (atBottom) end.current?.scrollIntoView({ block: "end" }); }, [messages.length, streaming]);
  const onScroll = () => { const el = scroller.current; if (el) setAtBottom(el.scrollHeight - el.scrollTop - el.clientHeight < 60); };
  const toBottom = () => { end.current?.scrollIntoView({ block: "end", behavior: "smooth" }); setAtBottom(true); };

  // Every change is written to the episode's record in the library database right away.
  const save = (next: ChatMessage[]) => { putChat(ep.id, { messages: next, loaded: true }); void api.setBlob(ep.id, "chat", JSON.stringify(next)); };
  const saveArchive = (next: ArchivedChat[]) => { setArchive(next); void api.setBlob(ep.id, "chat-archive", JSON.stringify(next)); };

  const ask = async (history: ChatMessage[]) => {
    try { await check(); } catch (e) { setSetupError(errorText(e)); return; }
    setSetupError(null);
    const model = `${t(provider!.name)} · ${shortModel(provider!.model)}`;
    save(history);
    putChat(ep.id, { streaming: "" });
    setAtBottom(true);
    const ctl = new AbortController();
    chatAborts.set(ep.id, ctl);
    let partial = "";
    try {
      const answer = await askEpisode({ provider: provider!, title: ep.title, show: ep.showTitle ?? "", transcript: context(), history, instruction: chatPromptOn ? chatPrompt : "", onDelta: (d) => { partial = d; putChat(ep.id, { streaming: d }); }, signal: ctl.signal });
      // Some agents only stream and return an empty final answer: keep what was shown.
      const content = answer.trim() ? answer : partial;
      save([...history, { role: "assistant", content: content.trim() ? content : t("出错了：{error}", { error: t("模型没有返回内容，请重试或换一个模型") }), model }]);
    } catch (e) {
      const note = ctl.signal.aborted ? t("（已停止）") : t("出错了：{error}", { error: errorText(e) });
      save([...history, { role: "assistant", content: partial ? `${partial}\n\n${note}` : note, model }]);
    } finally {
      putChat(ep.id, { streaming: null });
      chatAborts.delete(ep.id);
    }
  };
  const [editAt, setEditAt] = useState<number | null>(null);
  const send = (text: string) => {
    const q = text.trim();
    if (!q || streaming != null) return;
    const base = editAt != null ? messages.slice(0, editAt) : messages;
    setEditAt(null);
    setInput("");
    void ask([...base, { role: "user", content: q }]);
  };
  /** Index of the question an answer belongs to (the user message right before it). */
  const pairStart = (i: number) => (messages[i]?.role === "assistant" && messages[i - 1]?.role === "user" ? i - 1 : i);
  const removePair = (i: number) => {
    const prev = messages;
    const a = pairStart(i);
    const b = messages[a + 1]?.role === "assistant" ? a + 2 : a + 1;
    if (editAt != null && a <= editAt) setEditAt(null);
    save([...messages.slice(0, a), ...messages.slice(b)]);
    useStore.getState().notify(t("已删除"), { label: t("撤销"), run: () => save(prev) });
  };
  const retry = () => { const i = messages.length - 1; if (messages[i]?.role === "assistant" && streaming == null) void ask(messages.slice(0, i)); };
  const edit = (i: number) => { setEditAt(i); setInput(messages[i].content); window.setTimeout(() => box.current?.focus(), 0); };
  const newChat = () => {
    if (!messages.length) return;
    saveArchive([{ id: `${Date.now()}`, at: Date.now(), messages }, ...archive].slice(0, 30));
    save([]); setInput(""); box.current?.focus();
  };
  const restore = (id: string) => {
    const c = archive.find((x) => x.id === id);
    if (!c) return;
    const rest = archive.filter((x) => x.id !== id);
    saveArchive(messages.length ? [{ id: `${Date.now()}`, at: Date.now(), messages }, ...rest] : rest);
    save(c.messages);
  };
  const blocked = !transcript || connected === false;
  const empty = !messages.length && streaming == null;

  return (
    <div className="chat">
      <div className="chat-stage">
      <div className="chat-scroll" ref={scroller} onScroll={onScroll}>
        {empty && connected === false && <ConnectCard what={tk("问答")} />}
        {empty && connected !== false && (
          <div className="chat-intro">
            <div className="intro-head">
              <AskArt className="intro-art" />
              <div>
                <p className="lead">{t("问问这期节目")}</p>
                <p className="muted small">{t("回答依据整期文字稿，引用处带时间戳，点一下就跳过去听。")}</p>
              </div>
            </div>
            {!transcript && <p className="setup-note wait-note"><span className="wait-dots" aria-hidden><i /><i /><i /></span><span>{t("字幕准备好后就可以提问。")}</span></p>}
            <div className="prompt-list">
              {shownPrompts.map((p) => {
                const Icon = promptIcon(p.id);
                return (
                  <button key={p.id} type="button" className="prompt-row" title={p.body} disabled={blocked} onClick={() => { countUse(p.id); send(p.body); }}>
                    <Icon size={15} className="pr-icon" />
                    <span className="pr-text"><span className="pr-title">{p.title}</span><span className="pr-sub">{p.body}</span></span>
                  </button>
                );
              })}
            </div>
            <div className="prompt-foot">
              {prompts.length > 4 && <button type="button" className="link-btn" aria-expanded={allPrompts} onClick={() => setAllPrompts(!allPrompts)}>{allPrompts ? t("收起") : t("更多 {n} 个", { n: prompts.length - 4 })}<ChevronDown size={12} className={allPrompts ? "flip" : ""} /></button>}
              <span className="spacer" />
            <button type="button" className="link-btn" onClick={() => setPrefsOpen(true)}><Settings2 size={13} /> {t("自定义提示词和回答偏好")}</button>
            </div>
            <NightNote />
          </div>
        )}
        {messages.map((m, i) => m.role === "user" ? (
          <div key={i} className="msg-wrap user">
            <div className="msg user"><p>{m.content}</p></div>
            {streaming == null && <QuestionActions text={m.content} onEdit={() => edit(i)} onDelete={() => removePair(i)} />}
          </div>
        ) : (
          <div key={i} className={`msg-wrap assistant ${i === messages.length - 1 ? "last" : ""}`}>
            <div className="msg assistant"><Markdown text={m.content} onSeek={seek} onClip={clipRange} /></div>
            {streaming == null && <AnswerActions text={m.content} ep={ep} model={m.model} onRetry={i === messages.length - 1 && !blocked ? retry : undefined} onDelete={() => removePair(i)} />}
          </div>
        ))}
        {streaming != null && (
          <div className="msg assistant">
            {streaming ? <Markdown text={streaming} onSeek={seek} onClip={clipRange} /> : <p className="muted"><Loader2 size={14} className="spin" /> {t("正在阅读文字稿…")}</p>}
          </div>
        )}
        {setupError && (
          <div className="setup-note"><span>{setupError}</span><button className="btn primary sm" onClick={() => useStore.getState().openSettings("model")}>{t("去设置")}</button></div>
        )}
        <div ref={end} />
      </div>
      {!atBottom && (messages.length > 0 || streaming != null) && <button type="button" className="to-bottom" title={t("回到底部")} aria-label={t("回到底部")} onClick={toBottom}><ArrowDown size={16} /></button>}
      </div>
      {editAt != null && (
        <div className="edit-banner small">
          <span>{t("正在编辑历史提问，发送后将从此处重新生成")}</span>
          <button type="button" className="link-btn small" onClick={() => { setEditAt(null); setInput(""); }}>{t("取消")}</button>
        </div>
      )}
      <form className="composer" onSubmit={(e) => { e.preventDefault(); void send(input); }} onClick={(e) => { if (e.target === e.currentTarget) box.current?.focus(); }}>
        <textarea
          ref={box}
          value={input}
          rows={1}
          placeholder={connected === false ? t("连接模型后可以提问") : transcript ? t("问点什么，Enter 发送，Shift+Enter 换行") : t("字幕准备好后可以提问")}
          disabled={blocked}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={(e) => { if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) { e.preventDefault(); void send(input); } }}
        />
        <div className="composer-bar">
          <ModelPicker disabled={connected === false} />
          <PromptMenu onPick={(b) => void send(b)} onManage={() => setPrefsOpen(true)} disabled={blocked || streaming != null} />
          <span className="spacer" />
          <HistoryMenu archive={archive} ep={ep} current={messages} onRestore={restore} onDelete={(id) => saveArchive(archive.filter((x) => x.id !== id))} />
          {messages.length > 0 && streaming == null && <button type="button" className="btn ghost sm new-chat" title={t("开始新对话，当前对话存进历史")} onClick={newChat}><SquarePen size={14} /> {t("新对话")}</button>}
          {streaming != null
            ? <button type="button" className="send" title={t("停止")} onClick={() => chatAborts.get(ep.id)?.abort()}><Square size={13} fill="currentColor" /></button>
            : <button type="submit" className="send" title={t("发送")} disabled={!input.trim() || blocked}><ArrowUp size={16} /></button>}
        </div>
      </form>
      <p className="model-note">{t("回答由 AI 生成，可能有误，重要信息请回到原文核对")}</p>
      {prefsOpen && <ChatPrefsDialog onClose={() => setPrefsOpen(false)} />}
    </div>
  );
}
/** 导读 runs per episode outside the panel, so switching tabs or episodes mid-run neither loses the result
 *  nor offers to start (and pay for) the same run again. */
interface GuideJob { summary?: string | null; streaming: string | null; error: string | null; needsSetup: boolean; shared?: boolean; checking?: boolean; model?: string }
const useGuides = create<Record<string, GuideJob>>(() => ({}));
const guideAborts = new Map<string, AbortController>();
const putGuide = (id: string, p: Partial<GuideJob>) => useGuides.setState((s) => ({ [id]: { ...(s[id] ?? { streaming: null, error: null, needsSetup: false }), ...p } }));

export function SummaryPanel({ ep }: { ep: Episode }) {
  const { transcript, provider, context, check } = useReady(ep);
  const translation = useJobs((s) => s.translations[ep.id]);
  const connected = useConnected();
  const seek = useSeek(ep);
  const clipTools = useClipTools();
  const clipRange = useMemo(() => {
    if (!clipTools || !ep.videoId || !transcript?.segments.length) return undefined;
    return (start: number, end?: number) => {
      const { startIdx, endIdx } = rangeFromClock(transcript.segments, start, end);
      const clip = createManualClip(transcript.segments, translation?.items, startIdx, endIdx, ep);
      openVideoClipDialog({ ep, segments: transcript.segments, zh: translation?.items, clip });
    };
  }, [ep, transcript, translation?.items, clipTools]);
  const job = useGuides((s) => s[ep.id]);
  const summary = job?.summary ?? null;
  const streaming = job?.streaming ?? null;
  const error = job?.error ?? null;
  const needsSetup = !!job?.needsSetup;
  const checking = !!job?.checking;

  useEffect(() => {
    if (useGuides.getState()[ep.id]?.streaming != null) return;
    void (async () => {
      const [v, from, model] = await Promise.all([api.getBlob(ep.id, "summary"), api.getBlob(ep.id, "summary-from"), api.getBlob(ep.id, "summary-model")]);
      if (useGuides.getState()[ep.id]?.streaming != null) return;
      if (v) { putGuide(ep.id, { summary: v, shared: from === "shared", model: model || undefined }); return; }
      // Someone may have shared one: checked before offering to generate (and pay for) it.
      putGuide(ep.id, { summary: null, checking: true });
      const hit = await api.sharedGuide(ep.id).catch(() => null);
      if (useGuides.getState()[ep.id]?.streaming != null) return;
      putGuide(ep.id, { checking: false, ...(hit ? { summary: hit, shared: true } : {}) });
    })();
  }, [ep.id, transcript]);

  const run = async () => {
    if (useGuides.getState()[ep.id]?.streaming != null) return;
    putGuide(ep.id, { error: null, needsSetup: false });
    const ctl = new AbortController();
    guideAborts.set(ep.id, ctl);
    try {
      await check();
      const modelLabel = [provider ? t(provider.name) : "", provider?.model ? shortModel(provider.model) : ""].filter(Boolean).join(" · ");
      putGuide(ep.id, { streaming: "" });
      const text = await askEpisode({ provider: provider!, title: ep.title, show: ep.showTitle ?? "", transcript: context(), history: [{ role: "user", content: summaryPrompt(transcript?.segments[transcript.segments.length - 1]?.end ?? ep.duration) }], onDelta: (d) => putGuide(ep.id, { streaming: d }), signal: ctl.signal });
      // Some agents only stream: keep what arrived if the final answer comes back empty.
      const out = text.trim() ? text : (useGuides.getState()[ep.id]?.streaming ?? "");
      if (!out.trim()) throw new Error(t("模型没有返回内容，请重试或换一个模型"));
      putGuide(ep.id, { summary: out, shared: false, model: modelLabel });
      await api.setBlob(ep.id, "summary", out);
      if (modelLabel) void api.setBlob(ep.id, "summary-model", modelLabel);
      void api.setBlob(ep.id, "summary-from", "local");
      void api.shareGuide(ep.id, out, `${provider!.name} · ${provider!.model}`).catch(() => {});
    } catch (e) {
      if (!ctl.signal.aborted) {
        putGuide(ep.id, { error: errorText(e), needsSetup: Boolean((e as { setup?: boolean })?.setup) || /设置|API Key/.test(rawErrorText(e)) });
      }
    }
    finally {
      guideAborts.delete(ep.id);
      putGuide(ep.id, { streaming: null });
    }
  };
  const stop = () => { guideAborts.get(ep.id)?.abort(); };

  const text = streaming ?? summary;
  if (!text && connected === false) return <div className="panel-empty"><ConnectCard what={tk("导读")} /></div>;
  if (!text && streaming != null) {
    return (
      <div className="panel-empty guide-wait" role="status">
        <PageArt writing />
        <p className="es-title">{t("正在通读整期文字稿")}</p>
        <p className="wait-steps" aria-hidden><span>{t("先划出重点")}</span><span>{t("再理一条时间线")}</span><span>{t("挑几句值得记住的原话")}</span></p>
        <button className="btn ghost sm" onClick={stop}><Square size={12} fill="currentColor" /> {t("停止")}</button>
        <span className="sr-only">{t("正在生成…")}</span>
      </div>
    );
  }
  if (!text && checking) return <div className="panel-empty" role="status"><PageArt /><p className="muted"><Loader2 size={14} className="spin" /> {t("正在查找别人分享的导读…")}</p></div>;
  if (!text) {
    return (
      <div className="panel-empty">
        <PageArt />
        <p className="es-title">{t("一键中文导读")}</p>
        <p className="muted">{t("一句话总结、时间线、核心观点、原话和推荐片段，都带时间戳。")}</p>
        {error && <p className="error-text">{error}</p>}
        {error && needsSetup && <button className="btn" onClick={() => useStore.getState().openSettings("model")}>{t("去设置")}</button>}
        <button className="btn primary" onClick={run} disabled={!transcript || streaming != null}>{streaming != null ? t("正在生成…") : t("生成导读")}</button>
        {!transcript && <p className="muted small">{t("字幕准备好后可以生成。")}</p>}
      </div>
    );
  }
  const fallbackModel = [provider ? t(provider.name) : "", provider?.model ? shortModel(provider.model) : ""].filter(Boolean).join(" · ");
  const byLabel = job?.shared ? t("来自共享导读，没有花你的额度") : (job?.model || fallbackModel);
  const readGuideText = () => text.replace(/\[\d{1,2}:\d{2}(?::\d{2})?(?:\s*[-–~至]\s*\d{1,2}:\d{2}(?::\d{2})?)?\]/g, "").replace(/[#*`>]/g, "").trim();
  return (
    <div className="summary">
      <Markdown text={text} onSeek={seek} onClip={clipRange} />
      {streaming != null ? (
        <div className="summary-foot">
          <span className="muted small"><Loader2 size={12} className="spin" /> {t("正在生成…")}</span>
          <button className="btn ghost sm" onClick={stop}><Square size={12} fill="currentColor" /> {t("停止")}</button>
        </div>
      ) : (
        <div className="summary-foot">
          <span className="muted small clamp1">{byLabel}</span>
          <ReadAloud text={readGuideText} title={t("朗读导读")} />
          <CopyMenu text={text} ep={ep} />
          <button className="btn ghost sm" onClick={run}><RefreshCw size={13} /> {t("重新生成")}</button>
        </div>
      )}
      {error && (
        <div className="row-actions">
          <p className="error-text">{error}</p>
          {needsSetup && <button className="btn sm" onClick={() => useStore.getState().openSettings("model")}>{t("去设置")}</button>}
        </div>
      )}
    </div>
  );
}
