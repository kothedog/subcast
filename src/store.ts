import { create } from "zustand";
import { api, type Episode, type Show } from "./lib/api";
import { invalidateEpisodes } from "./lib/cache";
import { DEFAULTS, loadSettings, type Settings } from "./lib/settings";
import { setLang, setTranslateNames, t } from "./lib/i18n";
import type { Aligned, AlignStatus } from "./lib/timemap";

export type View =
  | { kind: "latest" }
  | { kind: "recent" }
  | { kind: "stats" }
  | { kind: "discover" }
  | { kind: "downloads" }
  | { kind: "show"; id: string }
  | { kind: "channel"; input: string }
  | { kind: "episode"; id: string }
  | { kind: "words" }
  | { kind: "settings" };

export interface PlayerHandle {
  /** To a subtitle time. */
  seek(seconds: number): void;
  /** To a time on the player's own clock (the progress bar), when it differs from subtitle time. */
  seekRaw?(seconds: number): void;
  toggle(): void;
  play(): void;
  setRate(rate: number): void;
  /** Fastest speed this player can do, when below the list's top. */
  maxRate?: number;
  /** The episode this player plays. A video's player can be ready before the episode becomes current. */
  episodeId?: string;
}

export type SettingsSection = "subtitles" | "ai" | "general" | "learn" | "about" | "model" | "asr" | "reading" | "tts" | "shortcuts" | "storage" | "agents" | "network" | "sync";

interface State {
  speaking: boolean;
  ready: boolean;
  /** The hot update of the interface in use (ota.rs), null when it is the one built into the app. */
  uiBuild?: number | null;
  settings: Settings;
  shows: Show[];
  view: View;
  history: View[];
  future: View[];
  /** The episode loaded in the player (may differ from the one being viewed). */
  current: Episode | null;
  time: number;
  /** The audio element's own clock; differs from `time` (subtitle time) when the subtitles were made from another
   *  cut of the episode (src/lib/timemap.ts). */
  rawTime?: number;
  /** How the current episode's subtitles line up with its audio. */
  align?: (Omit<Aligned, "status"> & { id: string; status: AlignStatus | "working" }) | null;
  /** Playing something only this audio file has (an ad, most likely) until this time on its clock. */
  gapEnd?: number | null;
  length: number;
  playing: boolean;
  player: PlayerHandle | null;
  /** Since when playback has been asked for but no sound has come yet (connecting, buffering); null when not waiting. */
  loadingSince?: number | null;
  /** Play as soon as the player for `current` is ready (it may take a few seconds to appear). */
  playWhenReady?: string | null;
  /** Seek here as soon as this episode's player appears. */
  seekWhenReady?: { id: string; at: number } | null;
  /** First run: show the welcome card; shows are being subscribed in the background. */
  welcome?: boolean;
  settingUp?: boolean;
  /** The video fills the window (and the window the screen), subtitles on it. */
  theater?: boolean;
  /** Which tab the add dialog opens on. */
  addTab?: "search" | "link" | "import";
  toast: string | null;
  toastAction: { label: string; run: () => void } | null;
  /** The settings page is showing (kept in step with `view`, for panels that recheck keys when it closes). */
  settingsOpen: boolean;
  settingsSection?: SettingsSection;
  /** Bumped by each openSettings call, so a page already open still jumps to the asked section. */
  settingsNonce?: number;
  addOpen: boolean;
  /** The bulk "manage subscriptions" dialog is open. */
  manageOpen: boolean;
  init(): Promise<void>;
  go(view: View): void;
  back(): void;
  forward(): void;
  reloadShows(): Promise<void>;
  /** `lazy`: for text being typed. The change shows at once; the disk write waits until typing pauses. */
  saveSettings(patch: Partial<Settings>, lazy?: boolean): Promise<void>;
  openEpisode(ep: Episode): void;
  /** Loads an episode into the player without navigating. */
  load(ep: Episode): void;
  /** A short message at the bottom; `action` adds a button (kept on screen longer). */
  notify(text: string, action?: { label: string; run: () => void }): void;
  openSettings(section?: SettingsSection): void;
  /** Checks every subscription for new episodes; tells the user when there are some. */
  checkUpdates(): Promise<void>;
  lastChecked: number;
  set(patch: Partial<State>): void;
}

const CHECK_EVERY = 30 * 60_000;

let toastTimer: number | undefined;
let lastDbVersion = 0;

function sameView(a: View, b: View): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind === "show" && b.kind === "show") return a.id === b.id;
  if (a.kind === "channel" && b.kind === "channel") return a.input === b.input;
  if (a.kind === "episode" && b.kind === "episode") return a.id === b.id;
  return true;
}

export const useStore = create<State>((set, get) => ({
  ready: false,
  speaking: false,
  settings: DEFAULTS,
  shows: [],
  view: { kind: "latest" },
  history: [],
  future: [],
  current: null,
  time: 0,
  length: 0,
  playing: false,
  player: null,
  toast: null,
  toastAction: null,
  settingsOpen: false,
  addOpen: false,
  manageOpen: false,
  async init() {
    const [settings, shows] = await Promise.all([loadSettings(), api.listShows()]);
    setLang(settings.uiLang);
    setTranslateNames(settings.translateShowNames);
    void api.setMenuLabels([t("设置…")]).catch(() => {});
    set({ settings, shows, ready: true });
    void api.dbDataVersion().then((v) => { lastDbVersion = v; }).catch(() => {});
    void get().checkUpdates();
    // Periodically, and when the window comes back after a while (the app may sit in the background for hours).
    window.setInterval(() => void get().checkUpdates(), CHECK_EVERY);
    window.addEventListener("focus", () => {
      if (Date.now() - get().lastChecked > 10 * 60_000) void get().checkUpdates();
      void api.dbDataVersion().then(async (v) => {
        if (!v || v === lastDbVersion) return;
        lastDbVersion = v;
        invalidateEpisodes();
        const freshSettings = await loadSettings();
        setLang(freshSettings.uiLang);
        setTranslateNames(freshSettings.translateShowNames);
        set({ settings: freshSettings });
        await get().reloadShows();
      }).catch(() => {});
    });
  },
  lastChecked: 0,
  async checkUpdates() {
    if (!get().shows.some((s) => s.kind !== "single")) return;
    set({ lastChecked: Date.now() });
    try {
      const r = await api.refreshAll();
      if (r.skipped) return;
      await get().reloadShows();
      if (r.newEpisodes > 0) get().notify(t("订阅有 {n} 期新节目", { n: r.newEpisodes }));
    } catch { /* offline: try again next time */ }
  },
  go(view) {
    const cur = get().view;
    // Opening settings again while on it only switches the section (see openSettings), no new history entry.
    if (view.kind === "settings" && cur.kind === "settings") return;
    if (sameView(cur, view)) return;
    set((s) => ({ view, history: [...s.history.slice(-30), s.view], future: [], settingsOpen: view.kind === "settings" }));
  },
  back() {
    const h = get().history;
    if (!h.length) return;
    const view = h[h.length - 1];
    set((s) => ({ view, history: h.slice(0, -1), future: [s.view, ...s.future].slice(0, 30), settingsOpen: view.kind === "settings" }));
  },
  forward() {
    const f = get().future;
    if (!f.length) return;
    const view = f[0];
    set((s) => ({ view, history: [...s.history.slice(-30), s.view], future: f.slice(1), settingsOpen: view.kind === "settings" }));
  },
  async reloadShows() {
    // Unchanged after a refresh (the usual case): keep the same array, so pages that depend on it don't reload.
    const shows = await api.listShows();
    if (JSON.stringify(shows) !== JSON.stringify(get().shows)) {
      invalidateEpisodes();
      set({ shows });
    }
  },
  async saveSettings(patch, lazy) {
    const settings = { ...get().settings, ...patch };
    if (patch.uiLang) { setLang(patch.uiLang); void api.setMenuLabels([t("设置…")]).catch(() => {}); }
    if (patch.translateShowNames !== undefined) setTranslateNames(patch.translateShowNames);
    set({ settings });
    window.clearTimeout(settingsTimer);
    settingsTimer = 0;
    if (lazy) { settingsTimer = window.setTimeout(() => { settingsTimer = 0; void api.setSettings(get().settings).then(() => api.dbDataVersion()).then((v) => { lastDbVersion = v; }).catch(() => {}); }, 400); return; }
    await api.setSettings(settings);
    void api.dbDataVersion().then((v) => { lastDbVersion = v; }).catch(() => {});
  },
  openEpisode(ep) {
    // The episode page decides whether to load it into the player (it leaves a playing episode alone).
    get().go({ kind: "episode", id: ep.id });
  },
  load(ep) {
    // Keep a player that already belongs to this episode (a video ready before "play" was pressed).
    if (get().current?.id !== ep.id) { const p = get().player; set({ current: ep, time: ep.progress || 0, rawTime: undefined, align: null, gapEnd: null, length: ep.duration || 0, playing: false, player: p?.episodeId === ep.id ? p : standby.get(ep.id) ?? null }); }
  },
  notify(text, action) {
    set({ toast: text, toastAction: action ?? null });
    window.clearTimeout(toastTimer);
    toastTimer = window.setTimeout(() => set({ toast: null, toastAction: null }), action ? 8000 : 4200);
  },
  openSettings(section) {
    set((s) => ({ settingsSection: section, settingsNonce: (s.settingsNonce ?? 0) + 1 }));
    get().go({ kind: "settings" });
  },
  set: (patch) => set(patch),
}));

/** Players that exist for an episode that isn't the current one (a video page opened while something else plays).
 *  Only the current episode's player drives the global controls; another becomes current when it is loaded. */
const standby = new Map<string, PlayerHandle>();

/** A player is ready for its episode: it takes the controls if that episode is current, else waits here. */
export function offerPlayer(h: PlayerHandle & { episodeId: string }, extra?: Partial<State>) {
  standby.set(h.episodeId, h);
  const s = useStore.getState();
  if (s.current?.id === h.episodeId) s.set({ player: h, ...extra });
}

/** A player is going away: it stops being offered, and leaves the controls if it had them. */
export function withdrawPlayer(h: PlayerHandle & { episodeId: string }) {
  if (standby.get(h.episodeId) === h) standby.delete(h.episodeId);
  const s = useStore.getState();
  if (s.player === h) s.set({ player: null, playing: false });
}

/** Seeks once the current episode's player exists (a video's player takes a moment to appear). */
export function seekSoon(seconds: number) {
  const s = useStore.getState();
  if (s.player) { s.player.seek(seconds); return; }
  if (s.current) s.set({ time: seconds, seekWhenReady: { id: s.current.id, at: seconds } });
}

let settingsTimer = 0;
// A typed setting still waiting is written as soon as the window loses focus or hides (⌘Q may not give
// the page a chance to unload).
const flushSettings = () => { if (settingsTimer) { window.clearTimeout(settingsTimer); settingsTimer = 0; void api.setSettings(useStore.getState().settings); } };
window.addEventListener("blur", flushSettings);
document.addEventListener("visibilitychange", flushSettings);
window.addEventListener("beforeunload", flushSettings);

export function providerFor(settings: Settings, use: "chat" | "translate") {
  if (use === "translate") return settings.providers[settings.translateProvider];
  const p = settings.providers[settings.chatProvider];
  return p && settings.chatModel ? { ...p, model: settings.chatModel } : p;
}
