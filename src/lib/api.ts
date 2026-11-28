import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { tr } from "./i18n";
import type { Aligned, TimeMap } from "./timemap";

/** A match inside one show: which part of the episode matched, a few words around it, and for subtitles the time. */
/** One day-hour of listening to one episode (from the player; seeks don't count). */
export interface ListenRow { day: string; hour: number; episodeId: string; showId: string; seconds: number; translated: boolean; showTitle: string; kind: string; cover: string; episodeTitle: string; video: boolean; duration: number; progress: number; published: number }

export interface CliStatus {
  bundled: boolean;
  bundledPath: string | null;
  installed: boolean;
  linkPath: string | null;
  targetPath: string | null;
  needsUpdate: boolean;
  /** A new terminal finds the command. */
  inPath: boolean;
  /** Where SubCast just put the command's folder on the PATH (a shell startup file, or "PATH" on Windows). */
  pathAdded?: string | null;
}

/** An AI agent on this computer and whether it can call SubCast (its MCP server). */
export interface McpTarget { id: string; name: string; icon: string; connected: boolean; outdated: boolean; config: string }
export interface ICloudStatus { available: boolean; path: string; defaultPath: string; hasFile: boolean; note?: string | null }
export interface BackendSyncSummary { backend: string; syncedAt: number; addedShows: number; removedShows: number; updatedEpisodes: number; updatedContent: number; uploaded: boolean; error?: string | null }
export interface SyncStatus { icloud: ICloudStatus; lastIcloud?: BackendSyncSummary | null; lastWebdav?: BackendSyncSummary | null }
export interface SyncReport { results: BackendSyncSummary[]; showsChanged: boolean; settingsChanged: boolean; blobsChanged: boolean }

export interface ShowHit { episode: Episode; field: "title" | "notes" | "transcript"; snippet: string; time?: number | null }

export interface Show {
  id: string;
  kind: "youtube" | "podcast" | "xiaoyuzhou" | "single" | "qiaomu";
  title: string;
  author?: string;
  cover?: string;
  feedUrl: string;
  link?: string;
  description?: string;
  updatedAt: number;
  unplayed: number;
}

/** One line of the network diagnosis (src-tauri network_check). */
export interface NetCheck { id: string; ok: boolean; ms?: number | null; detail: string }

export interface Episode {
  id: string;
  showId: string;
  guid: string;
  title: string;
  published?: number;
  duration?: number;
  audioUrl?: string;
  videoId?: string;
  link?: string;
  cover?: string;
  notes?: string;
  transcriptUrl?: string;
  transcriptType?: string;
  progress: number;
  hasTranscript: boolean;
  showTitle?: string;
  /** Has a 乔木精读 article. */
  deep?: boolean;
  /** User/ended override: "done" | "unseen" | null (auto from progress). */
  watchedOverride?: "done" | "unseen" | null;
}

let blobMutatedHook: ((episodeId: string, kind: string) => void) | null = null;
export function setBlobMutatedHook(fn: (episodeId: string, kind: string) => void) {
  blobMutatedHook = fn;
}

export interface Segment { start: number; end: number; text: string }
export interface Transcript { episodeId: string; source: string; language: string; segments: Segment[] }
export interface SearchResult { title: string; author: string; cover?: string; feedUrl: string; genre?: string; episodes?: number; /** "youtube" for a YouTube channel; absent for an Apple Podcasts show. */ source?: "youtube" }
export interface ModelInfo { id: string; name: string; sizeMb: number; note: string; installed: boolean; /** Another app's copy, used in place (no download). */ from?: string | null }
export interface Provider { id: string; name: string; protocol: "openai" | "anthropic" | "cli"; baseUrl: string; model: string; needsKey: boolean; keyUrl?: string; models?: string[]; group?: "relay" | "provider" | "local" | "cli" }
export interface CloudAsr { id: string; name: string; note: string; protocol: "doubao" | "openai" | "deepseek"; baseUrl: string; model: string; models: string[]; keyUrl?: string; needsKey: boolean; chunkSecs?: number }
export interface LocalTtsStatus { supported: boolean; runtime: boolean; models: { id: string; installed: boolean; bytes: number }[] }
export interface TtsRequest {
  engine: string;
  protocol: "edge" | "local" | "system" | "doubao" | "mimo" | "openai" | "qwen" | "minimax" | "elevenlabs" | "deepseek";
  baseUrl?: string;
  model?: string;
  voice: string;
  resource?: string;
  text: string;
  rate?: number;
  needsKey?: boolean;
}
export interface TtsResult { path: string; seconds: number }
export interface SystemVoice { id: string; name: string; lang: string }
export interface ChatMessage { role: "user" | "assistant"; content: string; /** Which model wrote an answer, for display. */ model?: string }

/** An episode saved for offline listening. */
export interface Saved { episodeId: string; path: string; bytes: number; savedAt: number }

export interface AsrEvent { episodeId: string; stage: "download" | "transcribe" | "segment" | "done" | "error"; progress: number; message?: string; segment?: Segment }
export interface FfmpegStatus { installed: boolean; path?: string | null; hardwareH264: boolean }
export interface PreparedClip { path: string; duration: number; /** Seconds before the clip in the file (stream copy from a keyframe). */ lead: number; width: number; height: number; hasBurnedSubtitles: boolean; cached: boolean; frames: string[]; bestFrame: number; /** Pauses (clip time), where "remove pauses" cuts. */ silences?: [number, number][]; /** Where the speaker sits over time: (clip time, 0–1 across). */ subjectTrack?: [number, number][] }
export interface CueOverlay { start: number; end: number; y: number; pngBase64: string }
export interface RenderVideoClipRequest {
  rawPath: string;
  outputPath?: string | null;
  defaultName: string;
  /** "frame" (with `frame`), or an older fixed layout. */
  layout: "frame" | "landscape" | "portrait_card" | "portrait_fill";
  /** The export folder from Settings (empty: Downloads). */
  outputDir?: string | null;
  /** Seconds to skip at the start of the fetched file. */
  lead?: number;
  /** Playback speed of the export (cue times already on that clock). */
  speed?: number;
  frame?: { w: number; h: number; fit: "card" | "fill"; vx: number; vy: number; vw: number; vh: number; cropBox: boolean };
  cardBg: "blur" | "night" | "paper";
  cropX: number;
  duration: number;
  staticOverlayPng?: string | null;
  cues: CueOverlay[];
  coverPng?: string | null;
  coverMode: "flash" | "hold" | "none";
  embedThumbnail: boolean;
  saveCoverSidecar: boolean;
  landscapeCoverPng?: string | null;
  /** Stretches to keep (clip time, before speed); cue times already on the cut clock. */
  keepRanges?: [number, number][];
  /** Even the sound to −14 LUFS. */
  loudness?: boolean;
  /** Smart framing: the crop follows the speaker along these (clip time, 0–1) points. */
  pan?: [number, number][];
}
export interface RenderVideoClipResult { videoPath: string; coverPath?: string | null; landscapeCoverPath?: string | null }
export interface VideoClipProgress { jobId: string; stage: string; progress: number; message: string }

export const api = {
  addSubscription: (input: string) => invoke<{ show?: Show; episode?: Episode }>("add_subscription", { input }),
  refreshShow: (id: string) => invoke<Show>("refresh_show", { id }),
  setMenuLabels: (labels: string[]) => invoke<void>("set_menu_labels", { labels }),
  refreshAll: () => invoke<{ shows: number; newEpisodes: number; skipped: boolean }>("refresh_all"),
  listShows: () => invoke<Show[]>("list_shows"),
  listEpisodes: (showId?: string, limit?: number) => invoke<Episode[]>("list_episodes", { showId, limit }),
  recentEpisodes: () => invoke<Episode[]>("recent_episodes"),
  getEpisode: (id: string) => invoke<Episode | null>("get_episode", { id }),
  removeShow: (id: string, keepEpisodeId?: string) => invoke<void>("remove_show", { id, keepEpisodeId }),
  setProgress: (id: string, seconds: number) => invoke<void>("set_progress", { id, seconds }),
  setWatched: (id: string, watched: "done" | "unseen" | null) => invoke<void>("set_watched", { id, watched }),
  markShowWatched: (showId: string) => invoke<number>("mark_show_watched", { showId }),
  searchShow: (showId: string, query: string) => invoke<ShowHit[]>("search_show", { showId, query }),
  searchPodcasts: (term: string, country?: string) => invoke<SearchResult[]>("search_podcasts", { term, country }),
  youtubeChannelFeed: (videoId: string) => invoke<string>("youtube_channel_feed", { videoId }),
  loadTranscript: (episodeId: string) => invoke<Transcript | null>("load_transcript", { episodeId }),
  fetchOfficialTranscript: (episodeId: string) => invoke<Transcript | null>("fetch_official_transcript", { episodeId }),
  transcribe: (episodeId: string, model: string, language: string, cloud?: CloudAsr) => invoke<Transcript>("transcribe", { episodeId, model, language, cloud: cloud ?? null }),
  cancelJob: (id: string) => invoke<void>("cancel_job", { id }),
  asrModels: () => invoke<ModelInfo[]>("asr_models"),
  logListen: (episodeId: string, showId: string, seconds: number, translated: boolean) => invoke<void>("log_listen", { episodeId, showId, seconds, translated }),
  listeningStats: (since: string) => invoke<ListenRow[]>("listening_stats", { since }),
  sharedTranscript: (episodeId: string) => invoke<Transcript | null>("shared_transcript", { episodeId }),
  /** The ids (of these) the shared library has subtitles for. */
  sharedExists: (episodeIds: string[]) => invoke<string[]>("shared_exists", { episodeIds }),
  /** A 导读 someone shared (saved as this episode's own when found). */
  sharedGuide: (episodeId: string) => invoke<string | null>("shared_guide", { episodeId }),
  shareGuide: (episodeId: string, text: string, model: string) => invoke<string>("share_guide", { episodeId, text, model }),
  asrElsewhere: () => invoke<{ app: string; format: string }[]>("asr_elsewhere"),
  rescanModels: () => invoke<void>("rescan_models"),
  downloadModel: (id: string, mirrorFirst: boolean) => invoke<void>("download_model", { id, mirrorFirst }),
  deleteModel: (id: string) => invoke<void>("delete_model", { id }),
  getBlob: (episodeId: string, kind: string) => invoke<string | null>("get_blob", { episodeId, kind }),
  setBlob: async (episodeId: string, kind: string, data: string) => {
    await invoke<void>("set_blob", { episodeId, kind, data });
    blobMutatedHook?.(episodeId, kind);
  },
  getSettings: () => invoke<unknown>("get_settings"),
  setSettings: (settings: unknown) => invoke<void>("set_settings", { settings }),
  setApiKey: (provider: string, key: string) => invoke<void>("set_api_key", { provider, key }),
  hasApiKey: (provider: string) => invoke<boolean>("has_api_key", { provider }),
  /** Free Google translation with a persistent cache (no model needed). */
  previewShow: (input: string) => invoke<{ show: Show; episodes: Episode[]; fetchedAt: number }>("preview_show", { input }),
  saveImage: (name: string, base64: string) => invoke<string>("save_image", { name, base64 }),
  /** The local file with one sentence of an episode in the speaker's voice (made once, then reused). */
  learnClip: (episodeId: string, start: number, end: number) => invoke<string>("learn_clip", { episodeId, start, end }),
  saveText: (path: string, text: string) => invoke<void>("save_text", { path, text }),
  /** Writes an Anki package; resolves to how many notes carry their sentence audio. */
  exportAnki: (path: string, deck: string, model: string, notes: AnkiNote[]) => invoke<number>("export_anki", { path, deck, model, notes }),
  cachedShow: (input: string) => invoke<{ show: Show; episodes: Episode[]; fetchedAt: number } | null>("cached_show", { input }),
  qiaomuDetail: (episodeId: string) => invoke<{ audio?: string; deepRead?: string }>("qiaomu_detail", { episodeId }),
  listModels: (provider: Provider) => invoke<string[]>("list_models", { provider }),
  ttsSpeak: (text: string, voice: string, resource?: string) => invoke<string>("tts_speak", { text, voice, resource }),
  ttsSynthesize: (req: TtsRequest) => invoke<TtsResult>("tts_synthesize", { req }),
  ttsSystemVoices: () => invoke<SystemVoice[]>("tts_system_voices"),
  clearTtsCache: () => invoke<void>("clear_tts_cache"),
  localTtsStatus: () => invoke<LocalTtsStatus>("local_tts_status"),
  localTtsInstall: (id: string, mirrorFirst: boolean) => invoke<void>("local_tts_install", { id, mirrorFirst }),
  localTtsDelete: (id: string) => invoke<void>("local_tts_delete", { id }),
  networkCheck: () => invoke<{ checks: NetCheck[]; webviewProxy: string }>("network_check"),
  youtubeStream: (videoId: string, mode: "native-hls" | "proxy-hls" | "mp4", fresh = false) => invoke<{ url: string; kind: "hls" | "mp4"; duration?: number | null }>("youtube_stream", { videoId, mode, fresh }),
  uiReady: () => invoke<{ shell: string; build: number | null }>("ui_ready"),
  shareTranslation: (episodeId: string, engine: string, model: string) => invoke<string>("share_translation", { episodeId, engine, model }),
  exportOpml: () => invoke<string>("export_opml"),
  uiCheck: () => invoke<string>("ui_check"),
  refreshConfig: () => invoke<void>("refresh_config"),
  prepareYoutube: () => invoke<void>("prepare_youtube"),
  /** Opens a SubCast window to sign in to YouTube; `youtube-signed-in` fires when it is done. */
  youtubeSignIn: () => invoke<void>("youtube_sign_in"),
  youtubeSignedIn: () => invoke<boolean>("youtube_signed_in"),
  youtubeSignOut: () => invoke<void>("youtube_sign_out"),
  openPrivacySettings: () => invoke<void>("open_privacy_settings"),
  shadowStart: () => invoke<void>("shadow_start"),
  shadowStop: () => invoke<void>("shadow_stop"),
  shadowClear: () => invoke<void>("shadow_clear"),
  shadowReady: (model: string) => invoke<boolean>("shadow_ready", { model }),
  shadowHear: (model: string, language: string) => invoke<string>("shadow_hear", { model, language }),
  openMicSettings: () => invoke<void>("open_mic_settings"),
  youtubeTracks: (episodeId: string) => invoke<{ language: string; auto: boolean }[]>("youtube_tracks", { episodeId }),
  useYoutubeTrack: (episodeId: string, language: string, auto: boolean) => invoke<Transcript>("use_youtube_track", { episodeId, language, auto }),
  translateTexts: (texts: string[], target = "zh-CN") => invoke<string[]>("translate_texts", { texts, target }),
  downloadEpisode: (episodeId: string) => invoke<Saved>("download_episode", { episodeId }),
  listDownloads: () => invoke<Saved[]>("list_downloads"),
  deleteDownload: (episodeId: string) => invoke<void>("delete_download", { episodeId }),
  clearDownloads: () => invoke<void>("clear_downloads"),
  storageInfo: () => invoke<{ downloads: number; models: number; library: number; temp: number; audioCache: number; ttsCache?: number; ttsModels?: number }>("storage_info"),
  alignAudio: (episodeId: string, force: boolean, model?: string) => invoke<Aligned>("align_audio", { episodeId, force, model }),
  saveTimeMap: (episodeId: string, map: TimeMap) => invoke<void>("save_time_map", { episodeId, map }),
  listAudioCache: () => invoke<Saved[]>("list_audio_cache"),
  /** A loopback address that fetches an episode's audio through SubCast (app 0.3.8+; older apps reject it). */
  audioStream: (url: string) => invoke<string>("audio_stream", { url }),
  clearAudioCache: () => invoke<void>("clear_audio_cache"),
  openDataFolder: (kind: "downloads" | "models" | "data" | "tts-cache" | "tts-models") => invoke<void>("open_data_folder", { kind }),
  revealDownload: (episodeId: string) => invoke<void>("reveal_download", { episodeId }),
  clearTemp: () => invoke<void>("clear_temp"),
  fullNotes: (episodeId: string) => invoke<string | null>("full_notes", { episodeId }),
  videoClipEnv: () => invoke<FfmpegStatus>("video_clip_env"),
  /** Resolves the video's download addresses in the background, so cutting a clip starts downloading at once. */
  videoClipWarm: (videoId: string, ranges?: [number, number][]) => invoke<void>("video_clip_warm", { videoId, ranges: ranges ?? null }),
  /** One stretch, or (with `ranges`) several cut together in order. */
  videoClipPrepare: (jobId: string, videoId: string, start: number, end: number, ranges?: [number, number][]) => invoke<PreparedClip>("video_clip_prepare", { jobId, videoId, start, end, ranges: ranges ?? null }),
  videoClipRender: (jobId: string, req: RenderVideoClipRequest) => invoke<RenderVideoClipResult>("video_clip_render", { jobId, req }),
  revealFile: (path: string) => invoke<void>("reveal_file", { path }),
  dbDataVersion: () => invoke<number>("db_data_version"),
  cliStatus: () => invoke<CliStatus>("cli_status"),
  cliInstall: () => invoke<CliStatus>("cli_install"),
  mcpTargets: () => invoke<McpTarget[]>("mcp_targets"),
  mcpConnect: (id: string) => invoke<McpTarget>("mcp_connect", { id }),
  mcpDisconnect: (id: string) => invoke<McpTarget>("mcp_disconnect", { id }),
  syncStatus: (customDir?: string) => invoke<SyncStatus>("sync_status", { customDir }),
  syncWebdavTest: (url: string, path: string, username: string, password?: string) => invoke<string>("sync_webdav_test", { url, path, username, password }),
  syncRun: (target?: "icloud" | "webdav") => invoke<SyncReport>("sync_run", { target }),
};

let llmCounter = 0;

/** Streams one model reply. `onDelta` receives the text so far. */
export async function llmChat(opts: {
  provider: Provider;
  system: string;
  messages: ChatMessage[];
  maxTokens?: number;
  jsonMode?: boolean;
  /** Needs no deep thinking: ask the provider to turn thinking off or down, so the budget goes to the answer. */
  quick?: boolean;
  onDelta?: (full: string) => void;
  signal?: AbortSignal;
}): Promise<string> {
  const requestId = `llm-${Date.now()}-${llmCounter++}`;
  let full = "";
  let unlisten: UnlistenFn | undefined;
  // Pieces arrive faster than the screen redraws: hand the text over at most every 40 ms. A timer, not a frame
  // callback, so it still runs with the window hidden; cancelled at the end, so a late one can't bring back
  // "streaming" after the answer is done.
  let pending = 0;
  if (opts.onDelta) {
    unlisten = await listen<{ requestId: string; delta: string }>("llm", (ev) => {
      if (ev.payload.requestId !== requestId) return;
      full += ev.payload.delta;
      if (!pending) pending = window.setTimeout(() => { pending = 0; opts.onDelta!(full); }, 40);
    });
  }
  const abort = () => void api.cancelJob(requestId);
  opts.signal?.addEventListener("abort", abort);
  try {
    const { id, protocol, baseUrl, model } = opts.provider;
    return await invoke<string>("llm_chat", {
      requestId,
      provider: { id, protocol, baseUrl, model },
      system: opts.system,
      messages: opts.messages,
      maxTokens: opts.maxTokens,
      jsonMode: opts.jsonMode,
      quick: opts.quick,
    });
  } finally {
    // The last pieces go out now, before the caller sees the result (it keeps them if the reply fails).
    if (pending) { window.clearTimeout(pending); opts.onDelta?.(full); }
    unlisten?.();
    opts.signal?.removeEventListener("abort", abort);
  }
}

/** The error as it came, for matching on its wording; show it with errorText (or tr). */
export const rawErrorText = (e: unknown) => (typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e));
/** YouTube's "confirm you're not a bot" check, as the backend words it (asr.rs with_bot_fallback). */
export interface AnkiNote { id: string; word: string; ipa: string; meaning: string; context: string; sentence: string; translation: string; source: string; tags: string; clip: string | null }

export const isBotCheck = (m: string) => m.includes("YouTube 要求验证「不是机器人」"); // i18n-ignore (matches the backend's text)
/** The browser keeps its cookies encrypted for itself (Chrome on Windows), so borrowing its sign-in can't work. */
export const browserLocked = (m: string) => m.includes("把登录信息加密了"); // i18n-ignore (matches the backend's text)
/** The sign-in made in SubCast stopped working. */
export const signInExpired = (m: string) => m.includes("在 SubCast 里登录的 YouTube 已失效"); // i18n-ignore (matches the backend's text)
export const errorText = (e: unknown) => tr(rawErrorText(e));
/** macOS refused SubCast access to a browser's data (import.rs NEEDS_ACCESS). */
export const needsBrowserAccess = (m: string) => m.includes("需要你允许 SubCast 读取"); // i18n-ignore (matches the backend's text)
