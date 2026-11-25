// The first seconds of a clip decide whether people keep watching. A clip that opens on "so…" / "所以…" or in
// the middle of a sentence leaves a viewer without the context; this spots that and suggests a better start.
import type { Segment } from "./api";

const LEAD_ZH = /^(然后|所以|但是|但|而且|因为|那么|那个|就是|嗯|呃)/;
const LEAD_EN = /^(and|so|but|because|yeah|yes|um|uh|right|well|or|then|like)\b/i;
const ENDS = /[。！？.!?…]["”')）]*$/;

export type OpeningIssue = { kind: "lead-word"; word: string } | { kind: "mid-sentence" };

function leadWord(text: string): string | null {
  const s = text.trim();
  return s.match(LEAD_ZH)?.[1] ?? s.match(LEAD_EN)?.[1] ?? null;
}

/** Whether sentence `i` makes a weak opening, and why. Mid-sentence is only judged where the subtitles carry
 *  punctuation (auto-captions without any can't tell a sentence end from a pause). */
export function openingIssue(segs: Segment[], i: number): OpeningIssue | null {
  const s = segs[i];
  if (!s) return null;
  const w = leadWord(s.text);
  if (w) return { kind: "lead-word", word: w };
  const prev = segs[i - 1];
  const punctuated = segs.slice(Math.max(0, i - 6), i + 6).some((x) => ENDS.test(x.text.trim()));
  if (prev && punctuated && !ENDS.test(prev.text.trim()) && s.start - prev.end < 0.35) return { kind: "mid-sentence" };
  return null;
}

/** A better first sentence within the next few: one that opens cleanly, preferring a question or a short line
 *  (hooks); never past `endIdx` minus one so the clip keeps at least two sentences. */
export function betterStart(segs: Segment[], startIdx: number, endIdx: number): number | null {
  const last = Math.min(endIdx - 1, startIdx + 5);
  let fallback: number | null = null;
  for (let i = startIdx + 1; i <= last; i++) {
    if (openingIssue(segs, i)) continue;
    const t = segs[i].text.trim();
    if (/[？?]["”')）]*$/.test(t) || [...t].length <= 20) return i;
    fallback ??= i;
  }
  return fallback;
}
