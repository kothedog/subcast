use crate::{ctx::CoreCtx, feeds, ops};
use anyhow::{anyhow, Result};
use serde::Deserialize;

#[derive(Deserialize, Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Pick {
    pub id: String,
    pub title: String,
    pub zh: String,
    pub by: String,
    pub intro: String,
    pub input: String,
    pub cover: String,
    pub topics: Vec<String>,
    #[serde(default)]
    pub video: bool,
    #[serde(default)]
    pub deep: bool,
    #[serde(default)]
    pub channel: Option<String>,
}

const PICKS_JSON: &str = include_str!("picks.json");

pub fn all() -> Vec<Pick> {
    serde_json::from_str(PICKS_JSON).unwrap_or_default()
}

pub fn list(topic: Option<&str>) -> Vec<Pick> {
    let items = all();
    let Some(q) = topic.map(str::trim).filter(|s| !s.is_empty()) else {
        return items;
    };
    let ql = q.to_lowercase();
    items
        .into_iter()
        .filter(|p| p.topics.iter().any(|t| t.to_lowercase().contains(&ql)))
        .collect()
}

pub fn find(query: &str) -> Option<Pick> {
    let q = query.trim().to_lowercase();
    let items = all();
    items
        .iter()
        .find(|p| p.id.to_lowercase() == q || p.title.to_lowercase() == q || p.zh.to_lowercase() == q)
        .or_else(|| {
            items.iter().find(|p| {
                p.id.to_lowercase().contains(&q)
                    || p.title.to_lowercase().contains(&q)
                    || p.zh.to_lowercase().contains(&q)
            })
        })
        .cloned()
}

pub async fn subscribe(ctx: &CoreCtx, query: &str) -> Result<(Pick, ops::Added)> {
    let pick = find(query).ok_or_else(|| anyhow!("没有在精选目录里找到：{query}"))?; // i18n-ignore
    let added = ops::add_subscription(ctx, &pick.input).await?;
    Ok((pick, added))
}

pub fn same_feed(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim()
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_lowercase()
    };
    norm(a) == norm(b)
}

pub fn is_valid_pick_source(p: &Pick) -> bool {
    p.input.starts_with("http") && !feeds::youtube_video_id(&p.input).is_some()
}
