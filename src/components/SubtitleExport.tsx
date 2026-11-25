// "导出字幕…" in the subtitle bar's menu: pick a format and what to include, see the first lines, save.
import { useEffect, useMemo, useState } from "react";
import { save as saveDialog } from "@tauri-apps/plugin-dialog";
import { Modal } from "./Chrome";
import { Seg } from "./ReadingSettings";
import { api, errorText, type Episode, type Segment } from "../lib/api";
import { t, useLang } from "../lib/i18n";
import { subtitleModeFor } from "../lib/settings";
import { useStore } from "../store";
import type { TimeMap } from "../lib/timemap";
import { buildBlocks, buildCues, fileName, FORMATS, onClock, render, type SubContent, type SubFormat } from "../lib/subexport";

/** The map onto the audio this episode plays, when it differs from the one the subtitles were made from. */
async function playMap(ep: Episode): Promise<TimeMap | null> {
  const a = useStore.getState().align;
  let m: TimeMap | null = null;
  if (a?.id === ep.id) m = a.map;
  else {
    try { const raw = await api.getBlob(ep.id, "tmap"); m = raw ? JSON.parse(raw) : null; } catch { m = null; }
  }
  const shifts = !!m && (m.pieces.length > 1 || m.pieces.some((p) => Math.abs(p.playStart - p.refStart) > 0.05));
  return shifts ? m : null;
}

const LAST = "subcast.export";
function remembered(): { format: SubFormat; content?: SubContent } {
  try { const v = JSON.parse(localStorage.getItem(LAST) ?? "null"); if (v && FORMATS.some((f) => f.id === v.format)) return v; } catch { /* private window, or never saved */ }
  return { format: "srt" };
}

export function SubtitleExport({ ep, segments, zh, onClose }: { ep: Episode; segments: Segment[]; zh: (string | null | undefined)[] | undefined; onClose: () => void }) {
  useLang((s) => s.lang);
  const mode = useStore((s) => subtitleModeFor(s.settings, ep));
  const defaultContent: SubContent = mode === "off" ? (zh?.some((x) => !!x?.trim()) ? "bilingual" : "original") : mode;
  const notify = useStore((s) => s.notify);
  const translated = zh ? segments.filter((_, i) => !!zh[i]?.trim()).length : 0;
  const last = useMemo(remembered, []);
  const [format, setFormat] = useState<SubFormat>(last.format);
  const [content, setContent] = useState<SubContent>(translated ? (last.content ?? defaultContent) : "original");
  const [map, setMap] = useState<TimeMap | null>(null);
  const [onPlay, setOnPlay] = useState(true);
  const [busy, setBusy] = useState(false);
  useEffect(() => { void playMap(ep).then(setMap); }, [ep.id]);

  const clock = map && onPlay ? map : null;
  const text = (limit?: number) => {
    const segs = limit ? segments.slice(0, limit) : segments;
    const cues = onClock(buildCues(segs, zh, content), clock);
    const blocks = onClock(buildBlocks(segs, zh, content), clock);
    return render(format, cues, blocks, { title: ep.title, show: ep.showTitle, url: ep.link });
  };
  const preview = useMemo(() => {
    const full = text(4);
    // A few cues are enough to see the format; the ASS header alone is long, so show its events.
    const lines = (format === "ass" ? full.slice(full.indexOf("[Events]")) : full).split("\n");
    return lines.slice(0, 14).join("\n") + (lines.length > 14 ? "\n…" : "");
  }, [format, content, clock, segments, zh]);
  const missing = content !== "original" ? segments.length - translated : 0;

  const save = async () => {
    const f = FORMATS.find((x) => x.id === format)!;
    const path = await saveDialog({ defaultPath: fileName(ep, f.ext), filters: [{ name: f.name, extensions: [f.ext] }] }).catch(() => null);
    if (!path) return;
    setBusy(true);
    try {
      await api.saveText(path, text());
      try { localStorage.setItem(LAST, JSON.stringify({ format, content })); } catch { /* not remembered, that's all */ }
      notify(t("字幕已导出"));
      onClose();
    } catch (e) {
      notify(t("导出失败：{error}", { error: errorText(e) }));
    } finally { setBusy(false); }
  };

  return (
    <Modal title={t("导出字幕")} onClose={onClose}>
      <div className="sub-export">
        <div className="reading-row"><span>{t("格式")}</span>
          <Seg value={format} options={FORMATS.map((f) => [f.id, f.name])} onChange={setFormat} />
        </div>
        <div className="reading-row"><span>{t("内容")}</span>
          <Seg value={content} options={translated ? [["bilingual", t("双语")], ["translation", t("中文")], ["original", t("原文")]] : [["original", t("原文")]]} onChange={setContent} />
        </div>
        {map && (
          <div className="reading-row"><span>{t("时间")}</span>
            <Seg value={onPlay ? "play" : "ref"} options={[["play", t("按你播放的音频")], ["ref", t("按字幕原本的时间")]]} onChange={(v) => setOnPlay(v === "play")} />
          </div>
        )}
        {map && <p className="muted small">{onPlay ? t("这一集的音频重新剪辑过，导出的时间对准你在 SubCast 里播放的这个版本，广告处没有字幕。") : t("时间对准当初转写用的那个音频版本。")}</p>}
        {missing > 0 && <p className="muted small">{t("还有 {n} 句没有翻译，这些句子只导出原文。", { n: missing })}</p>}
        {format === "txt" || format === "md" ? <p className="muted small">{t("文字稿按段落导出，每段带开始时间。")}</p> : null}
        <pre className="sub-export-preview" aria-label={t("预览")}>{preview}</pre>
        <div className="row-actions end">
          <button className="btn ghost sm" onClick={onClose}>{t("取消")}</button>
          <button className="btn primary sm" disabled={busy || !segments.length} onClick={() => void save()}>{t("导出…")}</button>
        </div>
      </div>
    </Modal>
  );
}
