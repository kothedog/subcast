use crate::model::*;
use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

pub fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
        -- In WAL mode NORMAL is still crash-safe, and commits stop waiting on a disk flush each time.
        PRAGMA synchronous=NORMAL;
        PRAGMA busy_timeout=5000;
        PRAGMA temp_store=MEMORY;
        PRAGMA cache_size=-16000;
        PRAGMA mmap_size=67108864;
        CREATE TABLE IF NOT EXISTS shows(
            id TEXT PRIMARY KEY, kind TEXT NOT NULL, title TEXT NOT NULL, author TEXT, cover TEXT,
            feed_url TEXT NOT NULL, link TEXT, description TEXT, updated_at INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS episodes(
            id TEXT PRIMARY KEY, show_id TEXT NOT NULL, guid TEXT NOT NULL, title TEXT NOT NULL,
            published INTEGER, duration REAL, audio_url TEXT, video_id TEXT, link TEXT, cover TEXT, notes TEXT,
            transcript_url TEXT, transcript_type TEXT, progress REAL NOT NULL DEFAULT 0);
        CREATE INDEX IF NOT EXISTS episodes_show ON episodes(show_id, published DESC);
        CREATE INDEX IF NOT EXISTS episodes_published ON episodes(published DESC);
        CREATE INDEX IF NOT EXISTS episodes_show_progress ON episodes(show_id, progress);
        CREATE INDEX IF NOT EXISTS episodes_progress ON episodes(progress) WHERE progress > 0;
        CREATE TABLE IF NOT EXISTS transcripts(
            episode_id TEXT PRIMARY KEY, source TEXT NOT NULL, language TEXT NOT NULL, segments TEXT NOT NULL, created_at INTEGER NOT NULL);
        CREATE TABLE IF NOT EXISTS blobs(
            episode_id TEXT NOT NULL, kind TEXT NOT NULL, data TEXT NOT NULL, PRIMARY KEY(episode_id, kind));
        CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS tr_cache(key TEXT PRIMARY KEY, value TEXT NOT NULL);
        -- Time actually spent listening (playing, not seeking), per local day and hour, for 我的收听.
        CREATE TABLE IF NOT EXISTS listens(day TEXT NOT NULL, hour INTEGER NOT NULL, episode_id TEXT NOT NULL, show_id TEXT NOT NULL,
            seconds REAL NOT NULL, translated INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(day, hour, episode_id));
        CREATE TABLE IF NOT EXISTS sync_subs(
            feed_url TEXT PRIMARY KEY,
            deleted_at INTEGER,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sync_episodes(
            episode_id TEXT PRIMARY KEY,
            progress REAL NOT NULL DEFAULT 0,
            watched_override TEXT,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS sync_blobs(
            episode_id TEXT NOT NULL,
            kind TEXT NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(episode_id, kind)
        );",
    )?;
    let has_deep: bool = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('episodes') WHERE name='deep'", [], |r| r.get::<_, i64>(0))? > 0;
    if !has_deep {
        conn.execute_batch("ALTER TABLE episodes ADD COLUMN deep INTEGER NOT NULL DEFAULT 0; UPDATE episodes SET deep=1 WHERE transcript_type='qiaomu';")?;
    }
    let has_watched: bool = conn.query_row("SELECT COUNT(*) FROM pragma_table_info('episodes') WHERE name='watched_override'", [], |r| r.get::<_, i64>(0))? > 0;
    if !has_watched {
        conn.execute_batch("ALTER TABLE episodes ADD COLUMN watched_override TEXT;")?;
    }
    Ok(conn)
}

pub fn upsert_show(conn: &Connection, s: &Show) -> Result<()> {
    conn.execute(
        "INSERT INTO shows(id,kind,title,author,cover,feed_url,link,description,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(id) DO UPDATE SET title=excluded.title, author=excluded.author, cover=COALESCE(excluded.cover, shows.cover),
         link=excluded.link, description=excluded.description, updated_at=excluded.updated_at",
        params![s.id, s.kind, s.title, s.author, s.cover, s.feed_url, s.link, s.description, s.updated_at],
    )?;
    if s.kind != "single" && !s.feed_url.trim().is_empty() {
        let now = now_ms();
        conn.execute(
            "INSERT INTO sync_subs(feed_url, deleted_at, updated_at) VALUES(?1, NULL, ?2)
             ON CONFLICT(feed_url) DO UPDATE SET deleted_at = NULL, updated_at = excluded.updated_at
             WHERE sync_subs.deleted_at IS NOT NULL",
            params![s.feed_url.trim(), now],
        )?;
    }
    Ok(())
}

pub fn upsert_episodes(conn: &mut Connection, eps: &[Episode]) -> Result<()> {
    let tx = conn.transaction()?;
    {
        let mut st = tx.prepare(
            "INSERT INTO episodes(id,show_id,guid,title,published,duration,audio_url,video_id,link,cover,notes,transcript_url,transcript_type,deep)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
             ON CONFLICT(id) DO UPDATE SET title=excluded.title, published=excluded.published, duration=COALESCE(excluded.duration, episodes.duration),
             audio_url=COALESCE(excluded.audio_url, episodes.audio_url), video_id=COALESCE(excluded.video_id, episodes.video_id),
             link=excluded.link, cover=COALESCE(excluded.cover, episodes.cover),
             notes=COALESCE(excluded.notes, episodes.notes), transcript_url=excluded.transcript_url, transcript_type=excluded.transcript_type, deep=excluded.deep",
        )?;
        for e in eps {
            st.execute(params![
                e.id, e.show_id, e.guid, e.title, e.published, e.duration, e.audio_url, e.video_id, e.link, e.cover, e.notes,
                e.transcript_url, e.transcript_type, e.deep
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn show_from(r: &rusqlite::Row) -> rusqlite::Result<Show> {
    Ok(Show {
        id: r.get(0)?, kind: r.get(1)?, title: r.get(2)?, author: r.get(3)?, cover: r.get(4)?, feed_url: r.get(5)?,
        link: r.get(6)?, description: r.get(7)?, updated_at: r.get(8)?, unplayed: r.get(9).unwrap_or(0),
    })
}

/// Shared SQL predicate for "watched" (mirrors `src/lib/watched.ts` exactly).
/// Uses COALESCE so the expression is strictly boolean (never SQL NULL under `NOT`).
const IS_WATCHED_SQL: &str = "(COALESCE(e.watched_override, '') = 'done' OR (e.watched_override IS NULL AND COALESCE(e.duration, 0) > 0 AND (e.progress >= e.duration * 0.95 OR (e.duration > 300.0 AND (e.duration - e.progress) <= 90.0))))";

pub fn list_shows(conn: &Connection) -> Result<Vec<Show>> {
    let sql = format!(
        "SELECT s.id,s.kind,s.title,s.author,s.cover,s.feed_url,s.link,s.description,s.updated_at,
         (SELECT COUNT(*) FROM episodes e WHERE e.show_id=s.id AND NOT {IS_WATCHED_SQL}) FROM shows s WHERE s.kind != 'single' ORDER BY s.title COLLATE NOCASE"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map([], show_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_show(conn: &Connection, id: &str) -> Result<Option<Show>> {
    Ok(conn
        .query_row("SELECT id,kind,title,author,cover,feed_url,link,description,updated_at,0 FROM shows WHERE id=?1", [id], show_from)
        .optional()?)
}

pub fn delete_show(conn: &Connection, id: &str, keep_episode_id: Option<&str>) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let meta: Option<(String, String)> = tx
        .query_row("SELECT kind, feed_url FROM shows WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    if keep_episode_id.is_some() {
        if let Some(show) = get_show(&tx, id)? {
            set_blob(&tx, id, "show-json", &serde_json::to_string(&show)?)?;
        }
    }
    tx.execute(
        "DELETE FROM episodes WHERE show_id=?1 AND id NOT IN (SELECT episode_id FROM transcripts)
         AND (?2 IS NULL OR (id != ?2 AND progress = 0))",
        params![id, keep_episode_id]
    )?;
    tx.execute("DELETE FROM shows WHERE id=?1", [id])?;
    if let Some((kind, feed_url)) = meta {
        if kind != "single" && !feed_url.trim().is_empty() {
            let now = now_ms();
            tx.execute(
                "INSERT OR REPLACE INTO sync_subs(feed_url, deleted_at, updated_at) VALUES(?1, ?2, ?2)",
                params![feed_url.trim(), now],
            )?;
        }
    }
    tx.commit()?;
    Ok(())
}

const EPISODE_COLS: &str = "e.id,e.show_id,e.guid,e.title,e.published,e.duration,e.audio_url,e.video_id,e.link,e.cover,e.notes,e.transcript_url,e.transcript_type,e.progress,
    EXISTS(SELECT 1 FROM transcripts t WHERE t.episode_id=e.id),
    CASE WHEN s.kind='single' THEN NULL ELSE COALESCE(s.title, (SELECT json_extract(b.data, '$.title') FROM blobs b WHERE b.episode_id=e.show_id AND b.kind='show-json')) END,
    e.deep, e.watched_override";

/// The same columns without show notes, which are large and lists don't show.
fn list_cols() -> String { EPISODE_COLS.replacen("e.notes", "NULL", 1) }

/// Every show note of a show, by episode (for searching inside it).
pub fn show_notes(conn: &Connection, show_id: &str) -> Result<std::collections::HashMap<String, String>> {
    let mut st = conn.prepare("SELECT id, notes FROM episodes WHERE show_id=?1 AND notes IS NOT NULL")?;
    let rows = st.query_map([show_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn episode_from(r: &rusqlite::Row) -> rusqlite::Result<Episode> {
    Ok(Episode {
        id: r.get(0)?, show_id: r.get(1)?, guid: r.get(2)?, title: r.get(3)?, published: r.get(4)?, duration: r.get(5)?,
        audio_url: r.get(6)?, video_id: r.get(7)?, link: r.get(8)?, cover: r.get(9)?, notes: r.get(10)?,
        transcript_url: r.get(11)?, transcript_type: r.get(12)?, progress: r.get(13)?, has_transcript: r.get(14)?, show_title: r.get(15)?,
        deep: r.get(16)?, watched_override: r.get(17)?,
    })
}

pub fn list_episodes(conn: &Connection, show_id: Option<&str>, limit: i64) -> Result<Vec<Episode>> {
    // Notes are large; the list does not need them.
    let sql = format!(
        "SELECT {} FROM episodes e LEFT JOIN shows s ON s.id=e.show_id {} ORDER BY e.published DESC LIMIT ?1",
        list_cols(),
        if show_id.is_some() { "WHERE e.show_id=?2" } else { "WHERE s.kind != 'single'" }
    );
    let mut st = conn.prepare(&sql)?;
    let map = |r: &rusqlite::Row| episode_from(r).map(|mut e| { e.notes = None; e });
    let rows = match show_id {
        Some(id) => st.query_map(params![limit, id], map)?.collect::<rusqlite::Result<Vec<_>>>()?,
        None => st.query_map(params![limit], map)?.collect::<rusqlite::Result<Vec<_>>>()?,
    };
    Ok(rows)
}

pub fn recent_episodes(conn: &Connection, limit: i64) -> Result<Vec<Episode>> {
    let sql = format!(
        "SELECT {} FROM episodes e LEFT JOIN shows s ON s.id=e.show_id
         WHERE e.progress > 0 OR EXISTS(SELECT 1 FROM transcripts t WHERE t.episode_id=e.id) OR s.kind='single'
         ORDER BY e.rowid DESC LIMIT ?1",
        list_cols()
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params![limit], |r| episode_from(r).map(|mut e| { e.notes = None; e }))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

pub fn get_episode(conn: &Connection, id: &str) -> Result<Option<Episode>> {
    let sql = format!("SELECT {EPISODE_COLS} FROM episodes e LEFT JOIN shows s ON s.id=e.show_id WHERE e.id=?1");
    Ok(conn.query_row(&sql, [id], episode_from).optional()?)
}

pub fn set_audio_url(conn: &Connection, id: &str, url: &str) -> Result<()> {
    conn.execute("UPDATE episodes SET audio_url=?2 WHERE id=?1", params![id, url])?;
    Ok(())
}

/// Gives the video's length to the episodes playing it that have none (feeds that carry no length).
pub fn fill_video_duration(conn: &Connection, video_id: &str, seconds: f64) -> Result<usize> {
    if seconds <= 0.0 { return Ok(0); }
    Ok(conn.execute("UPDATE episodes SET duration=?2 WHERE video_id=?1 AND COALESCE(duration, 0) <= 0", params![video_id, seconds])?)
}

/// Clears YouTube video_id (and YouTube fallback cover) for an audio-only curated show whose episodes
/// previously inherited a 1:1 static-cover YouTube upload.
pub fn clear_show_videos(conn: &Connection, show_id: &str) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE episodes SET video_id=NULL, cover=CASE WHEN cover LIKE 'https://i.ytimg.com/%' THEN NULL ELSE cover END WHERE show_id=?1 AND video_id IS NOT NULL",
        params![show_id],
    )?)
}

pub fn set_progress(conn: &Connection, id: &str, seconds: f64) -> Result<()> {
    let s = seconds.max(0.0);
    // Resuming playback past 5 s clears an explicit "unseen" reset so auto-tracking takes over again.
    let updated = conn.execute(
        "UPDATE episodes SET progress=?2, watched_override = CASE WHEN watched_override = 'unseen' AND ?2 > 5.0 THEN NULL ELSE watched_override END WHERE id=?1",
        params![id, s],
    )?;
    if updated > 0 {
        if let Some((progress, watched)) = conn
            .query_row("SELECT progress, watched_override FROM episodes WHERE id=?1", [id], |r| {
                Ok((r.get::<_, f64>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .optional()?
        {
            conn.execute(
                "INSERT OR REPLACE INTO sync_episodes(episode_id, progress, watched_override, updated_at) VALUES(?1, ?2, ?3, ?4)",
                params![id, progress, watched, now_ms()],
            )?;
        }
    }
    Ok(())
}

/// Sets or clears an explicit watched override ("done" | "unseen" | None). Marking "unseen" also resets progress to 0.
pub fn set_watched(conn: &Connection, id: &str, watched: Option<&str>) -> Result<()> {
    let updated = match watched {
        Some("done") => conn.execute("UPDATE episodes SET watched_override = 'done' WHERE id = ?1", [id])?,
        Some("unseen") => conn.execute("UPDATE episodes SET watched_override = 'unseen', progress = 0 WHERE id = ?1", [id])?,
        _ => conn.execute("UPDATE episodes SET watched_override = NULL WHERE id = ?1", [id])?,
    };
    if updated > 0 {
        if let Some((progress, watched_cur)) = conn
            .query_row("SELECT progress, watched_override FROM episodes WHERE id=?1", [id], |r| {
                Ok((r.get::<_, f64>(0)?, r.get::<_, Option<String>>(1)?))
            })
            .optional()?
        {
            conn.execute(
                "INSERT OR REPLACE INTO sync_episodes(episode_id, progress, watched_override, updated_at) VALUES(?1, ?2, ?3, ?4)",
                params![id, progress, watched_cur, now_ms()],
            )?;
        }
    }
    Ok(())
}

/// Marks every unwatched episode of a show as watched ("done"); returns how many changed.
pub fn mark_show_watched(conn: &Connection, show_id: &str) -> Result<usize> {
    let select_sql = format!("SELECT e.id, e.progress FROM episodes e WHERE e.show_id = ?1 AND NOT {IS_WATCHED_SQL}");
    let affected: Vec<(String, f64)> = {
        let mut st = conn.prepare(&select_sql)?;
        let rows = st.query_map([show_id], |r| Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?)))?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    let sql = format!("UPDATE episodes SET watched_override = 'done' WHERE show_id = ?1 AND NOT {IS_WATCHED_SQL}");
    // Replace table alias `e.` with direct column names for UPDATE.
    let sql = sql.replace("e.", "");
    let changed = conn.execute(&sql, [show_id])?;
    if !affected.is_empty() {
        let now = now_ms();
        let mut st = conn.prepare(
            "INSERT OR REPLACE INTO sync_episodes(episode_id, progress, watched_override, updated_at) VALUES(?1, ?2, 'done', ?3)",
        )?;
        for (id, progress) in affected {
            st.execute(params![id, progress, now])?;
        }
    }
    Ok(changed)
}

pub fn save_transcript(conn: &Connection, t: &Transcript) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO transcripts(episode_id,source,language,segments,created_at) VALUES(?1,?2,?3,?4,?5)",
        params![t.episode_id, t.source, t.language, serde_json::to_string(&t.segments)?, now_ms()],
    )?;
    // A new transcript invalidates translations and summaries made from the old one.
    conn.execute("DELETE FROM blobs WHERE episode_id=?1 AND kind NOT IN ('chat', 'chat-archive', 'notes-full', 'ctx')", [&t.episode_id])?;
    Ok(())
}

pub fn get_transcript(conn: &Connection, id: &str) -> Result<Option<Transcript>> {
    let row: Option<(String, String, String)> = conn
        .query_row("SELECT source,language,segments FROM transcripts WHERE episode_id=?1", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    Ok(match row {
        Some((source, language, segs)) => Some(Transcript { episode_id: id.to_string(), source, language, segments: serde_json::from_str(&segs)? }),
        None => None,
    })
}

pub fn get_blob(conn: &Connection, id: &str, kind: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT data FROM blobs WHERE episode_id=?1 AND kind=?2", [id, kind], |r| r.get(0)).optional()?)
}

/// Returns all episode ids stored for a show in a single query.
pub fn list_show_episode_ids(conn: &Connection, show_id: &str) -> Result<std::collections::HashSet<String>> {
    let mut st = conn.prepare("SELECT id FROM episodes WHERE show_id=?1")?;
    let rows = st.query_map([show_id], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Returns raw transcript segments JSON and optional Chinese translation JSON for pre-filtered search.
pub fn raw_transcript_and_zh(conn: &Connection, id: &str) -> Result<Option<(String, Option<String>)>> {
    Ok(conn
        .query_row(
            "SELECT t.segments, b.data FROM transcripts t LEFT JOIN blobs b ON b.episode_id = t.episode_id AND b.kind = 'zh' WHERE t.episode_id = ?1",
            [id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .optional()?)
}

pub fn is_synced_blob(episode_id: &str, kind: &str) -> bool {
    if episode_id == "_app" {
        return matches!(kind, "favorites" | "notes");
    }
    if matches!(episode_id, "_sync" | "learn" | "stats") {
        return false;
    }
    matches!(
        kind,
        "summary"
            | "summary-from"
            | "summary-model"
            | "deep-read"
            | "article"
            | "chat"
            | "chat-archive"
            | "highlights"
            | "notes-zh"
    )
}

pub fn set_blob(conn: &Connection, id: &str, kind: &str, data: &str) -> Result<()> {
    conn.execute("INSERT OR REPLACE INTO blobs(episode_id,kind,data) VALUES(?1,?2,?3)", params![id, kind, data])?;
    if is_synced_blob(id, kind) {
        conn.execute(
            "INSERT OR REPLACE INTO sync_blobs(episode_id,kind,updated_at) VALUES(?1,?2,?3)",
            params![id, kind, now_ms()],
        )?;
    }
    Ok(())
}

pub fn delete_blob(conn: &Connection, id: &str, kind: &str) -> Result<()> {
    conn.execute("DELETE FROM blobs WHERE episode_id=?1 AND kind=?2", params![id, kind])?;
    Ok(())
}

/// Atomic read-modify-write inside an IMMEDIATE transaction so CLI and GUI updates don't clobber each other.
pub fn update_blob<F>(conn: &mut Connection, id: &str, kind: &str, f: F) -> Result<String>
where
    F: FnOnce(Option<String>) -> Result<String>,
{
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let cur: Option<String> = tx
        .query_row("SELECT data FROM blobs WHERE episode_id=?1 AND kind=?2", [id, kind], |r| r.get(0))
        .optional()?;
    let next = f(cur)?;
    tx.execute("INSERT OR REPLACE INTO blobs(episode_id,kind,data) VALUES(?1,?2,?3)", params![id, kind, &next])?;
    if is_synced_blob(id, kind) {
        tx.execute(
            "INSERT OR REPLACE INTO sync_blobs(episode_id,kind,updated_at) VALUES(?1,?2,?3)",
            params![id, kind, now_ms()],
        )?;
    }
    tx.commit()?;
    Ok(next)
}

pub fn data_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA data_version", [], |r| r.get(0))?)
}

pub fn get_setting(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM settings WHERE key=?1", [key], |r| r.get(0)).optional()?)
}

pub fn set_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute("INSERT OR REPLACE INTO settings(key,value) VALUES(?1,?2)", params![key, value])?;
    Ok(())
}

pub fn tr_get(conn: &Connection, key: &str) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT value FROM tr_cache WHERE key=?1", [key], |r| r.get(0)).optional()?)
}

pub fn tr_put(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute("INSERT OR REPLACE INTO tr_cache(key,value) VALUES(?1,?2)", params![key, value])?;
    Ok(())
}

/// Adds listening time to today's row for this hour (local time).
pub fn log_listen(conn: &Connection, episode_id: &str, show_id: &str, seconds: f64, translated: bool) -> Result<()> {
    let now = chrono::Local::now();
    conn.execute(
        "INSERT INTO listens(day,hour,episode_id,show_id,seconds,translated) VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(day,hour,episode_id) DO UPDATE SET seconds = seconds + excluded.seconds, translated = MAX(translated, excluded.translated)",
        params![now.format("%Y-%m-%d").to_string(), now.format("%H").to_string().parse::<i64>().unwrap_or(0), episode_id, show_id, seconds.clamp(0.0, 120.0), translated as i64],
    )?;
    Ok(())
}

/// Every listening row since a day (YYYY-MM-DD), with the episode's show and kind for grouping.
pub fn listens_since(conn: &Connection, since: &str) -> Result<Vec<serde_json::Value>> {
    let mut st = conn.prepare(
        "SELECT l.day, l.hour, l.episode_id, l.show_id, l.seconds, l.translated, COALESCE(s.title,''), COALESCE(s.kind,''), COALESCE(s.cover,''), COALESCE(e.title,''), COALESCE(e.video_id,''),
                COALESCE(e.duration,0), COALESCE(e.progress,0), COALESCE(e.published,0)
         FROM listens l LEFT JOIN shows s ON s.id = l.show_id LEFT JOIN episodes e ON e.id = l.episode_id WHERE l.day >= ?1 ORDER BY l.day")?;
    let rows = st.query_map([since], |r| Ok(serde_json::json!({
        "day": r.get::<_, String>(0)?, "hour": r.get::<_, i64>(1)?, "episodeId": r.get::<_, String>(2)?, "showId": r.get::<_, String>(3)?,
        "seconds": r.get::<_, f64>(4)?, "translated": r.get::<_, i64>(5)? == 1, "showTitle": r.get::<_, String>(6)?, "kind": r.get::<_, String>(7)?,
        "cover": r.get::<_, String>(8)?, "episodeTitle": r.get::<_, String>(9)?, "video": !r.get::<_, String>(10)?.is_empty(),
        "duration": r.get::<_, f64>(11)?, "progress": r.get::<_, f64>(12)?, "published": r.get::<_, i64>(13)?,
    })))?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

#[derive(Clone, Debug)]
pub struct SyncSubLedger {
    pub feed_url: String,
    pub deleted_at: Option<i64>,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct SyncEpLedger {
    pub episode_id: String,
    pub progress: f64,
    pub watched_override: Option<String>,
    pub updated_at: i64,
}

pub fn list_sync_subs(conn: &Connection) -> Result<Vec<SyncSubLedger>> {
    let mut st = conn.prepare("SELECT feed_url, deleted_at, updated_at FROM sync_subs")?;
    let rows = st.query_map([], |r| {
        Ok(SyncSubLedger {
            feed_url: r.get(0)?,
            deleted_at: r.get(1)?,
            updated_at: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn upsert_sync_sub_raw(conn: &Connection, feed_url: &str, deleted_at: Option<i64>, updated_at: i64) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO sync_subs(feed_url, deleted_at, updated_at) VALUES(?1, ?2, ?3)",
        params![feed_url.trim(), deleted_at, updated_at],
    )?;
    Ok(())
}

pub fn prune_sync_sub_tombstones(conn: &Connection, cutoff_ms: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM sync_subs WHERE deleted_at IS NOT NULL AND deleted_at < ?1",
        params![cutoff_ms],
    )?)
}

/// Lists tracked episode progress/watched states, including legacy non-zero rows not yet in `sync_episodes`.
pub fn list_sync_episodes(conn: &Connection) -> Result<Vec<SyncEpLedger>> {
    let mut st = conn.prepare(
        "SELECT episode_id, progress, watched_override, updated_at FROM sync_episodes
         UNION ALL
         SELECT e.id, e.progress, e.watched_override, 1
         FROM episodes e
         WHERE (e.progress > 0 OR e.watched_override IS NOT NULL)
           AND NOT EXISTS (SELECT 1 FROM sync_episodes s WHERE s.episode_id = e.id)",
    )?;
    let rows = st.query_map([], |r| {
        Ok(SyncEpLedger {
            episode_id: r.get(0)?,
            progress: r.get(1)?,
            watched_override: r.get(2)?,
            updated_at: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn apply_sync_episode_raw(
    conn: &Connection,
    episode_id: &str,
    progress: f64,
    watched_override: Option<&str>,
    updated_at: i64,
) -> Result<()> {
    let p = progress.max(0.0);
    conn.execute(
        "UPDATE episodes SET progress=?2, watched_override=?3 WHERE id=?1",
        params![episode_id, p, watched_override],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO sync_episodes(episode_id, progress, watched_override, updated_at) VALUES(?1, ?2, ?3, ?4)",
        params![episode_id, p, watched_override, updated_at],
    )?;
    Ok(())
}

pub fn delete_show_with_tombstone(conn: &Connection, id: &str, feed_url: &str, deleted_at: i64) -> Result<()> {
    delete_show(conn, id, None)?;
    upsert_sync_sub_raw(conn, feed_url, Some(deleted_at), deleted_at)?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct SyncBlobRow {
    pub episode_id: String,
    pub kind: String,
    pub data: String,
    pub updated_at: i64,
}

pub fn list_synced_blobs(conn: &Connection) -> Result<Vec<SyncBlobRow>> {
    let mut st = conn.prepare(
        "SELECT b.episode_id, b.kind, b.data, COALESCE(s.updated_at, 1)
         FROM blobs b
         LEFT JOIN sync_blobs s ON s.episode_id = b.episode_id AND s.kind = b.kind
         WHERE b.kind IN ('summary','summary-from','summary-model','deep-read','article','chat','chat-archive','highlights','notes-zh','favorites','notes')
           AND b.data != ''",
    )?;
    let rows = st.query_map([], |r| {
        Ok(SyncBlobRow {
            episode_id: r.get(0)?,
            kind: r.get(1)?,
            data: r.get(2)?,
            updated_at: r.get(3)?,
        })
    })?;
    Ok(rows
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .filter(|row| is_synced_blob(&row.episode_id, &row.kind))
        .collect())
}

pub fn apply_sync_blob_raw(
    conn: &Connection,
    episode_id: &str,
    kind: &str,
    data: &str,
    updated_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO blobs(episode_id,kind,data) VALUES(?1,?2,?3)",
        params![episode_id, kind, data],
    )?;
    conn.execute(
        "INSERT OR REPLACE INTO sync_blobs(episode_id,kind,updated_at) VALUES(?1,?2,?3)",
        params![episode_id, kind, updated_at],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_unsubscribe_keeps_the_open_episode_and_progress() {
        let mut conn = open(Path::new(":memory:")).unwrap();
        let show = Show {
            id: "show".into(), kind: "podcast".into(), title: "Show".into(),
            feed_url: "https://example.com/feed.xml".into(), ..Default::default()
        };
        upsert_show(&conn, &show).unwrap();
        let episodes = ["open", "played", "unplayed"].map(|id| Episode {
            id: id.into(), show_id: show.id.clone(), guid: id.into(), title: id.into(), ..Default::default()
        });
        upsert_episodes(&mut conn, &episodes).unwrap();
        set_progress(&conn, "played", 42.0).unwrap();

        delete_show(&conn, &show.id, Some("open")).unwrap();

        assert!(get_show(&conn, &show.id).unwrap().is_none());
        assert!(get_episode(&conn, "open").unwrap().is_some());
        assert_eq!(get_episode(&conn, "played").unwrap().unwrap().progress, 42.0);
        assert!(get_episode(&conn, "unplayed").unwrap().is_none());
        let cached = get_blob(&conn, &show.id, "show-json").unwrap().unwrap();
        assert_eq!(serde_json::from_str::<Show>(&cached).unwrap().feed_url, show.feed_url);
    }

    /// Audio first, video hours later (词元之外 uploads to YouTube after the podcast): a refresh that finds the
    /// video fills it in, and one that doesn't find it (the channel feed failed) keeps it.
    #[test]
    fn a_video_found_later_is_kept() {
        let path = std::env::temp_dir().join(format!("subcast-db-video-{}.sqlite", uuid::Uuid::new_v4()));
        let mut conn = open(&path).unwrap();
        let mut ep = Episode { id: "e".into(), show_id: "s".into(), guid: "g".into(), title: "#011".into(), audio_url: Some("a.mp3".into()), ..Default::default() };
        upsert_episodes(&mut conn, std::slice::from_ref(&ep)).unwrap();
        assert_eq!(get_episode(&conn, "e").unwrap().unwrap().video_id, None);
        ep.video_id = Some("XQGJPVDBZPk".into());
        upsert_episodes(&mut conn, std::slice::from_ref(&ep)).unwrap();
        assert_eq!(get_episode(&conn, "e").unwrap().unwrap().video_id.as_deref(), Some("XQGJPVDBZPk"));
        ep.video_id = None;
        upsert_episodes(&mut conn, std::slice::from_ref(&ep)).unwrap();
        assert_eq!(get_episode(&conn, "e").unwrap().unwrap().video_id.as_deref(), Some("XQGJPVDBZPk"));
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn deep_column_migrates_and_round_trips() {
        let path = std::env::temp_dir().join(format!("subcast-db-test-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            // A database from before the deep column existed.
            let old = Connection::open(&path).unwrap();
            old.execute_batch(
                "CREATE TABLE episodes(id TEXT PRIMARY KEY, show_id TEXT NOT NULL, guid TEXT NOT NULL, title TEXT NOT NULL,
                 published INTEGER, duration REAL, audio_url TEXT, video_id TEXT, link TEXT, cover TEXT, notes TEXT,
                 transcript_url TEXT, transcript_type TEXT, progress REAL NOT NULL DEFAULT 0);
                 INSERT INTO episodes(id,show_id,guid,title,transcript_type) VALUES('a','s','g','A','qiaomu'),('b','s','h','B',NULL);",
            ).unwrap();
        }
        let mut conn = open(&path).unwrap();
        assert!(get_episode(&conn, "a").unwrap().unwrap().deep);
        assert!(!get_episode(&conn, "b").unwrap().unwrap().deep);
        // Reopening must not try to add the column again.
        drop(conn);
        conn = open(&path).unwrap();
        let mut ep = get_episode(&conn, "b").unwrap().unwrap();
        ep.deep = true;
        upsert_episodes(&mut conn, std::slice::from_ref(&ep)).unwrap();
        assert!(get_episode(&conn, "b").unwrap().unwrap().deep);
        ep.deep = false;
        upsert_episodes(&mut conn, std::slice::from_ref(&ep)).unwrap();
        assert!(!get_episode(&conn, "b").unwrap().unwrap().deep);
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn watched_override_and_unplayed_counts() {
        let path = std::env::temp_dir().join(format!("subcast-watched-test-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut conn = open(&path).unwrap();
        let show = Show { id: "s1".into(), kind: "podcast".into(), title: "Show".into(), feed_url: "https://ex.com/rss".into(), ..Default::default() };
        upsert_show(&conn, &show).unwrap();
        let eps = vec![
            // Unseen (0 s)
            Episode { id: "e1".into(), show_id: "s1".into(), guid: "1".into(), title: "E1".into(), duration: Some(1800.0), ..Default::default() },
            // In progress (600 / 1800 s) -> still counts as unwatched
            Episode { id: "e2".into(), show_id: "s1".into(), guid: "2".into(), title: "E2".into(), duration: Some(1800.0), ..Default::default() },
            // Auto-watched via <= 90 s remaining on > 5 min episode (1720 / 1800 s)
            Episode { id: "e3".into(), show_id: "s1".into(), guid: "3".into(), title: "E3".into(), duration: Some(1800.0), ..Default::default() },
            // Auto-watched via >= 95% on short episode (190 / 200 s)
            Episode { id: "e4".into(), show_id: "s1".into(), guid: "4".into(), title: "E4".into(), duration: Some(200.0), ..Default::default() },
            // No duration -> not auto-watched until override = 'done'
            Episode { id: "e5".into(), show_id: "s1".into(), guid: "5".into(), title: "E5".into(), duration: None, ..Default::default() },
        ];
        upsert_episodes(&mut conn, &eps).unwrap();
        set_progress(&conn, "e2", 600.0).unwrap();
        set_progress(&conn, "e3", 1720.0).unwrap();
        set_progress(&conn, "e4", 190.0).unwrap();
        set_progress(&conn, "e5", 999.0).unwrap();

        // e1, e2, e5 are unwatched (3); e3 and e4 are auto-watched.
        assert_eq!(list_shows(&conn).unwrap()[0].unplayed, 3);

        // Manually mark e5 done -> 2 unwatched.
        set_watched(&conn, "e5", Some("done")).unwrap();
        assert_eq!(get_episode(&conn, "e5").unwrap().unwrap().watched_override.as_deref(), Some("done"));
        assert_eq!(list_shows(&conn).unwrap()[0].unplayed, 2);

        // Manually reset e3 to unseen -> progress resets to 0 and unplayed becomes 3.
        set_watched(&conn, "e3", Some("unseen")).unwrap();
        let e3 = get_episode(&conn, "e3").unwrap().unwrap();
        assert_eq!(e3.progress, 0.0);
        assert_eq!(e3.watched_override.as_deref(), Some("unseen"));
        assert_eq!(list_shows(&conn).unwrap()[0].unplayed, 3);

        // Playing e3 past 5 s clears the 'unseen' override so progress tracks normally.
        set_progress(&conn, "e3", 20.0).unwrap();
        assert_eq!(get_episode(&conn, "e3").unwrap().unwrap().watched_override, None);

        // set_progress allows resetting cleanly to 0.
        set_progress(&conn, "e2", 0.0).unwrap();
        assert_eq!(get_episode(&conn, "e2").unwrap().unwrap().progress, 0.0);

        // Mark entire show watched -> 3 episodes updated, unplayed becomes 0.
        assert_eq!(mark_show_watched(&conn, "s1").unwrap(), 3);
        assert_eq!(list_shows(&conn).unwrap()[0].unplayed, 0);

        drop(conn);
        let _ = std::fs::remove_file(&path);
    }
}
