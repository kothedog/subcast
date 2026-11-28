// Cross-device sync state & auto-sync scheduler for iCloud Drive and WebDAV.
import { create } from "zustand";
import { api, errorText, setBlobMutatedHook, type BackendSyncSummary, type SyncReport, type SyncStatus } from "./api";
import { invalidateEpisodes } from "./cache";
import { setLang, setTranslateNames, t, tr } from "./i18n";
import { loadFavorites } from "./library";
import { loadSettings } from "./settings";
import { useStore } from "../store";

interface SyncState {
  busy: "icloud" | "webdav" | "all" | null;
  status: SyncStatus | null;
  refreshStatus: (customDir?: string) => Promise<SyncStatus | null>;
  run: (target?: "icloud" | "webdav", opts?: { quiet?: boolean }) => Promise<SyncReport | null>;
}

const SYNCED_BLOBS = new Set([
  "summary",
  "summary-from",
  "summary-model",
  "deep-read",
  "article",
  "chat",
  "chat-archive",
  "highlights",
  "notes-zh",
  "favorites",
  "notes",
]);

let autoTimer = 0;
let lastAutoRun = 0;

function anyEnabled(): boolean {
  const s = useStore.getState().settings.sync;
  return !!(s?.icloudEnabled || s?.webdavEnabled);
}

function summarizeReport(report: SyncReport): { text: string; hasError: boolean } {
  const errs = report.results.filter((r) => r.error).map((r) => `${r.backend === "icloud" ? "iCloud" : "WebDAV"}：${tr(r.error!)}`);
  if (errs.length && errs.length === report.results.length) {
    return { text: errs[0], hasError: true };
  }
  const added = report.results.reduce((n, r) => n + r.addedShows, 0);
  const removed = report.results.reduce((n, r) => n + r.removedShows, 0);
  const eps = report.results.reduce((n, r) => n + r.updatedEpisodes, 0);
  const content = report.results.reduce((n, r) => n + (r.updatedContent ?? 0), 0);
  const parts: string[] = [];
  if (added > 0) parts.push(t("新增 {n} 个订阅", { n: added }));
  if (removed > 0) parts.push(t("移除 {n} 个订阅", { n: removed }));
  if (eps > 0) parts.push(t("更新 {n} 集进度", { n: eps }));
  if (content > 0) parts.push(t("更新 {n} 集导读/问答", { n: content }));
  if (errs.length) {
    return { text: t("部分同步失败：{err}", { err: errs[0] }), hasError: true };
  }
  if (!parts.length) {
    return { text: t("同步完成，已是最新"), hasError: false };
  }
  return { text: t("同步完成：{detail}", { detail: parts.join(" · ") }), hasError: false };
}

export const useSyncState = create<SyncState>((set, get) => ({
  busy: null,
  status: null,
  async refreshStatus(customDir?: string) {
    try {
      const dir = customDir ?? useStore.getState().settings.sync?.icloudDir;
      const status = await api.syncStatus(dir);
      set({ status });
      return status;
    } catch {
      return null;
    }
  },
  async run(target, opts) {
    if (get().busy) return null;
    const quiet = !!opts?.quiet;
    set({ busy: target ?? "all" });
    try {
      const report = await api.syncRun(target);
      if (report.settingsChanged) {
        const fresh = await loadSettings();
        setLang(fresh.uiLang);
        setTranslateNames(fresh.translateShowNames);
        useStore.setState({ settings: fresh });
      }
      if (report.blobsChanged) {
        void loadFavorites();
      }
      const anyEp = report.results.some((r: BackendSyncSummary) => r.updatedEpisodes > 0);
      if (report.showsChanged || anyEp) {
        invalidateEpisodes();
        await useStore.getState().reloadShows();
      }
      await get().refreshStatus();
      if (!quiet) {
        const { text } = summarizeReport(report);
        useStore.getState().notify(text);
      } else {
        const added = report.results.reduce((n, r) => n + r.addedShows, 0);
        const removed = report.results.reduce((n, r) => n + r.removedShows, 0);
        if (added > 0 || removed > 0) {
          useStore.getState().notify(t("已同步 {n} 个订阅变更", { n: added + removed }));
        }
      }
      return report;
    } catch (e) {
      if (!quiet) useStore.getState().notify(errorText(e));
      return null;
    } finally {
      set({ busy: null });
    }
  },
}));

/** Schedules a quiet background sync after local subscription, alias, or content edits. */
export function scheduleAutoSync(delayMs = 2000) {
  const s = useStore.getState().settings.sync;
  if (!s?.autoSync || !anyEnabled()) return;
  window.clearTimeout(autoTimer);
  autoTimer = window.setTimeout(() => {
    autoTimer = 0;
    lastAutoRun = Date.now();
    void useSyncState.getState().run(undefined, { quiet: true });
  }, delayMs);
}

/** Runs a quiet background sync if auto-sync is enabled and `minIntervalMs` has elapsed. */
export function autoSyncIfDue(minIntervalMs = 2 * 60_000) {
  const s = useStore.getState().settings.sync;
  if (!s?.autoSync || !anyEnabled()) return;
  if (Date.now() - lastAutoRun < minIntervalMs) return;
  lastAutoRun = Date.now();
  void useSyncState.getState().run(undefined, { quiet: true });
}

let autoSyncStarted = false;

/** Initializes auto-sync timers, focus listeners, and mutation hooks after `useStore` is ready. */
export function startAutoSync() {
  if (autoSyncStarted) return;
  autoSyncStarted = true;
  setBlobMutatedHook((episodeId, kind) => {
    if (episodeId === "_sync" || !SYNCED_BLOBS.has(kind)) return;
    if (useStore.getState().settings.sync?.syncContent === false) return;
    scheduleAutoSync(4000);
  });
  useStore.subscribe((s, prev) => {
    if (prev.ready && !useSyncState.getState().busy && s.settings.showAliases !== prev.settings.showAliases) {
      scheduleAutoSync(2500);
    }
  });
  window.setTimeout(() => autoSyncIfDue(0), 2500);
  window.setInterval(() => autoSyncIfDue(15 * 60_000), 15 * 60_000);
  window.addEventListener("focus", () => autoSyncIfDue(2 * 60_000));
}
