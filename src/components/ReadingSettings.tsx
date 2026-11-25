// Settings › 阅读与字幕: how subtitles and answers look, with a live preview using a line from what is playing.
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../store";
import { useJobs } from "../lib/jobs";
import { t, useLang } from "../lib/i18n";
import { CHAT_MAX, CHAT_MIN, SCALE_MAX, SCALE_MIN, SUB_FONTS, chatScaleOf, origScale, setOrigScale, setSubScale, subScale } from "../lib/reading";
import { POSTER_FONTS } from "../lib/poster";
import type { Settings } from "../lib/settings";
import { useLayout } from "../lib/layout";

export function Seg<T extends string | number>({ value, options, onChange }: { value: T; options: [T, string][]; onChange: (v: T) => void }) {
  return (
    <div className="seg-control" role="radiogroup">
      {options.map(([k, label]) => <button key={String(k)} role="radio" aria-checked={value === k} className={value === k ? "on" : ""} onClick={() => onChange(k)}>{label}</button>)}
    </div>
  );
}

/** Two sliders, one per subtitle line; shared by Settings and the video's "Aa". */
export function SubSizeSliders() {
  useLang((s) => s.lang);
  const s = useStore(useShallow((x) => ({ subtitleScale: x.settings.subtitleScale, subtitleSize: x.settings.subtitleSize, subtitleOrigScale: x.settings.subtitleOrigScale })));
  const rows: [string, number, (v: number) => void][] = [
    [t("中文"), subScale(s), setSubScale],
    [t("原文"), origScale(s), setOrigScale],
  ];
  return (
    <>
      {rows.map(([label, v, set]) => (
        <div className="size-row" key={label}>
          <span>{label}</span>
          <input type="range" min={SCALE_MIN * 100} max={SCALE_MAX * 100} step={5} value={Math.round(v * 100)} aria-label={t("{line}字幕大小", { line: label })} onChange={(e) => set(Number(e.target.value) / 100)} />
          <span className="num size-val">{Math.round(v * 100)}%</span>
        </div>
      ))}
    </>
  );
}

export function ReadingSettings() {
  const { settings, saveSettings, current } = useStore(useShallow((s) => ({ settings: s.settings, saveSettings: s.saveSettings, current: s.current })));
  useLang((s) => s.lang);
  const below = useLayout((l) => l.transcriptBelow);
  const save = (p: Partial<Settings>) => void saveSettings(p);
  // A real line when something is playing, else a sample.
  const tr = useJobs((s) => (current ? s.transcripts[current.id] : undefined));
  const items = useJobs((s) => (current ? s.translations[current.id]?.items : undefined));
  const k = tr?.segments.findIndex((_, i) => !!items?.[i]) ?? -1;
  const orig = k >= 0 ? tr!.segments[k].text : "The most important thing is to keep learning faster than the world changes.";
  const zh = k >= 0 ? items![k]! : t("最重要的是，学习的速度要比世界变化的速度更快。");
  const fontName = (id: keyof typeof SUB_FONTS) => id === "system" ? t("系统字体") : t(POSTER_FONTS.find((f) => f.id === SUB_FONTS[id])?.name ?? "");

  return (
    <div className="reading">
      <div className={`reading-preview look-${settings.subtitleLook ?? "outline"}`} aria-hidden>
        <div className="subtitle-overlay">
          {settings.subtitleMode === "off" ? (
            <p className="muted small">{t("画面不显示字幕")}</p>
          ) : (
            <div className="sub-box">
              {settings.subtitleMode !== "original" && <p className="sub-zh">{zh}</p>}
              {settings.subtitleMode !== "translation" && <p className="sub-orig">{orig}</p>}
            </div>
          )}
        </div>
      </div>

      <section className="setup-block">
        <p className="setup-title">{t("字幕")}</p>
        {/* Same choice as the video's "Aa" and the C key: the preview above follows it. */}
        <div className="reading-row"><span>{t("显示")}</span>
          <Seg value={settings.subtitleMode} options={[["bilingual", t("双语")], ["translation", t("中文")], ["original", t("原文")], ["off", t("关闭")]]} onChange={(v) => save({ subtitleMode: v })} />
        </div>
        <div className="reading-row"><span>{t("字号")}</span>
          <div className="reading-sizes"><SubSizeSliders /></div>
        </div>
        <div className="reading-row"><span>{t("字体")}</span>
          <Seg value={settings.subtitleFont ?? "sans"} options={(Object.keys(SUB_FONTS) as (keyof typeof SUB_FONTS)[]).map((id) => [id, fontName(id)])} onChange={(v) => save({ subtitleFont: v })} />
        </div>
        <p className="muted small reading-note">{t("字体同时用于中文和原文两行。")}</p>
        <div className="reading-row"><span>{t("视频上的字幕")}</span>
          <Seg value={settings.subtitleLook ?? "outline"} options={[["box", t("半透明底")], ["outline", t("描边，无底")]]} onChange={(v) => save({ subtitleLook: v })} />
        </div>
        <div className="reading-row"><span>{t("字幕列表")}</span>
          <Seg value={below ? "below" : "side"} options={[["side", t("右侧栏")], ["below", t("播放器下方")]]} onChange={(v) => useLayout.getState().put({ transcriptBelow: v === "below" })} />
        </div>
        <label className="check"><input type="checkbox" checked={settings.audioNextLine === true} onChange={(e) => save({ audioNextLine: e.target.checked })} /> {t("音频节目提前显示下一句")}</label>
      </section>

      <section className="setup-block">
        <p className="setup-title">{t("AI 问答与导读")}</p>
        {/* The same bubbles the chat uses, so the preview is what the answers will look like. */}
        <div className="chat-preview" aria-hidden>
          <div className="msg user"><p>{t("这期节目最重要的观点是什么？")}</p></div>
          <div className="msg assistant"><p>{t("嘉宾认为，学习的速度要比世界变化的速度更快：先抓住底层原理，再用小项目反复练习。")}</p></div>
        </div>
        <div className="reading-row"><span>{t("字号")}</span>
          <div className="reading-sizes">
            <div className="size-row">
              <input type="range" min={CHAT_MIN * 100} max={CHAT_MAX * 100} step={5} value={Math.round(chatScaleOf(settings) * 100)} aria-label={t("AI 问答与导读字号")}
                onChange={(e) => void saveSettings({ chatScale: Number(e.target.value) / 100 }, true)} />
              <span className="num size-val">{Math.round(chatScaleOf(settings) * 100)}%</span>
            </div>
          </div>
        </div>
      </section>

      <section className="setup-block">
        <p className="setup-title">{t("播放")}</p>
        <label className="check"><input type="checkbox" checked={!!settings.autoNext} onChange={(e) => save({ autoNext: e.target.checked })} /> {t("播完一集，自动接着播同一节目的下一集")}</label>
      </section>
    </div>
  );
}
