// Actions on Q&A messages (copy three ways, retry, edit, delete) and the conversation history menu.
import { useEffect, useRef, useState, type ReactNode } from "react";
import { Check, ChevronDown, Copy, History, Pencil, RefreshCw, Trash2, Download } from "lucide-react";
import type { ChatMessage, Episode } from "../lib/api";
import { date } from "../lib/format";
import { t } from "../lib/i18n";
import { showName } from "../lib/picks";
import { useStore } from "../store";

export interface ArchivedChat { id: string; at: number; messages: ChatMessage[] }

const STAMP = /\[(\d{1,2}:\d{2}(?::\d{2})?)\]/g;
const RANGE_OR_STAMP = /\s*\[\d{1,2}:\d{2}(?::\d{2})?\](?:\s*[-–~至]\s*\[\d{1,2}:\d{2}(?::\d{2})?\])?/g;
const secs = (c: string) => c.split(":").map(Number).reduce((a, b) => a * 60 + b, 0);

/** Markdown answer → text that pastes cleanly into chat apps and notes. */
function plain(md: string): string {
  return md
    .replace(/\*\*([^*]+)\*\*/g, "$1")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/^#{1,6}\s*/gm, "")
    .replace(/^\s*[-*]\s+/gm, "• ")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export type CopyKind = "stamps" | "plain" | "markdown";

export function copyText(md: string, kind: CopyKind, ep: Episode): string {
  if (kind === "plain") return plain(md.replace(RANGE_OR_STAMP, "")).replace(/[ \t]+([，。；、,.;])/g, "$1");
  if (kind === "stamps") return plain(md);
  // Markdown with a source line; on a video episode each timestamp links to that moment on YouTube.
  const body = ep.videoId ? md.replace(STAMP, (_, c: string) => `[${c}](https://youtu.be/${ep.videoId}?t=${secs(c)})`) : md;
  const src = ep.videoId ? `https://youtu.be/${ep.videoId}` : ep.link;
  const show = ep.showTitle ? `${showName(ep.showTitle)} · ` : "";
  return `${body.trim()}\n\n${t("来源：")}${show}${ep.title}${src ? `\n${src}` : ""}`;
}

async function toClipboard(text: string) {
  try { await navigator.clipboard.writeText(text); useStore.getState().notify(t("已复制")); }
  catch { useStore.getState().notify(t("复制失败，请重试")); }
}

/** Closes a popover on an outside click or Esc. */
function usePop() {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const h = (e: PointerEvent) => { if (!ref.current?.contains(e.target as Node)) setOpen(false); };
    const k = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); setOpen(false); } };
    document.addEventListener("pointerdown", h);
    window.addEventListener("keydown", k, true);
    return () => { document.removeEventListener("pointerdown", h); window.removeEventListener("keydown", k, true); };
  }, [open]);
  return { open, setOpen, ref };
}

function Act({ icon, label, onClick }: { icon: ReactNode; label: string; onClick: () => void }) {
  return <button type="button" className="msg-act" title={label} aria-label={label} onClick={onClick}>{icon}</button>;
}

/** Copy button with a menu: with timestamps (default), without, or Markdown with the source. */
export function CopyMenu({ text, ep }: { text: string; ep: Episode }) {
  const { open, setOpen, ref } = usePop();
  const [done, setDone] = useState(false);
  const copy = (kind: CopyKind) => { setOpen(false); void toClipboard(copyText(text, kind, ep)); setDone(true); window.setTimeout(() => setDone(false), 1400); };
  return (
    <div className="msg-copy" ref={ref}>
      <button type="button" className="msg-act" title={t("复制（带时间戳）")} aria-label={t("复制（带时间戳）")} onClick={() => copy("stamps")}>{done ? <Check size={14} /> : <Copy size={14} />}</button>
      <button type="button" className="msg-act caret" title={t("更多复制方式")} aria-label={t("更多复制方式")} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen(!open)}><ChevronDown size={12} /></button>
      {open && (
        <div className="menu msg-menu" role="menu">
          <button role="menuitem" onClick={() => copy("stamps")}><Copy size={14} /><span>{t("复制，保留时间戳")}</span></button>
          <button role="menuitem" onClick={() => copy("plain")}><Copy size={14} /><span>{t("复制，去掉时间戳")}</span></button>
          <button role="menuitem" onClick={() => copy("markdown")}><Copy size={14} /><span>{ep.videoId ? t("复制为 Markdown，时间戳链到 YouTube") : t("复制为 Markdown，附出处")}</span></button>
        </div>
      )}
    </div>
  );
}

export function AnswerActions({ text, ep, model, onRetry, onDelete }: { text: string; ep: Episode; model?: string; onRetry?: () => void; onDelete: () => void }) {
  return (
    <div className="msg-actions">
      <CopyMenu text={text} ep={ep} />
      {onRetry && <Act icon={<RefreshCw size={14} />} label={t("重新回答")} onClick={onRetry} />}
      <Act icon={<Trash2 size={14} />} label={t("删除这一问一答")} onClick={onDelete} />
      {model && <span className="msg-model">{model}</span>}
    </div>
  );
}

export function QuestionActions({ text, onEdit, onDelete }: { text: string; onEdit: () => void; onDelete: () => void }) {
  return (
    <div className="msg-actions user">
      <Act icon={<Copy size={14} />} label={t("复制")} onClick={() => void toClipboard(text)} />
      <Act icon={<Pencil size={14} />} label={t("编辑后重新提问")} onClick={onEdit} />
      <Act icon={<Trash2 size={14} />} label={t("删除这一问一答")} onClick={onDelete} />
    </div>
  );
}

export function exportChat(messages: ChatMessage[], ep: Episode): string {
  const head = `# ${ep.title}\n`;
  const body = messages.map((m) => (m.role === "user" ? `**${t("问")}：**${m.content}` : copyText(m.content, "markdown", ep).split(`\n\n${t("来源：")}`)[0])).join("\n\n");
  return `${head}\n${body}\n\n${t("来源：")}${ep.showTitle ? `${showName(ep.showTitle)} · ` : ""}${ep.title}${ep.videoId ? `\nhttps://youtu.be/${ep.videoId}` : ep.link ? `\n${ep.link}` : ""}`;
}

/** Earlier conversations about this episode: restore, copy the current one, or delete. */
export function HistoryMenu({ archive, ep, current, onRestore, onDelete }: { archive: ArchivedChat[]; ep: Episode; current: ChatMessage[]; onRestore: (id: string) => void; onDelete: (id: string) => void }) {
  const { open, setOpen, ref } = usePop();
  if (!archive.length && !current.length) return null;
  const titleOf = (m: ChatMessage[]) => m.find((x) => x.role === "user")?.content.slice(0, 40) ?? t("空对话");
  return (
    <div className="mp" ref={ref}>
      <button type="button" className="icon-btn sm" title={t("对话历史")} aria-haspopup="menu" aria-expanded={open} onClick={() => setOpen(!open)}><History size={16} /></button>
      {open && (
        <div className="mp-pop history" role="menu">
          <p className="mp-head">{t("这一集的对话")}</p>
          <div className="mp-list">
            {!archive.length && <p className="mp-empty">{t("点「新对话」后，之前的对话会保存在这里")}</p>}
            {archive.map((c) => (
              <div key={c.id} className="hist-row">
                <button type="button" role="menuitem" className="pm-row" onClick={() => { setOpen(false); onRestore(c.id); }}>
                  <span className="hist-title clamp1">{titleOf(c.messages)}</span>
                  <span className="hist-meta">{t("{n} 问", { n: c.messages.filter((m) => m.role === "user").length })} · {date(c.at)}</span>
                </button>
                <button type="button" className="icon-btn sm" title={t("删除这段对话")} onClick={() => onDelete(c.id)}><Trash2 size={13} /></button>
              </div>
            ))}
          </div>
          {current.length > 0 && <button type="button" className="mp-foot" onClick={() => { setOpen(false); void toClipboard(exportChat(current, ep)); }}><Download size={14} /> {t("复制当前对话为 Markdown")}</button>}
        </div>
      )}
    </div>
  );
}
