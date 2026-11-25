// Sentences for read-along: where they start and end in a block of text, the article HTML with each sentence
// marked, and an estimate of when each word is spoken (for TTS engines that report no timestamps).

/** Sentence ends: Chinese/English stops plus any closing quotes or brackets right after them. */
const END = /[。！？!?；;…]+["'”’」』）)】》]*|[.](?=\s)["'”’）)]*/gu;
/** Shorter than this, a sentence is read together with the one before it (e.g. a lone "家。"). */
const MIN_CHARS = 6;
/** Longer than this, a sentence is cut at a comma so the first audio arrives sooner and seeking stays fine. */
const MAX_CHARS = 160;

/** [start, end) ranges of the sentences in `text`, whitespace trimmed off each. */
export function splitSentences(text: string): [number, number][] {
  const raw: [number, number][] = [];
  let from = 0;
  for (const m of text.matchAll(END)) {
    const to = m.index! + m[0].length;
    raw.push([from, to]);
    from = to;
  }
  if (from < text.length) raw.push([from, text.length]);
  const out: [number, number][] = [];
  for (const [a0, b0] of raw) {
    let a = a0, b = b0;
    while (a < b && /\s/.test(text[a])) a++;
    while (b > a && /\s/.test(text[b - 1])) b--;
    if (a >= b) continue;
    // Too long: cut at the last comma before MAX_CHARS, repeatedly.
    while (b - a > MAX_CHARS) {
      const slice = text.slice(a, a + MAX_CHARS);
      const cut = Math.max(slice.lastIndexOf("，"), slice.lastIndexOf(","), slice.lastIndexOf("、"));
      if (cut < MAX_CHARS / 3) break;
      out.push([a, a + cut + 1]);
      a += cut + 1;
      while (a < b && /\s/.test(text[a])) a++;
    }
    const last = out[out.length - 1];
    if (last && b - a < MIN_CHARS) last[1] = b;
    else out.push([a, b]);
  }
  return out;
}

/** Blocks whose text is read; a block holding another of these is left to its children. */
const BLOCKS = "p, h2, h3, h4, li, blockquote, figcaption";

/** Wraps every sentence of the article's readable blocks in `<span class="ra-s" data-s="N">` (a sentence that
 *  crosses a link or bold text gets several spans with the same N). Timestamp markers ([data-t]) are skipped.
 *  Returns the new HTML and the sentences' text, index = N. */
export function markSentences(html: string): { html: string; sentences: string[] } {
  const doc = new DOMParser().parseFromString(`<div>${html}</div>`, "text/html");
  const root = doc.body.firstElementChild!;
  const sentences: string[] = [];
  const blocks = Array.from(root.querySelectorAll(BLOCKS)).filter((b) => !b.querySelector(BLOCKS));
  for (const block of blocks) {
    const nodes: Text[] = [];
    const walker = doc.createTreeWalker(block, NodeFilter.SHOW_TEXT, {
      acceptNode: (n) => (n.parentElement?.closest("[data-t], code, pre") ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT),
    });
    for (let n = walker.nextNode(); n; n = walker.nextNode()) nodes.push(n as Text);
    const text = nodes.map((n) => n.data).join("");
    const ranges = splitSentences(text);
    if (!ranges.length) continue;
    const base = sentences.length;
    for (const [a, b] of ranges) sentences.push(text.slice(a, b));
    let offset = 0;
    for (const node of nodes) {
      const start = offset, end = offset + node.data.length;
      offset = end;
      const parts: Node[] = [];
      let cursor = start;
      ranges.forEach(([a, b], i) => {
        const s = Math.max(a, start), e = Math.min(b, end);
        if (s >= e) return;
        if (s > cursor) parts.push(doc.createTextNode(text.slice(cursor, s)));
        const span = doc.createElement("span");
        span.className = "ra-s";
        span.dataset.s = String(base + i);
        span.textContent = text.slice(s, e);
        parts.push(span);
        cursor = e;
      });
      if (!parts.length) continue;
      if (cursor < end) parts.push(doc.createTextNode(text.slice(cursor, end)));
      node.replaceWith(...parts);
    }
  }
  return { html: root.innerHTML, sentences };
}

// ---- Word timing estimate (after qiaomu-audio's core/tts/timing.js) ---------------------------------------------

const PAUSE = /[，、；;,:：]/u, STOP = /[。！？.!?…\n]/u, CLOSE = /["'”’）)」』】》]/u;

/** A word (or CJK character) of `text` [s, e) spoken during [from, to) seconds. */
export interface Mark { s: number; e: number; from: number; to: number }

function tokens(text: string) {
  const out: { s: number; e: number; weight: number }[] = [];
  const re = /[㐀-鿿぀-ヿ가-힯]|[\p{L}\p{N}'’_-]+|[^\s]/gu;
  for (const m of text.matchAll(re)) {
    const value = m[0], s = m.index!, e = s + value.length;
    const last = out[out.length - 1];
    if (/^[\p{P}\p{S}]$/u.test(value) && last) {
      // Punctuation belongs to the word before it and adds a pause.
      last.e = e;
      last.weight += STOP.test(value) ? 2.2 : PAUSE.test(value) ? 1.2 : CLOSE.test(value) ? 0 : 0.4;
      continue;
    }
    const cjk = /^[㐀-鿿぀-ヿ가-힯]$/u.test(value);
    out.push({ s, e, weight: cjk ? 1 : 1.3 + 0.18 * value.length });
  }
  return out;
}

/** Spreads `duration` seconds over the words of `text` by weight: right to within a word or two for prose. */
export function estimateMarks(text: string, duration: number): Mark[] {
  if (!(duration > 0)) return [];
  const list = tokens(text);
  const total = list.reduce((n, x) => n + x.weight, 0);
  if (!total) return [];
  let at = 0;
  return list.map((x) => { const from = at; at += (duration * x.weight) / total; return { s: x.s, e: x.e, from, to: at }; });
}

/** Characters of the sentence spoken by `time` seconds (the end of the word being spoken). */
export function spokenChars(marks: Mark[], time: number): number {
  let lo = 0, hi = marks.length - 1, found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (marks[mid].from <= time) { found = mid; lo = mid + 1; } else hi = mid - 1;
  }
  return found < 0 ? 0 : marks[found].e;
}
