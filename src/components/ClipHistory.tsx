import { useEffect, useRef, useState } from "react";
import { FolderOpen, History, X } from "lucide-react";
import { api } from "../lib/api";
import { clock } from "../lib/format";
import { t } from "../lib/i18n";
import { forgetExport, useClipHistory, type ExportedClip } from "../lib/clipHistory";
import { useStore } from "../store";

function ago(at: number): string {
  const m = Math.round((Date.now() - at) / 60000);
  if (m < 1) return t("刚刚");
  if (m < 60) return t("{n} 分钟前", { n: m });
  const h = Math.round(m / 60);
  if (h < 24) return t("{n} 小时前", { n: h });
  return new Date(at).toLocaleDateString();
}

function Row({ c, here }: { c: ExportedClip; here: boolean }) {
  const reveal = () => void api.revealFile(c.path).catch(() => {
    // Moved or deleted since: drop it from the list instead of failing again next time.
    forgetExport(c.path);
    useStore.getState().notify(t("找不到这个文件了，已从记录中移除"));
  });
  return (
    <div className="clip-hist-row">
      <button type="button" className="clip-hist-main" onClick={reveal} title={c.path}>
        <span className="clamp1">{c.title || t("未命名片段")}</span>
        <span className="small muted clamp1">{[here ? null : c.episodeTitle, c.aspect, clock(c.duration), ago(c.at)].filter(Boolean).join(" · ")}</span>
      </button>
      <button type="button" className="icon-btn sm" onClick={reveal} title={t("在文件夹中显示")} aria-label={t("在文件夹中显示")}><FolderOpen size={13} /></button>
      <button type="button" className="icon-btn sm" onClick={() => forgetExport(c.path)} title={t("从记录中移除")} aria-label={t("从记录中移除")}><X size={13} /></button>
    </div>
  );
}

/** "最近导出" in the highlights toolbar: this episode's clips first, then the rest. Files stay where they are. */
export function ClipHistory({ episodeId }: { episodeId: string }) {
  const items = useClipHistory((s) => s.items);
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false); };
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }, [open]);
  if (!items.length) return null;
  const mine = items.filter((c) => c.episodeId === episodeId);
  const others = items.filter((c) => c.episodeId !== episodeId).slice(0, 12);
  return (
    <div className="menu-wrap" ref={box}>
      <button type="button" className="link-btn" aria-expanded={open} onClick={() => setOpen(!open)}>
        <History size={12} /> {t("最近导出")}{mine.length ? ` ${mine.length}` : ""}
      </button>
      {open && (
        <div className="menu right clip-hist" role="dialog" aria-label={t("最近导出")}>
          {mine.length > 0 && <p className="menu-note">{t("这一集")}</p>}
          {mine.map((c) => <Row key={c.path} c={c} here />)}
          {others.length > 0 && <p className="menu-note">{t("其他节目")}</p>}
          {others.map((c) => <Row key={c.path} c={c} here={false} />)}
        </div>
      )}
    </div>
  );
}
