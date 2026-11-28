// Subtitle text rules for exported clips, after the Netflix Timed Text Style Guides (Simplified Chinese and
// English) and the BBC subtitle guidelines. Pure functions, no app imports: `node src/lib/subtitleText.test.ts`.
//
// - A line never starts with punctuation, and ends only with ？！ (a question keeps its mark; commas, full
//   stops, colons and dangling ellipses go: the next line or the pause says the same).
// - Inside a Chinese line, ，。 become one space (Netflix SC: "Do not use commas or periods").
// - Chinese breaks only between words (Intl.Segmenter), preferring punctuation, never before a particle
//   (的了着过吗呢吧啊) or a locative 里, nor right after a preposition (在把被对给…), never between a numeral and
//   its measure word (第二|名), and never leaving a one- or two-character line.

const CJK = /[㐀-鿿豈-﫿]/;
export const isCjk = (s: string) => CJK.test(s);

/** Punctuation a line may not start with: everything except opening quotes/brackets that close in the line. */
const LEAD = /^[\s，。、；：！？…—·,.;:!?\-)）」』》”’]+/u;
/** Pause punctuation a line doesn't end with (？！ stay). */
const TAIL = /[\s，。、；：…—·,.;:\-]+$/u;
const PAIRS: [string, string][] = [["“", "”"], ["‘", "’"], ["「", "」"], ["『", "』"], ["《", "》"], ["（", "）"], ["(", ")"]];

const count = (s: string, ch: string) => s.split(ch).length - 1;

/** One subtitle line, cleaned to the rules above. */
export function tidyLine(text: string): string {
  let s = text.replace(/\s+/g, " ").trim();
  for (let pass = 0; pass < 3; pass++) {
    const before = s;
    s = s.replace(LEAD, "").replace(TAIL, "");
    // An opening mark whose partner is on another line, or a closing one whose opener is, goes.
    for (const [open, close] of PAIRS) {
      if (s.startsWith(open) && count(s, close) < count(s, open)) s = s.slice(open.length);
      if (s.endsWith(close) && count(s, open) < count(s, close)) s = s.slice(0, -close.length);
    }
    if (/^"/.test(s) && count(s, '"') % 2) s = s.slice(1);
    if (/"$/.test(s) && count(s, '"') % 2) s = s.slice(0, -1);
    s = s.trim();
    if (s === before) break;
  }
  // "？！", "!?", "??" → the last mark only.
  s = s.replace(/[？！?!]{2,}$/u, (m) => m.slice(-1));
  if (isCjk(s)) s = s.replace(/\s*[，。]\s*/g, " ").replace(/ {2,}/g, " ").trim();
  return s;
}

/** Punctuation a Chinese line may end on (the best place to break). */
const BREAK_AFTER = /[，。、；：！？,.;:!?…]/u;
/** Words that lean on the word before: a line may not start with them. */
const PARTICLE = /^[的了着过吗呢吧啊嘛么呀哦地得里]/u;
/** A numeral and its measure word stay together ("第二名", "三家", "五个月"). */
const NUMERAL_END = /[0-9一二三四五六七八九十百千万两几第]$/u;
const MEASURE_START = /^[名个家位种次年月天岁倍分秒层条件只张本元块]/u;
/** Words that lean on the word after: a line may not end with them. */
const LEADS_ON = /(在|把|被|对|给|和|跟|与|从|向|往|让|为|是|的|个|这|那|一)$/u;

/** Code-point offsets (into `[...text]`) where a word ends; every offset when the system can't segment. */
export function wordEnds(text: string): Set<number> {
  const chars = [...text];
  const seg = typeof Intl !== "undefined" && "Segmenter" in Intl ? new Intl.Segmenter("zh", { granularity: "word" }) : null;
  if (!seg) return new Set(chars.map((_, i) => i + 1));
  const ends = new Set<number>();
  let at = 0;
  for (const s of seg.segment(text)) { at += [...s.segment].length; ends.add(at); }
  return ends;
}

/** Splits Chinese text into lines of at most `limit` characters, at word boundaries, as even as it can. */
export function splitCjk(text: string, limit: number): string[] {
  const chars = [...text];
  const n = chars.length;
  if (n <= limit) return [text];
  const ends = wordEnds(text);
  const parts = Math.ceil(n / limit);
  const out: string[] = [];
  let from = 0;
  for (let k = 1; k < parts; k++) {
    const left = parts - k + 1;
    const ideal = from + (n - from) / left;
    // Leave the remaining parts enough room, and no line under three characters.
    const lo = Math.max(from + 3, n - limit * (left - 1));
    const hi = Math.min(from + limit, n - 3);
    let best = -1, bestScore = Infinity;
    for (let cut = lo; cut <= hi; cut++) {
      if (!ends.has(cut)) continue;
      const head = chars.slice(from, cut).join(""), tail = chars.slice(cut).join("");
      let score = Math.abs(cut - ideal);
      if (BREAK_AFTER.test(chars[cut - 1])) score -= 5;
      if (PARTICLE.test(tail)) score += 8;
      if (LEADS_ON.test(head)) score += 4;
      if (NUMERAL_END.test(head) && MEASURE_START.test(tail)) score += 8;
      if (score < bestScore) { bestScore = score; best = cut; }
    }
    if (best < 0) best = Math.min(Math.max(Math.round(ideal), from + 1), from + limit);
    out.push(chars.slice(from, best).join(""));
    from = best;
  }
  out.push(chars.slice(from).join(""));
  return out;
}

/** Sentences of a text, each ending in its own 。！？.!? (or the end). A "." inside a number doesn't end one. */
export function sentences(text: string): string[] {
  const clean = text.replace(/\s+/g, " ").trim();
  if (!clean) return [];
  const out = clean.split(/(?<=[。！？!?]["”’」』)）]*|\.(?!\d)["”’)]*\s)/u).map((s) => s.trim()).filter(Boolean);
  return out.length ? out : [clean];
}

/** A bilingual pair split into the same number of parts by sentence, or `null` when the sentence counts differ
 *  (then the pair stays one cue: splitting one language by length pairs lines that don't mean the same). */
export function pairBySentence(orig: string, tr: string): { orig: string; tr: string }[] | null {
  const a = sentences(orig), b = sentences(tr);
  if (a.length !== b.length) return null;
  return a.map((o, k) => ({ orig: o, tr: b[k] }));
}
