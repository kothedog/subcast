//! Smart show/episode reference and timestamp parsing so CLI callers never have to type full 16-char hash IDs.
use crate::{ctx::CoreCtx, db, model::*, ops, settings};
use anyhow::{anyhow, Result};
use rusqlite::Connection;

fn looks_like_url(s: &str) -> bool {
    let t = s.trim();
    t.starts_with("http://")
        || t.starts_with("https://")
        || t.starts_with('@')
        || t.contains("youtube.com/")
        || t.contains("youtu.be/")
        || t.contains("xiaoyuzhoufm.com/")
        || t.contains("podcasts.apple.com/")
        || t.ends_with(".xml")
        || t.ends_with(".rss")
}

/// Resolves `<show>`: exact ID -> unique ID prefix (len >= 4) -> exact title/alias -> fuzzy title/alias/feed_url.
pub fn resolve_show(conn: &Connection, reference: &str) -> Result<Show> {
    let q = reference.trim();
    if q.is_empty() {
        return Err(anyhow!("请指定节目 ID 或名称")); // i18n-ignore
    }
    if let Some(s) = db::get_show(conn, q)? {
        return Ok(s);
    }
    let shows = db::list_shows(conn)?;
    let ql = q.to_lowercase();

    // Unique ID prefix (>= 4 chars)
    if q.len() >= 4 {
        let prefix_hits: Vec<&Show> = shows.iter().filter(|s| s.id.to_lowercase().starts_with(&ql)).collect();
        if prefix_hits.len() == 1 {
            return Ok(prefix_hits[0].clone());
        }
        if prefix_hits.len() > 1 {
            return Err(anyhow!(
                "节目 ID 前缀不唯一（匹配到 {} 个）：{}", // i18n-ignore
                prefix_hits.len(),
                prefix_hits.iter().map(|s| format!("{} ({})", s.id, s.title)).collect::<Vec<_>>().join(", ")
            )); // i18n-ignore
        }
    }

    // Load custom aliases (`showAliases`) if present
    let aliases = db::get_setting(conn, "app")
        .ok()
        .flatten()
        .and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok())
        .and_then(|v| v.get("showAliases").cloned())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    let alias_of = |s: &Show| -> Option<String> {
        aliases.get(&s.id).and_then(|v| v.as_str()).map(|x| x.to_lowercase())
    };

    // Exact title or alias match (case-insensitive)
    let exact: Vec<&Show> = shows
        .iter()
        .filter(|s| s.title.to_lowercase() == ql || alias_of(s).is_some_and(|a| a == ql))
        .collect();
    if exact.len() == 1 {
        return Ok(exact[0].clone());
    }

    // Substring match on title, alias, or feed_url
    let fuzzy: Vec<&Show> = shows
        .iter()
        .filter(|s| {
            s.title.to_lowercase().contains(&ql)
                || s.feed_url.to_lowercase().contains(&ql)
                || alias_of(s).is_some_and(|a| a.contains(&ql))
        })
        .collect();
    match fuzzy.len() {
        0 => Err(anyhow!("找不到节目：{q}")), // i18n-ignore
        1 => Ok(fuzzy[0].clone()),
        _ => Err(anyhow!(
            "找到多个匹配节目，请写得更具体或使用 ID：\n  {}", // i18n-ignore
            fuzzy
                .iter()
                .take(8)
                .map(|s| format!("{}  {}", s.id, s.title))
                .collect::<Vec<_>>()
                .join("\n  ")
        )), // i18n-ignore
    }
}

fn match_in_show(eps: Vec<Episode>, selector: &str) -> Result<Episode> {
    let sel = selector.trim();
    if sel.eq_ignore_ascii_case("latest") || sel == "@1" {
        return eps.into_iter().next().ok_or_else(|| anyhow!("这个节目里还没有单集")); // i18n-ignore
    }
    if let Some(n_str) = sel.strip_prefix('@') {
        if let Ok(idx) = n_str.parse::<usize>() {
            if idx >= 1 {
                if let Some(ep) = eps.into_iter().nth(idx - 1) {
                    return Ok(ep);
                }
                return Err(anyhow!("超出该节目的单集数量：@{idx}")); // i18n-ignore
            }
        }
    }
    let sl = sel.to_lowercase();
    let exact_id: Vec<Episode> = eps.iter().filter(|e| e.id == sel || (sel.len() >= 4 && e.id.starts_with(&sl))).cloned().collect();
    if exact_id.len() == 1 {
        return Ok(exact_id.into_iter().next().unwrap());
    }
    let hits: Vec<Episode> = eps.into_iter().filter(|e| e.title.to_lowercase().contains(&sl)).collect();
    match hits.len() {
        0 => Err(anyhow!("没有匹配的单集：{selector}")), // i18n-ignore
        1 => Ok(hits.into_iter().next().unwrap()),
        _ => Err(anyhow!(
            "匹配到多集，请提供更精确的标题或 ID：\n  {}", // i18n-ignore
            hits.iter().take(6).map(|e| format!("{}  {}", e.id, e.title)).collect::<Vec<_>>().join("\n  ")
        )), // i18n-ignore
    }
}

/// Resolves `<ep>`:
/// - exact ID
/// - unique ID prefix across all episodes (len >= 4)
/// - `latest` / `@1` / `@N` across recent/all episodes
/// - URL (finds existing or adds as `single`)
/// - `<show>:<selector>` (show ID/name + `latest`/`@N`/title keyword)
pub async fn resolve_episode(ctx: &CoreCtx, reference: &str) -> Result<Episode> {
    let q = reference.trim();
    if q.is_empty() {
        return Err(anyhow!("请指定单集 ID、URL 或名称")); // i18n-ignore
    }

    // 1. Exact ID or prefix check in DB
    {
        let conn = ctx.db.lock();
        if let Some(ep) = db::get_episode(&conn, q)? {
            return Ok(ep);
        }
    }

    // 2. Direct URL (check DB first via link/guid/audio_url, else fetch single episode via ops)
    if looks_like_url(q) {
        let found = {
            let conn = ctx.db.lock();
            let all = db::list_episodes(&conn, None, 2000)?;
            all.into_iter().find(|e| {
                e.link.as_deref() == Some(q)
                    || e.audio_url.as_deref() == Some(q)
                    || crate::feeds::youtube_video_id(q).is_some_and(|v| e.video_id.as_deref() == Some(v.as_str()))
            })
        };
        if let Some(ep) = found {
            return Ok(ep);
        }
        // Never subscribes: a show or channel link gives its newest episode, the show is only previewed.
        return match ops::fetch_input(ctx, q).await? {
            ops::Fetched::Single(ep) => Ok(ep),
            ops::Fetched::Show(show, eps) => ops::keep_preview(ctx, show, eps)?
                .episodes
                .into_iter()
                .next()
                .ok_or_else(|| anyhow!("没有可播放的单集")), // i18n-ignore
        };
    }

    // 3. `<show>:<selector>` syntax (avoid splitting URL schemes already handled above)
    if let Some((show_ref, ep_sel)) = q.split_once(':') {
        if !show_ref.trim().is_empty() && !ep_sel.trim().is_empty() {
            let found = {
                let conn = ctx.db.lock();
                resolve_show(&conn, show_ref).ok().map(|show| db::list_episodes(&conn, Some(&show.id), 5000)).transpose()?
            };
            // A show that isn't subscribed: found by name in the podcast and YouTube directory, not followed.
            let eps = match found { Some(eps) => eps, None => find_show_online_for(ctx, show_ref, ep_sel.trim()).await?.episodes };
            return match_in_show(eps, ep_sel);
        }
    }

    // 4. Global prefix, `latest` / `@N`, or global title substring
    let conn = ctx.db.lock();
    let ql = q.to_lowercase();
    if q.len() >= 4 {
        let mut st = conn.prepare("SELECT id FROM episodes WHERE id LIKE ?1 LIMIT 6")?;
        let ids: Vec<String> = st.query_map([format!("{q}%")], |r| r.get(0))?.collect::<std::result::Result<_, _>>()?;
        if ids.len() == 1 {
            if let Some(ep) = db::get_episode(&conn, &ids[0])? {
                return Ok(ep);
            }
        } else if ids.len() > 1 {
            return Err(anyhow!("单集 ID 前缀不唯一，请输入更多字符：{}", ids.join(", "))); // i18n-ignore
        }
    }
    if q.eq_ignore_ascii_case("latest") || q.starts_with('@') {
        let eps = db::list_episodes(&conn, None, 200)?;
        return match_in_show(eps, q);
    }
    let eps = db::list_episodes(&conn, None, 2000)?;
    let hits: Vec<Episode> = eps.into_iter().filter(|e| e.title.to_lowercase().contains(&ql)).collect();
    match hits.len() {
        // Not an episode in the library: a show name (subscribed or not) means its newest episode.
        0 => {
            if let Ok(show) = resolve_show(&conn, q) {
                return match_in_show(db::list_episodes(&conn, Some(&show.id), 1)?, "latest");
            }
            drop(conn);
            match find_show_online(ctx, q).await {
                Ok(p) => match_in_show(p.episodes, "latest"),
                Err(e) if e.downcast_ref::<AmbiguousShow>().is_some() => Err(e),
                Err(_) => Err(anyhow!("找不到单集：{q}")), // i18n-ignore
            }
        }
        1 => Ok(hits.into_iter().next().unwrap()),
        _ => Err(anyhow!(
            "匹配到多集，请使用 <节目>:<关键词> 或单集 ID：\n  {}", // i18n-ignore
            hits.iter()
                .take(6)
                .map(|e| format!("{}  [{}] {}", e.id, e.show_title.as_deref().unwrap_or(""), e.title))
                .collect::<Vec<_>>()
                .join("\n  ")
        )), // i18n-ignore
    }
}

/// A show found by name in the podcast and YouTube directory, with what a person needs to tell look-alikes apart.
#[derive(serde::Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct ShowCandidate {
    pub title: String,
    /// Channel handle (@name) or podcast author.
    pub author: String,
    /// "youtube" or "podcast".
    pub source: String,
    /// Feed or channel link: pass it back (to preview_show, or as a reference) to use this show.
    pub feed_url: String,
    pub genre: Option<String>,
    pub episodes: Option<i64>,
    pub latest_title: Option<String>,
    /// Newest episode's date, ms since epoch.
    pub latest_published: Option<i64>,
}

/// Several shows answer to a name: the caller asks the person which one (never a silent guess).
#[derive(Debug)]
pub struct AmbiguousShow { pub query: String, pub selector: String, pub candidates: Vec<ShowCandidate> }

impl std::fmt::Display for AmbiguousShow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "「{}」对应多个节目，请选择一个（把它的链接作为参数再运行）：", self.query)?; // i18n-ignore (CLI/MCP text)
        for (i, c) in self.candidates.iter().enumerate() {
            writeln!(f, "  {}. {}", i + 1, describe_candidate(c))?;
        }
        Ok(())
    }
}
impl std::error::Error for AmbiguousShow {}

/// One line: title · source · channel · episodes · newest · link.
pub fn describe_candidate(c: &ShowCandidate) -> String {
    let src = if c.source == "youtube" { "YouTube 频道" } else { "播客" }; // i18n-ignore
    let mut parts = vec![c.title.clone(), src.to_string()];
    if !c.author.is_empty() && c.author != "YouTube" { parts.push(c.author.clone()); }
    if let Some(n) = c.episodes { parts.push(format!("{n} 集")); } // i18n-ignore
    if let Some(ms) = c.latest_published {
        let day = chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default();
        parts.push(format!("最新 {day}{}", c.latest_title.as_deref().map(|t| format!("「{}」", t.chars().take(40).collect::<String>())).unwrap_or_default())); // i18n-ignore
    }
    parts.push(c.feed_url.clone());
    parts.join(" · ")
}

fn norm(s: &str) -> String { s.chars().filter(|c| c.is_alphanumeric()).flat_map(|c| c.to_lowercase()).collect() }

/// Shows matching a name in the directory (YouTube channels and podcasts), each with its newest episode.
pub async fn show_candidates(ctx: &CoreCtx, name: &str, max: usize) -> Result<Vec<ShowCandidate>> {
    let hits = crate::feeds::search_podcasts(name.trim(), "cn").await?;
    let base: Vec<ShowCandidate> = hits.into_iter().take(max).map(|h| ShowCandidate {
        title: h.title, author: h.author, source: h.source.unwrap_or_else(|| "podcast".into()), feed_url: h.feed_url,
        genre: h.genre, episodes: h.episodes, latest_title: None, latest_published: None,
    }).collect();
    let latest = futures_util::future::join_all(base.iter().map(|c| {
        let url = c.feed_url.clone();
        async move {
            let got = tokio::time::timeout(std::time::Duration::from_secs(15), ops::fetch_input(ctx, &url)).await;
            match got { Ok(Ok(ops::Fetched::Show(_, eps))) => { let n = eps.len() as i64; eps.into_iter().max_by_key(|e| e.published.unwrap_or(0)).map(|e| (e.title, e.published, n)) } _ => None }
        }
    })).await;
    Ok(base.into_iter().zip(latest).map(|(mut c, l)| {
        if let Some((t, p, n)) = l { c.latest_title = Some(t); c.latest_published = p; if c.episodes.is_none() { c.episodes = Some(n); } }
        c
    }).collect())
}

/// A show by name, previewed (episodes kept, not followed). Used without asking only when exactly one result has
/// that exact name; otherwise an AmbiguousShow error lists the candidates.
pub async fn find_show_online(ctx: &CoreCtx, name: &str) -> Result<ops::Preview> {
    find_show_online_for(ctx, name, "latest").await
}

async fn find_show_online_for(ctx: &CoreCtx, name: &str, selector: &str) -> Result<ops::Preview> {
    let hits = crate::feeds::search_podcasts(name.trim(), "cn").await?;
    if hits.is_empty() { return Err(anyhow!("没有找到节目：{name}")); } // i18n-ignore
    let q = norm(name);
    let exact: Vec<&crate::feeds::SearchResult> = hits.iter().filter(|h| norm(&h.title) == q || norm(h.author.trim_start_matches('@')) == q).collect();
    if exact.len() == 1 {
        let url = exact[0].feed_url.clone();
        return preview_url(ctx, &url).await;
    }
    let candidates = show_candidates(ctx, name, 6).await?;
    Err(AmbiguousShow { query: name.trim().to_string(), selector: selector.to_string(), candidates }.into())
}

/// Previews a feed or channel link (episodes kept, show not followed).
pub async fn preview_url(ctx: &CoreCtx, url: &str) -> Result<ops::Preview> {
    match ops::fetch_input(ctx, url).await? {
        ops::Fetched::Show(show, eps) => ops::keep_preview(ctx, show, eps),
        ops::Fetched::Single(_) => Err(anyhow!("这是单集链接，不是节目")), // i18n-ignore
    }
}

/// Picks an episode of a previewed show (latest, @N, id or title words).
pub fn pick_in_preview(p: ops::Preview, selector: &str) -> Result<Episode> { match_in_show(p.episodes, selector) }

/// Parses `ss`, `mm:ss`, `h:mm:ss`, `12.5s`, `1m30s`, `1h2m3s`.
pub fn parse_timestamp(raw: &str) -> Result<f64> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(anyhow!("时间不能为空")); // i18n-ignore
    }
    if let Ok(sec) = s.trim_end_matches('s').parse::<f64>() {
        if !s.contains(':') && !s.contains('m') && !s.contains('h') {
            return Ok(sec.max(0.0));
        }
    }
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 2 || parts.len() == 3 {
            let mut total = 0.0;
            for p in parts {
                total = total * 60.0 + p.trim().parse::<f64>().map_err(|_| anyhow!("无法识别时间格式：{raw}"))?; // i18n-ignore
            }
            return Ok(total.max(0.0));
        }
    }
    let lower = s.to_ascii_lowercase();
    if lower.contains('h') || lower.contains('m') || lower.contains('s') {
        let mut total = 0.0;
        let mut num = String::new();
        for c in lower.chars() {
            if c.is_ascii_digit() || c == '.' {
                num.push(c);
            } else if !num.is_empty() {
                let v: f64 = num.parse().map_err(|_| anyhow!("无法识别时间格式：{raw}"))?; // i18n-ignore
                num.clear();
                total += match c {
                    'h' => v * 3600.0,
                    'm' => v * 60.0,
                    's' => v,
                    _ => return Err(anyhow!("无法识别时间格式：{raw}")), // i18n-ignore
                };
            }
        }
        if !num.is_empty() {
            total += num.parse::<f64>().map_err(|_| anyhow!("无法识别时间格式：{raw}"))?; // i18n-ignore
        }
        return Ok(total.max(0.0));
    }
    Err(anyhow!("无法识别时间格式：{raw}")) // i18n-ignore
}

/// Checks if a given target looks like an input URL or local file path vs a library reference.
pub fn is_url_or_path(s: &str) -> bool {
    looks_like_url(s) || std::path::Path::new(s).exists()
}

pub fn show_display_name(ctx: &CoreCtx, s: &Show) -> String {
    if let Ok(cfg) = settings::load_effective(ctx) {
        if let Some(alias) = cfg["showAliases"][&s.id].as_str().filter(|x| !x.trim().is_empty()) {
            return alias.trim().to_string();
        }
    }
    s.title.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_timestamps() {
        assert!((parse_timestamp("90").unwrap() - 90.0).abs() < 1e-6);
        assert!((parse_timestamp("12.5s").unwrap() - 12.5).abs() < 1e-6);
        assert!((parse_timestamp("02:30").unwrap() - 150.0).abs() < 1e-6);
        assert!((parse_timestamp("1:02:03.5").unwrap() - 3723.5).abs() < 1e-6);
        assert!((parse_timestamp("2m15s").unwrap() - 135.0).abs() < 1e-6);
        assert!(parse_timestamp("bad").is_err());
    }

    #[test]
    fn resolves_show_and_episode_refs() {
        let mut conn = db::open(std::path::Path::new(":memory:")).unwrap();
        let show = Show { id: "abcdef1234567890".into(), kind: "podcast".into(), title: "Lex Fridman Podcast".into(), feed_url: "https://ex.com/lex".into(), ..Default::default() };
        db::upsert_show(&conn, &show).unwrap();
        let eps = vec![
            Episode { id: "ep111122223333".into(), show_id: show.id.clone(), guid: "g1".into(), title: "Sam Altman on AGI".into(), published: Some(2000), ..Default::default() },
            Episode { id: "ep999988887777".into(), show_id: show.id.clone(), guid: "g2".into(), title: "Dario Amodei".into(), published: Some(1000), ..Default::default() },
        ];
        db::upsert_episodes(&mut conn, &eps).unwrap();

        assert_eq!(resolve_show(&conn, "abcdef").unwrap().id, show.id);
        assert_eq!(resolve_show(&conn, "lex fridman").unwrap().id, show.id);
        assert_eq!(match_in_show(eps.clone(), "latest").unwrap().id, "ep111122223333");
        assert_eq!(match_in_show(eps.clone(), "@2").unwrap().id, "ep999988887777");
        assert_eq!(match_in_show(eps, "Dario").unwrap().id, "ep999988887777");
    }
}
