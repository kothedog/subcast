// WYSIWYG canvas renderer for exported clips: the static layer (title, show, brand, watermark) and timed
// subtitle bands, placed by one geometry that the export (videoclip.rs `Frame`) uses as well.
import type { Segment } from "./api";
import { type Cue, type SubContent } from "./subexport";
import { isCjk, sameChars, shortLines, splitInto, type PairLines } from "./clipLines";
import { pairBySentence, tidyLine } from "./subtitleText";
import logoUrl from "../assets/logo.png";

/** Output shape: the source's own, or a platform size. */
export type ClipAspect = "source" | "9:16" | "3:4" | "1:1" | "16:9";
/** "card": the whole picture in a box, title above and subtitles below; "fill": cropped to cover the canvas. */
export type ClipFit = "card" | "fill";
/** Card only: show the picture whole, or cropped to 4:3 / 1:1 around the speaker (a bigger picture). */
export type CardCrop = "none" | "4:3" | "1:1";
export type CardBg = "blur" | "night" | "paper";
export type SubStyleId = "cinema" | "amber" | "pill";
/** Old saved layouts, read once from preferences. */
export type ClipLayout = "landscape" | "portrait_card" | "portrait_fill";

export interface ClipGeometry {
  W: number;
  H: number;
  fit: ClipFit;
  /** Where the picture sits (the whole canvas when filled). */
  video: { x: number; y: number; w: number; h: number };
  /** Card: the picture is cropped to the box (not fitted whole). */
  cropBox: boolean;
  vertical: boolean;
}

export interface ClipVisualConfig {
  geo: ClipGeometry;
  cardBg: CardBg;
  subStyle: SubStyleId;
  subLang: SubContent;
  showTitle: boolean;
  title: string;
  showSource: boolean;
  showName: string;
  episodeTitle: string;
  /** The user's own line (a brand, an account name); nothing when empty. */
  brandText: string;
  watermark: boolean;
  liftForBurnedSub: boolean;
  /** Subtitle size, relative to the default (0.7–1.5). */
  subScale?: number;
}

export interface RenderedCueBand {
  canvas: HTMLCanvasElement;
  y: number;
}

const SANS = '-apple-system, "PingFang SC", "Hiragino Sans GB", "Noto Sans SC", "Microsoft YaHei", sans-serif';
const NO_START = /^[，。、：；！？）」』》”’,.;:!?)\]…—]/;

let sharedProbeCtx: CanvasRenderingContext2D | null = null;
function getProbeCtx(): CanvasRenderingContext2D {
  if (!sharedProbeCtx) sharedProbeCtx = document.createElement("canvas").getContext("2d")!;
  return sharedProbeCtx;
}
const even = (v: number) => Math.max(2, Math.round(v / 2) * 2);

const RATIO: Record<Exclude<ClipAspect, "source">, [number, number]> = { "9:16": [1080, 1920], "3:4": [1080, 1440], "1:1": [1080, 1080], "16:9": [1920, 1080] };

export function fromOldLayout(l: ClipLayout | undefined): { aspect: ClipAspect; fit: ClipFit } {
  if (l === "portrait_fill") return { aspect: "9:16", fit: "fill" };
  if (l === "landscape") return { aspect: "source", fit: "fill" };
  return { aspect: "9:16", fit: "card" };
}

/** Canvas size and picture box for an aspect, a fit and the source's size. */
export function clipGeometry(aspect: ClipAspect, fit: ClipFit, srcW = 1920, srcH = 1080, cardCrop: CardCrop = "none", subsBelow = true): ClipGeometry {
  const sw = srcW > 0 ? srcW : 1920, sh = srcH > 0 ? srcH : 1080;
  let W: number, H: number;
  if (aspect === "source") {
    if (sw >= sh) { W = 1920; H = even((1920 * sh) / sw); } else { H = 1920; W = even((1920 * sw) / sh); }
  } else [W, H] = RATIO[aspect];
  const vertical = H > W * 1.05;
  // Same shape as the source: nothing to fit.
  const same = Math.abs(W / H - sw / sh) < 0.03;
  if (fit === "fill" || same) return { W, H, fit: "fill", video: { x: 0, y: 0, w: W, h: H }, vertical, cropBox: false };
  // A narrower box than the source (4:3, 1:1) crops the sides around the speaker: a bigger picture.
  const boxRatio = cardCrop === "4:3" ? 4 / 3 : cardCrop === "1:1" ? 1 : sw / sh;
  const cropBox = boxRatio < sw / sh - 0.01;
  const ratio = cropBox ? boxRatio : sw / sh;
  let vw = W, vh = even(W / ratio);
  const maxH = H * (vertical ? 0.62 : 0.72);
  if (vh > maxH) { vh = even(maxH); vw = even(vh * ratio); }
  // Room above for the title and below for subtitles; with no subtitles of ours, title and picture are
  // centred together instead of leaving an empty band at the bottom.
  const free = H - vh;
  const y = even(free * (subsBelow ? (vertical ? 0.5 : 0.56) : vertical ? 0.62 : 0.66));
  return { W, H, fit: "card", video: { x: even((W - vw) / 2), y, w: vw, h: vh }, vertical, cropBox };
}

/** Clip subtitles relative to `clipStart` (0-based): one short line per cue, timed by length within its segment,
 *  the way short-video subtitles read. `lineBreaks` holds model-made lines (by segment index) for the CJK text;
 *  anything else is split here. Bilingual cues pair by meaning (`bilingualParts`), never by length alone. */
export function buildClipCues(
  segments: Segment[],
  zhOverrides: (string | null | undefined)[],
  startIdx: number,
  endIdx: number,
  clipStart: number,
  clipEnd: number,
  content: SubContent,
  lineBreaks?: Record<number, string[]>,
  maxCjk = 14,
  pairBreaks?: Record<number, PairLines>,
): Cue[] {
  const dur = Math.max(0.5, clipEnd - clipStart);
  const raw: Cue[] = [];
  for (let i = startIdx; i <= endIdx; i++) {
    const seg = segments[i];
    if (!seg) continue;
    const orig = seg.text.trim();
    const tr = zhOverrides[i]?.trim() || "";
    const lead = content === "translation" && tr ? tr : orig;
    if (!lead) continue;
    const isZhLead = lead === tr;
    let rows: { orig: string; zh?: string }[];
    if (content === "bilingual" && tr && !isZhLead && tr !== orig) {
      rows = bilingualParts(orig, tr, maxCjk, pairBreaks?.[i]);
    } else {
      const model = lineBreaks?.[i];
      const lines = model && sameChars(model, lead) ? model : shortLines(lead, maxCjk);
      rows = lines.map((line) => (isZhLead ? { orig: "", zh: line } : { orig: line }));
    }
    if (!rows.length) continue;
    const size = (r: { orig: string; zh?: string }) => (r.orig || r.zh || "").length;
    const total = rows.reduce((n, r) => n + size(r), 0) || 1;
    let acc = 0;
    for (const r of rows) {
      const s = seg.start + (seg.end - seg.start) * (acc / total);
      acc += size(r);
      const e = seg.start + (seg.end - seg.start) * (acc / total);
      raw.push({ start: s, end: e, orig: r.orig, zh: r.zh || undefined });
    }
  }

  const out: Cue[] = [];
  for (const c of raw) {
    const start = Math.max(0, c.start - clipStart);
    const end = Math.min(dur, c.end - clipStart);
    if (end - start < 0.08 || (!c.orig && !c.zh)) continue;
    const prev = out[out.length - 1];
    // A line that would flash by joins its neighbour when the two still fit on one line.
    const len = (x: Cue) => (x.orig || x.zh || "").length;
    if (prev && end - start < 0.7 && len(prev) + len(c) <= maxCjk * (isCjk(c.orig || c.zh || "") ? 1 : 2.3)) {
      prev.end = end;
      if (c.orig) prev.orig = `${prev.orig} ${c.orig}`.trim();
      if (c.zh) prev.zh = `${prev.zh ?? ""}${prev.zh ? " " : ""}${c.zh}`;
      continue;
    }
    if (prev && prev.end > start) prev.end = Math.max(prev.start + 0.08, start);
    else if (prev && start - prev.end < 0.25) prev.end = start; // no blink between lines
    out.push({ start, end, orig: c.orig, zh: c.zh });
  }
  return out;
}

/** A bilingual segment as cue rows whose two lines mean the same thing: the model's split when it kept both texts
 *  whole, else one row per sentence when both sides have the same sentences and each fits one line. A longer
 *  pair is split by length: the CJK side at word boundaries, the other side between words in proportion. */
function bilingualParts(orig: string, tr: string, maxCjk: number, model?: PairLines): { orig: string; zh?: string }[] {
  if (model && model.orig.length === model.tr.length && sameChars(model.orig, orig) && sameChars(model.tr, tr)) {
    return model.orig.map((o, k) => ({ orig: o, zh: model.tr[k] })).filter((r) => r.orig || r.zh);
  }
  // One line per side: the renderer wraps CJK by pixel width, character by character, so anything longer would
  // break mid-word again (or lose its tail past two lines).
  const fits = (text: string) => text.length <= (isCjk(text) ? maxCjk : Math.round(maxCjk * 2.3));
  const rows: { orig: string; zh?: string }[] = [];
  for (const p of pairBySentence(orig, tr) ?? [{ orig, tr }]) {
    if (fits(p.orig) && fits(p.tr)) { rows.push({ orig: tidyLine(p.orig), zh: tidyLine(p.tr) }); continue; }
    // The CJK side leads (its splitter breaks only between words); the other side splits into as many parts
    // between words. The parts may not line up exactly, but no word is cut in either language.
    const trLeads = isCjk(p.tr) && !isCjk(p.orig);
    const lines = shortLines(trLeads ? p.tr : p.orig, maxCjk);
    const second = splitInto(trLeads ? p.orig : p.tr, lines.length, lines.map((l) => l.length));
    lines.forEach((line, k) => rows.push(trLeads ? { orig: second[k] || "", zh: line } : { orig: line, zh: second[k] || undefined }));
  }
  return rows.filter((r) => r.orig || r.zh);
}

function wrapText(g: CanvasRenderingContext2D, text: string, maxW: number, maxLines: number): string[] {
  const clean = text.replace(/\s+/g, " ").trim();
  if (!clean) return [];
  const toks = clean.match(/[A-Za-z0-9'’\-.,;:!?%$]+\s*|\s+|./gu) || [];
  const lines: string[] = [];
  let line = "";
  for (const tok of toks) {
    if (!line && /^\s+$/.test(tok)) continue;
    const test = line + tok;
    if (g.measureText(test.trimEnd()).width > maxW && line) {
      if (NO_START.test(tok) && line.length > 1) {
        lines.push(line.slice(0, -1).trimEnd());
        line = line.slice(-1) + tok;
      } else {
        lines.push(line.trimEnd());
        line = /^\s+$/.test(tok) ? "" : tok;
      }
      if (lines.length === maxLines) break;
    } else {
      line = test;
    }
  }
  if (lines.length < maxLines && line.trim()) lines.push(line.trimEnd());
  const used = lines.join("").replace(/\s/g, "").length;
  if (used < clean.replace(/\s/g, "").length - 1 && lines.length) {
    let last = lines[lines.length - 1];
    while (last.length > 1 && g.measureText(`${last}…`).width > maxW) last = last.slice(0, -1);
    lines[lines.length - 1] = `${last}…`;
  }
  return lines;
}

function roundRectPath(g: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  g.beginPath();
  if (g.roundRect) g.roundRect(x, y, w, h, r);
  else g.rect(x, y, w, h);
}


/** Lines for a title: at punctuation when that fits, else by width. 【】 marks are kept for highlighting. */
function titleLines(g: CanvasRenderingContext2D, text: string, maxW: number, maxLines: number): string[] {
  const plain = (x: string) => x.replace(/[【】]/g, "");
  const width = (x: string) => g.measureText(plain(x)).width;
  const clean = text.replace(/\s+/g, " ").trim();
  if (!clean) return [];
  if (width(clean) <= maxW) return [clean];
  // Break after the punctuation mark closest to the middle when both halves fit.
  const marks = [...clean.matchAll(/[，、：；！？,:;!?—]/g)].map((m) => (m.index ?? 0) + 1).filter((i) => i > 1 && i < clean.length - 1);
  marks.sort((a, b) => Math.abs(a - clean.length / 2) - Math.abs(b - clean.length / 2));
  for (const i of marks) {
    const [l1, l2] = [clean.slice(0, i).trim(), clean.slice(i).trim()];
    if (width(l1) <= maxW && width(l2) <= maxW) return [l1, l2];
  }
  // Balanced split by width (never one long line and a stub).
  const chars = [...clean];
  for (let lines = 2; lines <= maxLines; lines++) {
    const per = Math.ceil(chars.length / lines);
    const out: string[] = [];
    for (let k = 0; k < chars.length; k += per) out.push(chars.slice(k, k + per).join(""));
    // Keep 【…】 whole and closing punctuation off a line start.
    for (let k = 1; k < out.length; k++) {
      while (NO_START.test(out[k]) && out[k].length > 1) { out[k - 1] += out[k][0]; out[k] = out[k].slice(1); }
    }
    if (out.every((l) => width(l) <= maxW)) return out;
  }
  return wrapText(g, plain(clean), maxW, maxLines);
}

/** Draws a centered line where 【…】 parts take the accent colour. */
function drawMarked(g: CanvasRenderingContext2D, line: string, cx: number, y: number, ink: string, accent: string, outline?: number) {
  const parts = line.split(/(【[^】]*】?)/).filter(Boolean).map((p) => ({ text: p.replace(/[【】]/g, ""), hot: p.startsWith("【") }));
  const total = parts.reduce((n, p) => n + g.measureText(p.text).width, 0);
  let x = cx - total / 2;
  for (const p of parts) {
    if (outline) { g.lineJoin = "round"; g.lineWidth = outline; g.strokeStyle = "rgba(0,0,0,0.85)"; g.strokeText(p.text, x, y); }
    g.fillStyle = p.hot ? accent : ink;
    g.fillText(p.text, x, y);
    x += g.measureText(p.text).width;
  }
}

const logo = typeof Image !== "undefined" ? Object.assign(new Image(), { src: logoUrl }) : null;

/** The static layer: title (with show name), source line, the user's brand line and the watermark. */
export function renderStaticOverlay(config: ClipVisualConfig): HTMLCanvasElement | null {
  const { geo } = config;
  const { W, H, video, vertical } = geo;
  const title = config.showTitle ? config.title.trim() : "";
  const brand = config.brandText.trim();
  const source = config.showSource ? [config.showName.trim(), config.episodeTitle.trim()].filter(Boolean).join(" · ") : "";
  if (!title && !brand && !source && !config.watermark) return null;
  const c = document.createElement("canvas");
  c.width = W;
  c.height = H;
  const g = c.getContext("2d")!;
  g.textBaseline = "alphabetic";
  const card = geo.fit === "card";
  const paper = card && config.cardBg === "paper";
  const ink = paper ? "#141416" : "#ffffff";
  const soft = paper ? "rgba(20,20,22,0.58)" : "rgba(255,255,255,0.74)";
  const accent = paper ? "#d9480f" : "#ffc53d";
  const unit = Math.min(W, H) / 1080;

  if (title) {
    const size = Math.round((vertical ? 64 : 58) * unit * (W > H ? 1.15 : 1));
    g.font = `800 ${size}px ${SANS}`;
    const maxW = W - Math.round((vertical ? 120 : 220) * unit);
    const lines = titleLines(g, title, maxW, 3);
    const lh = Math.round(size * 1.3);
    const kicker = config.showName.trim();
    const kSize = Math.round(28 * unit);
    let top: number;
    if (card) {
      // Bottom of the title block sits just above the picture.
      const blockH = lines.length * lh + (kicker ? kSize + 22 : 0);
      top = Math.max(Math.round(40 * unit), video.y - Math.round(44 * unit) - blockH);
    } else {
      // Filled: over a soft top shade, below the platform's top bar.
      const shadeH = Math.round(H * (vertical ? 0.3 : 0.34));
      const grad = g.createLinearGradient(0, 0, 0, shadeH);
      grad.addColorStop(0, "rgba(0,0,0,0.62)");
      grad.addColorStop(1, "rgba(0,0,0,0)");
      g.fillStyle = grad;
      g.fillRect(0, 0, W, shadeH);
      top = Math.round(H * (vertical ? 0.09 : 0.07));
    }
    if (kicker) {
      g.font = `600 ${kSize}px ${SANS}`;
      const label = kicker;
      const kw = g.measureText(label).width;
      const padX = Math.round(16 * unit), bh = kSize + Math.round(14 * unit);
      g.fillStyle = paper ? "rgba(217,72,15,0.1)" : "rgba(255,197,61,0.16)";
      roundRectPath(g, (W - kw) / 2 - padX, top, kw + padX * 2, bh, bh / 2);
      g.fill();
      g.fillStyle = accent;
      g.fillText(label, (W - kw) / 2, top + kSize + Math.round(2 * unit));
      top += bh + Math.round(18 * unit);
    }
    g.font = `800 ${size}px ${SANS}`;
    if (!paper) { g.shadowColor = "rgba(0,0,0,0.5)"; g.shadowBlur = 18 * unit; g.shadowOffsetY = 3 * unit; }
    lines.forEach((ln, i) => drawMarked(g, ln, W / 2, top + size * 0.92 + i * lh, ink, accent));
    g.shadowColor = "transparent"; g.shadowBlur = 0; g.shadowOffsetY = 0;
  }

  // Bottom lines: where the episode came from, then the user's own brand.
  const foot = [source, brand].filter(Boolean);
  if (foot.length) {
    const fs = Math.round(24 * unit);
    g.font = `500 ${fs}px ${SANS}`;
    let y = H - Math.round((vertical ? 92 : 44) * unit) - (foot.length - 1) * Math.round(fs * 1.6);
    if (!card) { g.shadowColor = "rgba(0,0,0,0.7)"; g.shadowBlur = 8 * unit; }
    foot.forEach((line, i) => {
      const l = wrapText(g, line, W - Math.round(160 * unit), 1)[0] ?? "";
      g.fillStyle = i === foot.length - 1 && brand ? (paper ? "rgba(20,20,22,0.72)" : "rgba(255,255,255,0.86)") : soft;
      g.font = `${i === foot.length - 1 && brand ? 600 : 500} ${fs}px ${SANS}`;
      g.fillText(l, (W - g.measureText(l).width) / 2, y);
      y += Math.round(fs * 1.6);
    });
    g.shadowColor = "transparent"; g.shadowBlur = 0;
  }

  if (config.watermark && logo?.complete && logo.naturalWidth) {
    // Top-right of the picture, small and quiet.
    const ls = Math.round(40 * unit);
    const m = Math.round(22 * unit);
    const fs = Math.round(22 * unit);
    g.font = `700 ${fs}px ${SANS}`;
    const word = "SubCast";
    const ww = g.measureText(word).width;
    const x = video.x + video.w - m - ls - 8 * unit - ww;
    const y = video.y + m + (card ? 0 : Math.round(H * (vertical ? 0.02 : 0)));
    g.globalAlpha = 0.82;
    g.shadowColor = "rgba(0,0,0,0.45)"; g.shadowBlur = 6 * unit;
    g.save();
    roundRectPath(g, x, y, ls, ls, ls * 0.24);
    g.clip();
    g.drawImage(logo, x, y, ls, ls);
    g.restore();
    g.fillStyle = "#ffffff";
    g.fillText(word, x + ls + 8 * unit, y + ls / 2 + fs * 0.36);
    g.globalAlpha = 1;
    g.shadowColor = "transparent"; g.shadowBlur = 0;
  }
  return c;
}

/** One subtitle band (transparent, full width) and where it goes. Bands of one export share a bottom line. */
export function renderCueBand(cue: Pick<Cue, "orig" | "zh">, config: ClipVisualConfig): RenderedCueBand {
  const { geo } = config;
  const { W, H, video, vertical } = geo;
  const unit = Math.min(W, H) / 1080;
  const card = geo.fit === "card";
  const scale = (W > H ? 1.12 : 1) * (config.subScale ?? 1);
  const zhSize = Math.round((card ? 54 : 60) * unit * scale);
  const origSize = Math.round((card ? 34 : 38) * unit * scale);
  const zhLH = Math.round(zhSize * 1.3);
  const origLH = Math.round(origSize * 1.34);
  const padX = Math.round(60 * unit);
  const maxTextW = W - padX * 2;

  const probe = getProbeCtx();
  probe.font = `700 ${zhSize}px ${SANS}`;
  const zhLines = config.subLang !== "original" && cue.zh ? wrapText(probe, cue.zh, maxTextW, 2) : [];
  probe.font = `500 ${origSize}px ${SANS}`;
  const origLines = (config.subLang !== "translation" || !zhLines.length) && cue.orig ? wrapText(probe, cue.orig, maxTextW, 2) : [];

  const lineGap = Math.round(8 * unit);
  const gap = zhLines.length && origLines.length ? lineGap : 0;
  const textH = zhLines.length * zhLH + gap + origLines.length * origLH;
  const padY = Math.round((config.subStyle === "pill" ? 22 : 14) * unit);
  // Uniform band height & Y per export config so FFmpeg streams all cues through one O(1) concat overlay.
  const maxTextH =
    config.subLang === "bilingual"
      ? 2 * zhLH + lineGap + 2 * origLH
      : config.subLang === "original"
        ? 2 * origLH
        : Math.max(2 * zhLH, 2 * origLH);
  const bandH = Math.max(Math.round(70 * unit), Math.max(textH, maxTextH) + padY * 2);

  const c = document.createElement("canvas");
  c.width = W;
  c.height = bandH;
  const g = c.getContext("2d")!;
  g.textBaseline = "alphabetic";
  let maxLineW = 0;
  g.font = `700 ${zhSize}px ${SANS}`;
  for (const ln of zhLines) maxLineW = Math.max(maxLineW, g.measureText(ln).width);
  g.font = `500 ${origSize}px ${SANS}`;
  for (const ln of origLines) maxLineW = Math.max(maxLineW, g.measureText(ln).width);

  const paper = card && config.cardBg === "paper";
  const below = H - (video.y + video.h);
  const sitsUnderCard = card && below >= bandH + 40 * unit;
  // Below a card: align to top of the band; overlaid on video: anchor to bottom so cues share one baseline.
  const boxTop = sitsUnderCard ? padY : Math.max(padY, bandH - textH - padY);
  if (config.subStyle === "pill" && maxLineW > 0) {
    const px = Math.round(30 * unit), py = Math.round(14 * unit);
    const pw = Math.min(W - padX, maxLineW + px * 2);
    g.fillStyle = paper ? "rgba(255,255,255,0.94)" : "rgba(14,14,18,0.78)";
    roundRectPath(g, (W - pw) / 2, boxTop - py, pw, textH + py * 2, Math.round(18 * unit));
    g.fill();
  }
  const zhColor = paper ? (config.subStyle === "amber" ? "#b45309" : "#141416") : config.subStyle === "amber" ? "#ffd84d" : "#ffffff";
  const origColor = paper ? "#52525b" : config.subStyle === "amber" ? "#ffffff" : "#ececef";
  const outline = config.subStyle !== "pill" && !paper;
  let curY = boxTop;
  const line = (ln: string, size: number, weight: number, color: string, width: number, lh: number) => {
    g.font = `${weight} ${size}px ${SANS}`;
    const x = (W - g.measureText(ln).width) / 2;
    const base = curY + size * 0.92;
    if (outline) { g.lineJoin = "round"; g.lineWidth = width * unit; g.strokeStyle = "rgba(0,0,0,0.9)"; g.strokeText(ln, x, base); }
    g.fillStyle = color;
    g.fillText(ln, x, base);
    curY += lh;
  };
  for (const ln of zhLines) line(ln, zhSize, 700, zhColor, 8, zhLH);
  curY += gap;
  for (const ln of origLines) line(ln, origSize, 500, origColor, 6, origLH);

  // Card: under the picture. Filled: above the platform's bottom bar (or above burned-in subtitles).
  let y: number;
  if (card) {
    const below = H - (video.y + video.h);
    y = below >= bandH + 40 * unit ? video.y + video.h + Math.round(36 * unit) : video.y + video.h - bandH - Math.round(24 * unit);
  } else {
    const safe = vertical ? H * 0.2 : H * 0.07;
    y = Math.round(H - safe - bandH - (config.liftForBurnedSub ? H * (vertical ? 0.06 : 0.12) : 0));
  }
  return { canvas: c, y: Math.max(0, y) };
}

export function canvasToPngBase64(canvas: HTMLCanvasElement): string {
  return canvas.toDataURL("image/png").replace(/^data:image\/png;base64,/, "");
}
