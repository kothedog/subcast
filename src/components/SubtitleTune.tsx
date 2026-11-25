// "Aa" on the video: subtitle size, what shows and how it looks, changed while watching. The same settings as
// Settings › 阅读与字幕 (src/lib/reading.ts turns them into CSS variables); position is set by dragging the subtitles.
import { useEffect, useRef, useState } from "react";
import { useShallow } from "zustand/react/shallow";
import { ALargeSmall } from "lucide-react";
import { useStore } from "../store";
import type { Episode } from "../lib/api";
import { t, useLang } from "../lib/i18n";
import { subtitleModeFor, subtitleModePatch, type SubtitleMode } from "../lib/settings";
import { ACTIONS, prettyCombo } from "../lib/shortcuts";
import { Seg, SubSizeSliders } from "./ReadingSettings";
import { switchToOriginalAudio, toggleTheater, wantsOriginalAudio } from "./Players";

export function SubtitleTune({ ep }: { ep?: Episode }) {
  const [open, setOpen] = useState(false);
  const box = useRef<HTMLDivElement>(null);
  useLang((s) => s.lang);
  const s = useStore(useShallow((x) => {
    const target = ep ?? x.current;
    return {
      subtitleScale: x.settings.subtitleScale,
      subtitleOrigScale: x.settings.subtitleOrigScale,
      subtitleLift: x.settings.subtitleLift,
      subtitleMode: subtitleModeFor(x.settings, target),
      subtitleLook: x.settings.subtitleLook,
      shortcuts: x.settings.shortcuts,
    };
  }));
  const save = useStore((x) => x.saveSettings);
  const saveMode = (v: SubtitleMode) => {
    const st = useStore.getState();
    void save(subtitleModePatch(st.settings, ep ?? st.current, v));
  };
  useEffect(() => {
    if (!open) return;
    const away = (e: PointerEvent) => { if (!box.current?.contains(e.target as Node)) setOpen(false); };
    const esc = (e: KeyboardEvent) => { if (e.key === "Escape") { e.stopPropagation(); setOpen(false); } };
    document.addEventListener("pointerdown", away);
    window.addEventListener("keydown", esc, true);
    return () => { document.removeEventListener("pointerdown", away); window.removeEventListener("keydown", esc, true); };
  }, [open]);
  const changed = !!s.subtitleLift || s.subtitleScale != null || s.subtitleOrigScale != null;
  const keyOf = (id: "subBigger" | "subSmaller") => (s.shortcuts?.[id] ?? ACTIONS.find((a) => a.id === id)!.keys)[0];
  const keys = [keyOf("subBigger"), keyOf("subSmaller")].filter(Boolean).map(prettyCombo).join(" / ");
  return (
    <div className={`sub-tune ${open ? "open" : ""}`} ref={box} onClick={(e) => e.stopPropagation()} onDoubleClick={(e) => e.stopPropagation()}>
      <button className="sub-tune-btn" title={t("调整字幕")} aria-label={t("调整字幕")} aria-expanded={open} onClick={() => setOpen(!open)}><ALargeSmall size={17} /></button>
      {open && (
        <div className="sub-tune-pop" role="dialog" aria-label={t("调整字幕")}>
          <SubSizeSliders />
          <div className="sub-tune-row">
            <span>{t("显示")}</span>
            <Seg value={s.subtitleMode} options={[["bilingual", t("双语")], ["translation", t("中文")], ["original", t("原文")], ["off", t("关闭")]]} onChange={saveMode} />
          </div>
          <div className="sub-tune-row">
            <span>{t("样式")}</span>
            <Seg value={s.subtitleLook ?? "outline"} options={[["box", t("半透明底")], ["outline", t("描边，无底")]]} onChange={(v) => void save({ subtitleLook: v })} />
          </div>
          {ep?.videoId && (
            <div className="sub-tune-row">
              <span>{t("音轨")}</span>
              {wantsOriginalAudio(ep.videoId)
                ? <span className="small muted">{t("原声")}</span>
                : <button className="link-btn" title={t("YouTube 可能按地区自动播放 AI 配音，点这里改回原声")} onClick={() => { setOpen(false); switchToOriginalAudio(ep.videoId); }}>{t("听到的是配音？换成原声")}</button>}
            </div>
          )}
          <p className="sub-tune-hint">{t("拖动字幕可以上下移动，双击字幕回到原位。")}{keys && ` ${t("快捷键 {keys} 调大小。", { keys })}`}</p>
          <div className="sub-tune-foot">
            <button className="link-btn" disabled={!changed} onClick={() => void save({ subtitleScale: undefined, subtitleOrigScale: undefined, subtitleLift: undefined })}>{t("恢复默认")}</button>
            <button className="link-btn" onClick={() => { setOpen(false); toggleTheater(false); useStore.getState().openSettings("reading"); }}>{t("更多字幕设置…")}</button>
          </div>
        </div>
      )}
    </div>
  );
}
