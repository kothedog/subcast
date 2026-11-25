// Clips exported on this computer, newest first: what was made, from which episode, and where the file is.
// Kept in the WebView's local storage (it only describes files on this disk); a lost list costs nothing.
import { create } from "zustand";

export interface ExportedClip {
  path: string;
  episodeId: string;
  episodeTitle: string;
  /** The clip's headline, or its first words. */
  title: string;
  /** Seconds, as exported (after cuts and speed). */
  duration: number;
  aspect: string;
  at: number;
}

const KEY = "subcast-clip-history";
const MAX = 60;

function load(): ExportedClip[] {
  try { const v = JSON.parse(localStorage.getItem(KEY) ?? "[]"); return Array.isArray(v) ? v : []; } catch { return []; }
}
function persist(list: ExportedClip[]) {
  try { localStorage.setItem(KEY, JSON.stringify(list)); } catch { /* storage unavailable: the list just won't survive a restart */ }
}

export const useClipHistory = create<{ items: ExportedClip[] }>(() => ({ items: load() }));

export function noteExport(c: ExportedClip) {
  const items = [c, ...useClipHistory.getState().items.filter((x) => x.path !== c.path)].slice(0, MAX);
  useClipHistory.setState({ items });
  persist(items);
}

export function forgetExport(path: string) {
  const items = useClipHistory.getState().items.filter((x) => x.path !== path);
  useClipHistory.setState({ items });
  persist(items);
}
