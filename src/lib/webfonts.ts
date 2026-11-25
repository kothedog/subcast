// Open-licence Chinese typefaces for covers, loaded when picked (not shipped: each is tens of MB). The
// stylesheets split each face by Unicode range, so only the characters a cover uses are downloaded.
// Offline or blocked, the cover falls back to the system face.

export type CoverFontId = "" | "sans" | "serif" | "noto-sans" | "noto-serif" | "wenkai";

interface WebFont { family: string; css: string[] }

const WEB: Record<"noto-sans" | "noto-serif" | "wenkai", WebFont> = {
  "noto-sans": {
    family: "Noto Sans SC",
    css: ["https://cdn.jsdelivr.net/npm/@fontsource/noto-sans-sc@5.3.1/500.css", "https://cdn.jsdelivr.net/npm/@fontsource/noto-sans-sc@5.3.1/700.css"],
  },
  "noto-serif": {
    family: "Noto Serif SC",
    css: ["https://cdn.jsdelivr.net/npm/@fontsource/noto-serif-sc@5.3.1/500.css", "https://cdn.jsdelivr.net/npm/@fontsource/noto-serif-sc@5.3.1/700.css"],
  },
  wenkai: {
    family: "LXGW WenKai",
    css: ["https://cdn.jsdelivr.net/npm/lxgw-wenkai-webfont@1.7.0/style.css"],
  },
};

const linked = new Set<string>();

/** The CSS family list for a cover font (web faces first, the system face after them). */
export function familyOf(id: CoverFontId, fallback: string): string {
  if (id === "noto-sans" || id === "noto-serif" || id === "wenkai") return `"${WEB[id].family}", ${fallback}`;
  return fallback;
}

/** Loads the face's glyphs for `text` (at most a few seconds); false when it couldn't. */
export async function ensureFont(id: CoverFontId, text: string): Promise<boolean> {
  if (id !== "noto-sans" && id !== "noto-serif" && id !== "wenkai") return true;
  const f = WEB[id];
  for (const href of f.css) {
    if (linked.has(href)) continue;
    linked.add(href);
    const link = document.createElement("link");
    link.rel = "stylesheet";
    link.href = href;
    document.head.appendChild(link);
    await new Promise<void>((resolve) => { link.onload = () => resolve(); link.onerror = () => resolve(); window.setTimeout(resolve, 4000); });
  }
  const sample = text.replace(/\s+/g, "") || "字"; // i18n-ignore: glyph sample for font loading
  const weights = id === "wenkai" ? ["400", "700"] : ["500", "700"];
  try {
    await Promise.race([
      Promise.all(weights.map((w) => document.fonts.load(`${w} 64px "${f.family}"`, sample))),
      new Promise((_, reject) => window.setTimeout(() => reject(new Error("timeout")), 6000)),
    ]);
    return true;
  } catch {
    return false;
  }
}

export const isWebFont = (id: CoverFontId) => id === "noto-sans" || id === "noto-serif" || id === "wenkai";
