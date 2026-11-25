// Settings › 阅读与字幕 take effect as CSS variables on the root, so every subtitle (video, audio, full screen)
// and the Q&A follow one set of values. Web fonts load only when chosen.
import { useEffect } from "react";
import { useShallow } from "zustand/react/shallow";
import { useStore } from "../store";
import { POSTER_FONTS, loadCss } from "./poster";
import { t } from "./i18n";
import type { Settings } from "./settings";

export const SUB_SCALE = { s: 0.86, m: 1, l: 1.18, xl: 1.38 } as const;
/** Subtitle fonts: the system's, or the poster's open-source ones (loaded from a CDN when picked). */
export const SUB_FONTS = { system: "", serif: "serif", kai: "kai", sans: "sans" } as const;

export const SCALE_MIN = 0.7, SCALE_MAX = 2;

/** The subtitle size multiplier in use: the one picked on the video, else the size picked in settings. */
export function subScale(s: Pick<Settings, "subtitleScale" | "subtitleSize">): number {
  return s.subtitleScale ?? SUB_SCALE[s.subtitleSize ?? "m"];
}

/** The original-language line's multiplier: its own when set, else the same as the Chinese line. */
export function origScale(s: Pick<Settings, "subtitleScale" | "subtitleSize" | "subtitleOrigScale">): number {
  return s.subtitleOrigScale ?? subScale(s);
}

export const CHAT_MIN = 0.85, CHAT_MAX = 1.5;

/** AI answers' text size multiplier: the slider's, else the old "标准 / 大". */
export function chatScaleOf(s: Pick<Settings, "chatScale" | "chatSize">): number {
  return s.chatScale ?? (s.chatSize === "l" ? 1.12 : 1);
}

const clampScale = (v: number) => Math.round(Math.min(SCALE_MAX, Math.max(SCALE_MIN, v)) * 20) / 20;

/** Chinese line size. Until the original line has its own size, it moves along. */
export function setSubScale(v: number) {
  const scale = clampScale(v);
  void useStore.getState().saveSettings({ subtitleScale: scale }, true);
  return scale;
}

/** Original-language line size, set on its own. */
export function setOrigScale(v: number) {
  const scale = clampScale(v);
  void useStore.getState().saveSettings({ subtitleOrigScale: scale }, true);
  return scale;
}

/** ⌘= / ⌘-: both lines one step bigger or smaller, said in a toast. */
export function stepSubScale(dir: 1 | -1) {
  const s = useStore.getState().settings;
  const zh = clampScale(subScale(s) + dir * 0.1);
  const patch: Partial<Settings> = { subtitleScale: zh };
  if (s.subtitleOrigScale != null) patch.subtitleOrigScale = clampScale(s.subtitleOrigScale + dir * 0.1);
  void useStore.getState().saveSettings(patch, true);
  useStore.getState().notify(t("字幕大小 {n}%", { n: Math.round(zh * 100) }));
}

/** Where dragging left the video subtitles, kept within the picture. */
export function setSubLift(v: number) {
  void useStore.getState().saveSettings({ subtitleLift: Math.round(Math.min(70, Math.max(-4, v)) * 10) / 10 }, true);
}

export function useReadingSettings() {
  // Only these fields: subscribing to all settings re-rendered the whole app on any change.
  const s = useStore(useShallow((x) => ({ subtitleSize: x.settings.subtitleSize, subtitleScale: x.settings.subtitleScale, subtitleOrigScale: x.settings.subtitleOrigScale, subtitleLift: x.settings.subtitleLift, subtitleFont: x.settings.subtitleFont, subtitleLook: x.settings.subtitleLook, chatSize: x.settings.chatSize, chatScale: x.settings.chatScale, audioNextLine: x.settings.audioNextLine })));
  useEffect(() => {
    const root = document.documentElement.style;
    root.setProperty("--sub-scale", String(subScale(s)));
    root.setProperty("--sub-orig-scale", String(origScale(s)));
    root.setProperty("--sub-lift", `${s.subtitleLift ?? 0}cqh`);
    root.setProperty("--chat-scale", String(chatScaleOf(s)));
    const f = POSTER_FONTS.find((x) => x.id === SUB_FONTS[s.subtitleFont ?? "sans"]);
    if (f?.css) void loadCss(f.css).catch(() => {});
    root.setProperty("--sub-font", f ? `${f.family}, var(--font, system-ui)` : "inherit");
    document.documentElement.dataset.subLook = s.subtitleLook ?? "outline";
    document.documentElement.dataset.nextLine = s.audioNextLine ? "on" : "off";
  }, [s.subtitleSize, s.subtitleScale, s.subtitleOrigScale, s.subtitleLift, s.subtitleFont, s.subtitleLook, s.chatSize, s.chatScale, s.audioNextLine]);
}
