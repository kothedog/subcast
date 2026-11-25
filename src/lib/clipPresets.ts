import { tk } from "./i18n";
import type { PostPlatform } from "./highlights";
import type { CardCrop, ClipAspect, ClipFit } from "./videoClipCanvas";

export type ClipPresetId = "vertical" | "xhs" | "landscape" | "moments";
export type CoverMode = "flash" | "hold" | "none";

/** The settings a preset decides; everything else (subtitles, cover style, branding) stays the user's. */
export interface PresetValues {
  aspect: ClipAspect;
  fit: ClipFit;
  cardCrop: CardCrop;
  coverMode: CoverMode;
  removePauses: boolean;
}

export interface ClipPreset {
  id: ClipPresetId;
  name: string;
  hint: string;
  values: PresetValues;
  /** The post the copy tab opens on. */
  post: PostPlatform;
}

/** Where the clip is going, in one click. Names are i18n keys (render with `t()`). */
export const CLIP_PRESETS: ClipPreset[] = [
  {
    id: "vertical",
    name: tk("抖音 · 视频号"),
    hint: tk("9:16 竖屏，完整画面，去掉停顿，首帧藏封面"),
    values: { aspect: "9:16", fit: "card", cardCrop: "4:3", coverMode: "flash", removePauses: true },
    post: "short",
  },
  {
    id: "xhs",
    name: tk("小红书"),
    hint: tk("3:4 竖屏，完整画面，去掉停顿，首帧藏封面"),
    values: { aspect: "3:4", fit: "card", cardCrop: "4:3", coverMode: "flash", removePauses: true },
    post: "short",
  },
  {
    id: "landscape",
    name: tk("B站 · 横屏"),
    hint: tk("16:9 横屏铺满，保留停顿，封面另存"),
    values: { aspect: "16:9", fit: "fill", cardCrop: "none", coverMode: "none", removePauses: false },
    post: "article",
  },
  {
    id: "moments",
    name: tk("朋友圈 · 公众号"),
    hint: tk("1:1 方形，片头定格 1 秒封面"),
    values: { aspect: "1:1", fit: "card", cardCrop: "4:3", coverMode: "hold", removePauses: false },
    post: "social",
  },
];

/** The preset the current settings match, if any. `fit` and `cardCrop` only count where they show. */
export function matchPreset(v: PresetValues, fitShown: boolean, card: boolean): ClipPresetId | null {
  for (const p of CLIP_PRESETS) {
    const w = p.values;
    if (w.aspect !== v.aspect || w.coverMode !== v.coverMode || w.removePauses !== v.removePauses) continue;
    if (fitShown && w.fit !== v.fit) continue;
    if (card && w.cardCrop !== v.cardCrop) continue;
    return p.id;
  }
  return null;
}
