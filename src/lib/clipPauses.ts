/** "Remove pauses": which stretches of a clip to cut, from the pauses found when it was fetched.
 *  Two clocks: raw clip time (what the fetched file plays) and kept time (the export, cuts removed). */

export type Range = [number, number];

export interface PauseCuts {
  /** Stretches cut out (raw clip time), in order. */
  cuts: Range[];
  /** Stretches kept (raw clip time), in order; what the export joins. */
  keep: Range[];
  /** Seconds the cuts take off. */
  saved: number;
}

/** Only pauses this long are cut; a little of each side stays so words don't clip. */
const MIN_PAUSE = 0.45;
const MARGIN = 0.12;

/** Cuts every pause of `MIN_PAUSE` or more inside the clip, leaving `MARGIN` either side.
 *  Pauses touching the clip's edges stay: they are the lead-in and tail the user set. */
export function pauseCuts(silences: Range[] | undefined, duration: number): PauseCuts {
  const cuts: Range[] = [];
  for (const [a, b] of [...(silences ?? [])].sort((x, y) => x[0] - y[0])) {
    if (a < 0.05 || b > duration - 0.05 || b - a < MIN_PAUSE) continue;
    const ca = a + MARGIN;
    const cb = b - MARGIN;
    if (cb - ca < 0.2) continue;
    const last = cuts[cuts.length - 1];
    if (last && ca <= last[1]) last[1] = Math.max(last[1], cb);
    else cuts.push([ca, cb]);
  }
  const keep: Range[] = [];
  let at = 0;
  for (const [a, b] of cuts) {
    if (a > at) keep.push([at, a]);
    at = b;
  }
  if (duration > at) keep.push([at, duration]);
  const saved = cuts.reduce((n, [a, b]) => n + (b - a), 0);
  return { cuts, keep, saved };
}

/** A raw clip moment on the cut clock (a moment inside a cut lands where the cut closes). */
export function toKeptTime(t: number, cuts: Range[]): number {
  let removed = 0;
  for (const [a, b] of cuts) {
    if (t <= a) break;
    removed += Math.min(t, b) - a;
  }
  return Math.max(0, t - removed);
}

/** The cut that holds raw moment `t`, if any. */
export function cutAt(t: number, cuts: Range[]): Range | undefined {
  return cuts.find(([a, b]) => t >= a && t < b - 0.02);
}
