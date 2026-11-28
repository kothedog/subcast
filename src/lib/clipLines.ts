// Subtitle lines for exported clips: short, one idea per line, the way short-video subtitles read. The text
// rules (punctuation, where Chinese may break, bilingual pairing) live in ./subtitleText.
// A deterministic splitter always works; DeepSeek (or the chat model) can add the punctuation that YouTube's
// automatic captions lack, which gives much better breaks. Its answer is only used where it kept every character.
import { llmChat, type Segment } from "./api";
import { llmReady } from "./jobs";
import { pieces } from "./pieces";
import { providerFor, useStore } from "../store";
import { isCjk, splitCjk, tidyLine, wordEnds } from "./subtitleText";

export { isCjk };
const BREAK_AFTER = /[，。、；：！？,.;:!?…]/u;

/** Splits text into subtitle lines of at most `max` characters (CJK) or `max * 2.3` characters (Latin). */
export function shortLines(text: string, max: number): string[] {
  const clean = text.replace(/\s+/g, " ").trim();
  if (!clean) return [];
  const cjk = isCjk(clean);
  const limit = cjk ? max : Math.round(max * 2.3);
  const out: string[] = [];
  for (const p of pieces(clean, cjk)) {
    if (p.length <= limit) { out.push(p); continue; }
    out.push(...(cjk ? splitCjk(p, limit) : splitWords(p, limit)));
  }
  return out.map(tidyLine).filter(Boolean);
}

function splitWords(text: string, limit: number): string[] {
  const words = text.split(" ");
  const parts = Math.ceil(text.length / limit);
  const target = text.length / parts;
  const out: string[] = [];
  let line = "";
  for (const w of words) {
    if (line && (line.length + 1 + w.length > limit || (line.length >= target && out.length < parts - 1))) { out.push(line); line = w; }
    else line = line ? `${line} ${w}` : w;
  }
  if (line) out.push(line);
  return out;
}

/** Splits `text` into `n` parts, only between words, sized like `shares` (the other language's lines, so the two
 *  sides line up), or evenly without them; prefers punctuation within two units of each cut. */
export function splitInto(text: string, n: number, shares?: number[]): string[] {
  const clean = text.replace(/\s+/g, " ").trim();
  if (n <= 1 || !clean) return [tidyLine(clean)];
  const cjk = isCjk(clean);
  const units = cjk ? [...clean] : clean.split(" ");
  if (units.length <= n) return units.concat(Array(n - units.length).fill("")).map(tidyLine);
  const ok = cjk ? wordEnds(clean) : null; // CJK: cut only where a word ends
  const len = units.map((u) => u.length + (cjk ? 0 : 1));
  const total = len.reduce((x, y) => x + y, 0);
  const w = shares && shares.length === n && shares.every((x) => x > 0) ? shares : Array(n).fill(1);
  const wTotal = w.reduce((x, y) => x + y, 0);
  const out: string[] = [];
  let from = 0, acc = 0, wAcc = 0, pos = 0;
  for (let k = 1; k < n; k++) {
    wAcc += w[k - 1];
    const goal = (total * wAcc) / wTotal;
    while (pos < units.length && acc + len[pos] / 2 < goal) acc += len[pos++];
    let cut = pos;
    let best = -1, bestScore = Infinity;
    const hi = units.length - (n - k);
    for (let c = Math.max(from + 1, cut - 3); c <= Math.min(hi, cut + 3); c++) {
      if (ok && !ok.has(c)) continue;
      const score = Math.abs(c - cut) - (BREAK_AFTER.test(units[c - 1].slice(-1)) ? 2 : 0);
      if (score < bestScore) { bestScore = score; best = c; }
    }
    cut = best >= 0 ? best : Math.min(Math.max(cut, from + 1), hi);
    out.push(units.slice(from, cut).join(cjk ? "" : " "));
    from = cut;
  }
  out.push(units.slice(from).join(cjk ? "" : " "));
  return out.map(tidyLine);
}

const bare = (s: string) => s.replace(/[\s\p{P}\p{S}]/gu, "");
/** The same characters, ignoring punctuation and spaces (model lines still match the current text). */
export const sameChars = (lines: string[], text: string) => bare(lines.join("")) === bare(text);

/** Model-made line breaks per segment index, kept only where the lines hold exactly the original characters. */
export async function aiLineBreaks(texts: { i: number; text: string }[], max: number, signal?: AbortSignal): Promise<Record<number, string[]>> {
  const items = texts.filter((x) => isCjk(x.text) && x.text.trim());
  if (!items.length) return {};
  if (!(await llmReady("chat"))) return {};
  const provider = providerFor(useStore.getState().settings, "chat");
  if (!provider?.model) return {};
  // i18n-ignore-start (prompt sent to the model)
  const system = `你是短视频字幕编辑。把每条口语字幕加上标点，再切成适合竖屏阅读的短句。
规则：
1. 每个短句不超过 ${max} 个字，按意群断开（主谓、动宾、量词短语不要拆开），不要在助词「的、了、着、吗、呢」前断开。
2. 只能添加标点，绝对不能增加、删除、改写、调换任何一个字。
3. 输入是 JSON 数组 [{"i":行号,"text":"字幕"}]。只返回 JSON：{"lines":[{"i":行号,"parts":["短句1","短句2"]}]}，每条输入都要有对应输出。`;
  // i18n-ignore-end
  const raw = await llmChat({
    provider,
    system,
    messages: [{ role: "user", content: JSON.stringify(items) }],
    maxTokens: 4000,
    jsonMode: provider.protocol === "openai",
    quick: true,
    signal,
  });
  const clean = raw.replace(/^```(?:json)?\s*|\s*```$/gm, "").trim();
  const a = clean.indexOf("{"), b = clean.lastIndexOf("}");
  if (a < 0 || b < a) return {};
  let parsed: { lines?: { i?: unknown; parts?: unknown }[] };
  try { parsed = JSON.parse(clean.slice(a, b + 1)); } catch { return {}; }
  const byIdx = new Map(items.map((x) => [x.i, x.text]));
  const out: Record<number, string[]> = {};
  for (const l of parsed.lines ?? []) {
    const i = Number(l.i);
    const src = byIdx.get(i);
    if (src == null || !Array.isArray(l.parts)) continue;
    const parts = l.parts.map((p) => String(p));
    if (bare(parts.join("")) !== bare(src)) continue;
    // Still too long somewhere: the deterministic splitter finishes the job on that line.
    out[i] = parts.flatMap((p) => shortLines(p, max));
  }
  return out;
}

/** The texts (by segment index) whose lines the model should break: the CJK text that drives each cue. */
export function textsToBreak(segments: Segment[], zh: (string | null | undefined)[], startIdx: number, endIdx: number, content: "bilingual" | "original" | "translation"): { i: number; text: string }[] {
  const out: { i: number; text: string }[] = [];
  for (let i = startIdx; i <= endIdx; i++) {
    const orig = segments[i]?.text ?? "";
    const tr = zh[i]?.trim() ?? "";
    const text = content === "original" ? orig : isCjk(orig) ? orig : tr;
    if (text && isCjk(text)) out.push({ i, text });
  }
  return out;
}

/** A bilingual segment split into matching parts: `orig[k]` and `tr[k]` say the same thing. */
export type PairLines = { orig: string[]; tr: string[] };

/** The segments (by index) whose original and translation the model should split together (bilingual cues). */
export function pairsToBreak(segments: Segment[], zh: (string | null | undefined)[], startIdx: number, endIdx: number): { i: number; orig: string; tr: string }[] {
  const out: { i: number; orig: string; tr: string }[] = [];
  for (let i = startIdx; i <= endIdx; i++) {
    const orig = segments[i]?.text?.trim() ?? "";
    const tr = zh[i]?.trim() ?? "";
    if (orig && tr && orig !== tr) out.push({ i, orig, tr });
  }
  return out;
}

/** Model-made bilingual splits per segment index, kept only where both sides hold exactly their original text and
 *  the part counts match. Splitting one side by length pairs lines that don't mean the same thing. */
export async function aiPairBreaks(items: { i: number; orig: string; tr: string }[], max: number, signal?: AbortSignal): Promise<Record<number, PairLines>> {
  if (!items.length) return {};
  if (!(await llmReady("chat"))) return {};
  const provider = providerFor(useStore.getState().settings, "chat");
  if (!provider?.model) return {};
  const latin = Math.round(max * 2.3);
  // i18n-ignore-start (prompt sent to the model)
  const system = `你是双语字幕编辑。每条输入有原文 orig 和它的译文 tr。把两边切成数量相同的几段，第 k 段原文和第 k 段译文必须意思对应。
规则：
1. 中文每段不超过 ${max} 个字，英文每段不超过 ${latin} 个字符；按意群断开，不拆词，不在「的、了、着、吗、呢」前断开。
2. 两边都只能切开、可以补标点，绝对不能增加、删除、改写、调换任何字或词；每一段必须是原文本里连续的一截。
3. 如果中英语序不同，导致切开后对不上，就少切几段，宁可整条不切（parts 各只有一段）。
4. 输入是 JSON 数组 [{"i":行号,"orig":"原文","tr":"译文"}]。只返回 JSON：{"lines":[{"i":行号,"orig":["原文段1",…],"tr":["译文段1",…]}]}，每条输入都要有对应输出。`;
  // i18n-ignore-end
  const raw = await llmChat({
    provider,
    system,
    messages: [{ role: "user", content: JSON.stringify(items) }],
    maxTokens: 6000,
    jsonMode: provider.protocol === "openai",
    quick: true,
    signal,
  });
  const clean = raw.replace(/^```(?:json)?\s*|\s*```$/gm, "").trim();
  const a = clean.indexOf("{"), b = clean.lastIndexOf("}");
  if (a < 0 || b < a) return {};
  let parsed: { lines?: { i?: unknown; orig?: unknown; tr?: unknown }[] };
  try { parsed = JSON.parse(clean.slice(a, b + 1)); } catch { return {}; }
  const byIdx = new Map(items.map((x) => [x.i, x]));
  const out: Record<number, PairLines> = {};
  for (const l of parsed.lines ?? []) {
    const src = byIdx.get(Number(l.i));
    if (!src || !Array.isArray(l.orig) || !Array.isArray(l.tr) || !l.orig.length || l.orig.length !== l.tr.length) continue;
    const orig = l.orig.map((p) => String(p)), tr = l.tr.map((p) => String(p));
    if (bare(orig.join("")) !== bare(src.orig) || bare(tr.join("")) !== bare(src.tr)) continue;
    // A part longer than one line would be wrapped mid-word by the renderer: use the deterministic split instead.
    const tooLong = (p: string) => p.length > (isCjk(p) ? max : latin);
    if (orig.some(tooLong) || tr.some(tooLong)) continue;
    out[src.i] = { orig: orig.map(tidyLine), tr: tr.map(tidyLine) };
  }
  return out;
}
