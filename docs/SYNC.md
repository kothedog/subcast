# Cross-Device Sync Protocol (`subcast-sync.json` v2)

Desktop SubCast and SubCast Mobile share subscriptions, custom show aliases, playback positions, watched overrides, and episode AI/reading content (`summary`, `deep-read`, `article`, `chat`, `chat-archive`, `highlights`, `notes-zh`, `favorites`, `notes`) using a two-tier layout:

```text
SubCast/
├── subcast-sync.json          # Index: subscriptions + playback progress + app blobs + contentIndex
└── episodes/
    └── <episode_id>.json      # Incremental per-episode AI/reading blobs (synced only when hash differs)
```

## Transports

### 1. iCloud Drive
- **Native container**: `iCloud.ai.qiaomu.subcast` (displayed as `SubCast` in iCloud Drive).
- **macOS Desktop path (default)**: `~/Library/Mobile Documents/iCloud~ai~qiaomu~subcast/Documents/subcast-sync.json` (or a user-selected folder inside iCloud Drive). Desktop waits for iOS/iCloud `bird` to materialize the container root before writing.
- **iOS Mobile**: Uses the native iCloud Documents container via `FileManager.url(forUbiquityContainerIdentifier: "iCloud.ai.qiaomu.subcast")` + `NSFileCoordinator` (zero manual folder selection), with Security-Scoped folder picker kept as fallback.
- **Evicted stub handling**: On macOS, if `.subcast-sync.json.icloud` (or `episodes/.<id>.json.icloud`) exists, SubCast invokes `/usr/bin/brctl download` and waits for CloudDocs to materialize it before reading.
- **Atomic writes**: Write to a temp file in the target directory, `fsync` (desktop) / `NSFileCoordinator` (iOS), and rename atomically.

### 2. WebDAV (Nutstore / Nextcloud / InfiniCLOUD / Self-hosted)
- **Target path**: Isolated folder configured by `webdavPath` (default `/SubCast`), storing `<base>/<webdavPath>/subcast-sync.json` and `<base>/<webdavPath>/episodes/<episode_id>.json`.
- **Authentication**: HTTP Basic Auth. Passwords are stored in the OS keychain (`ai.qiaomu.subcast`, account `sync.webdav`) on desktop & iOS, or private 0600 `keys.json` on Android.
- **Directory creation**: Walk each path segment with `MKCOL`; treat `200/201/204/301/302/405` as success and verify `409` via `PROPFIND` (`Depth: 0`).
- **Atomic writes**: `PUT <file>.tmp.<UUID>`, then `MOVE` with `Destination` + `Overwrite: T`; if `MOVE` is rejected by a reverse proxy, fall back to direct `PUT` and best-effort `DELETE` of `.tmp`.

---

## JSON Schema (`version: 2`, backward-compatible with `version: 1`)

### `subcast-sync.json`
```json
{
  "version": 2,
  "updatedAt": 1764299000000,
  "deviceId": "8f2b1c9d-...",
  "subscriptions": [
    {
      "feedUrl": "https://feeds.example.com/podcast.xml",
      "kind": "podcast",
      "title": "Example Podcast",
      "author": "Host Name",
      "cover": "https://cdn.example.com/cover.jpg",
      "link": "https://example.com",
      "alias": "示例播客",
      "updatedAt": 1764298000000,
      "deletedAt": null
    }
  ],
  "episodes": {
    "a1b2c3d4e5f607189012": {
      "progress": 642.5,
      "watched": null,
      "updatedAt": 1764298500000
    }
  },
  "favorites": { "data": "[{\"id\":\"a1b2c3d4e5f607189012\",\"at\":1764298500000}]", "updatedAt": 1764298500000 },
  "notes": { "data": "[...]", "updatedAt": 1764298500000 },
  "contentIndex": {
    "a1b2c3d4e5f607189012": {
      "updatedAt": 1764298600000,
      "hash": "0123456789abcdef",
      "kinds": ["chat", "deep-read", "summary"]
    }
  }
}
```

### `episodes/<episode_id>.json`
```json
{
  "episodeId": "a1b2c3d4e5f607189012",
  "blobs": {
    "summary": { "data": "...", "updatedAt": 1764298600000 },
    "summary-from": { "data": "", "updatedAt": 1764298600000 },
    "summary-model": { "data": "deepseek-flash", "updatedAt": 1764298600000 },
    "deep-read": { "data": "...", "updatedAt": 1764298600000 },
    "article": { "data": "...", "updatedAt": 1764298600000 },
    "chat": { "data": "[...]", "updatedAt": 1764298600000 },
    "chat-archive": { "data": "[...]", "updatedAt": 1764298600000 },
    "highlights": { "data": "{...}", "updatedAt": 1764298600000 },
    "notes-zh": { "data": "{...}", "updatedAt": 1764298600000 }
  }
}
```

### ID Compatibility
- `episodes` and `contentIndex` keys use the deterministic 20-char hex `episode.id` computed identically in desktop and mobile (`short_hash("{show_id}:{guid}", 20)` where `show_id = short_hash(feed_url, 16)`).
- `watched` values match `watched_override`: `null` (auto), `"done"`, or `"unseen"`.

---

## Conflict Resolution & Pruning
1. **Feed URL normalization**: Canonical key trims whitespace and strips trailing `/` when the URL has no query (`?`) or fragment (`#`).
2. **Subscriptions (LWW + Tombstones)**:
   - Effective timestamp = `max(updatedAt, deletedAt ?? 0)`. Higher wins; on tie, active beats deleted and missing metadata/alias fields are filled from the other side.
   - Tombstones older than **90 days** are pruned during merge.
3. **Episodes Progress (LWW)**:
   - Higher `updatedAt` wins; on tie, larger `progress` wins and `"done"` takes precedence. Capped at the 5,000 most recently updated episodes.
4. **Content Blobs (Incremental Two-Tier)**:
   - `contentIndex` maps `episodeId` to deterministic `hash = short_hash(sorted (kind, data) JSON, 16)`. Matching hashes are skipped with zero extra I/O.
   - `chat-archive` merges JSON arrays by `id` (sorted descending by `at`).
   - `chat` uses LWW (`updatedAt`), breaking ties by larger message array / string length.
   - All other per-episode blobs and `_app` `favorites`/`notes` use LWW (`updatedAt`), breaking ties toward non-empty/longer data.
5. **No-op skip**: `subcast-sync.json` upload is skipped when merged content matches the fetched remote snapshot.
