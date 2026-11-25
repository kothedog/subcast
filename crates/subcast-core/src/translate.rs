//! Free machine translation for when the user has not set up a model of their own: Google Translate's public
//! `client=gtx` endpoint (the same one elon.qiaomu.ai uses). Mainland China often cannot reach googleapis.com
//! directly, so a small rate-limited relay on elon.qiaomu.ai is tried second. Results are cached in SQLite.
use crate::net;
use anyhow::{anyhow, Result};
use serde_json::Value;

const DIRECT: &str = "https://translate.googleapis.com/translate_a/single";
const RELAY: &str = "https://elon.qiaomu.ai/api/tr";
/// The relay rejects query strings of 1200+ characters, so its chunks stay small.
const RELAY_CHUNK: usize = 300;
const DIRECT_CHUNK: usize = 3500;

fn parse(v: &Value) -> Option<String> {
    let parts = v.get(0)?.as_array()?;
    let text: String = parts.iter().filter_map(|p| p.get(0).and_then(Value::as_str)).collect();
    (!text.is_empty()).then_some(text)
}

async fn direct(text: &str, tl: &str) -> Result<String> {
    let res = net::client()
        .post(DIRECT)
        .query(&[("client", "gtx"), ("sl", "auto"), ("dt", "t"), ("tl", tl)])
        .form(&[("q", text)])
        .timeout(std::time::Duration::from_secs(12))
        .send()
        .await?;
    if !res.status().is_success() { return Err(anyhow!("谷歌翻译返回 {}", res.status().as_u16())); }
    parse(&res.json().await?).ok_or_else(|| anyhow!("谷歌翻译没有返回内容"))
}

async fn relay(text: &str, tl: &str) -> Result<String> {
    let res = net::client()
        .get(RELAY)
        .query(&[("tl", tl), ("q", text)])
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await?;
    if !res.status().is_success() { return Err(anyhow!("翻译服务返回 {}", res.status().as_u16())); }
    parse(&res.json().await?).ok_or_else(|| anyhow!("翻译服务没有返回内容"))
}

/// Groups lines into requests of at most `max` characters (a single longer line goes alone, cut if needed).
fn chunks(lines: &[String], max: usize) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut size = 0;
    for l in lines {
        let l: String = l.replace('\n', " ").chars().take(max).collect();
        let n = l.chars().count() + 1;
        if !cur.is_empty() && size + n > max { out.push(std::mem::take(&mut cur)); size = 0; }
        size += n;
        cur.push(l);
    }
    if !cur.is_empty() { out.push(cur); }
    out
}

async fn translate_group(lines: &[String], tl: &str, use_relay: bool) -> Result<Vec<String>> {
    let joined = lines.join("\n");
    let text = if use_relay { relay(&joined, tl).await? } else { direct(&joined, tl).await? };
    let mut parts: Vec<String> = text.split('\n').map(|s| s.trim().to_string()).collect();
    if parts.len() != lines.len() {
        if lines.len() == 1 { return Ok(vec![text.trim().to_string()]); }
        // The service merged or split lines: translate them one by one so each keeps its own translation.
        parts = Vec::with_capacity(lines.len());
        for l in lines {
            let t = if use_relay { relay(l, tl).await? } else { direct(l, tl).await? };
            parts.push(t.trim().to_string());
        }
    }
    Ok(parts)
}

/// Translates each line into `tl` ("zh-CN" or "en"). Lines come back in the same order.
pub async fn translate_lines(lines: &[String], tl: &str) -> Result<Vec<String>> {
    if lines.is_empty() { return Ok(vec![]); }
    let mut out = Vec::with_capacity(lines.len());
    // Try the direct endpoint first; once it fails, stay on the relay for the rest of this call.
    let mut use_relay = false;
    for group in chunks(lines, DIRECT_CHUNK) {
        if !use_relay {
            match translate_group(&group, tl, false).await {
                Ok(t) => { out.extend(t); continue; }
                Err(_) => use_relay = true,
            }
        }
        for small in chunks(&group, RELAY_CHUNK) {
            out.extend(translate_group(&small, tl, true).await.map_err(|e| anyhow!("翻译失败（{e}），请检查网络或在设置里配置代理"))?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn chunking() {
        let lines: Vec<String> = (0..10).map(|i| format!("line number {i}")).collect();
        let c = chunks(&lines, 40);
        assert!(c.len() > 1);
        assert_eq!(c.iter().map(Vec::len).sum::<usize>(), 10);
    }
    #[tokio::test]
    #[ignore]
    async fn live() {
        let lines = vec!["Recursive self-improvement has started".to_string(), "Powering AI-Driven Drug Discovery".to_string()];
        let t = translate_lines(&lines, "zh-CN").await.unwrap();
        println!("{t:?}");
        assert_eq!(t.len(), 2);
        let r = translate_group(&lines, "zh-CN", true).await.unwrap();
        println!("relay {r:?}");
    }
}
