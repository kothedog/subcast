//! 乔木精读 channels (lex.qiaomu.ai, nopriors.qiaomu.ai, …): curated shows with Chinese titles, Chinese summaries,
//! a Chinese deep-read for each episode and, for many, the original transcript. Each site is static: the episode
//! list is `/data/episodes.json`; an episode page carries the audio, the deep-read and the transcript.
use crate::model::*;
use crate::net;
use anyhow::{anyhow, Result};
use regex::Regex;
use serde::Serialize;

/// `https://lex.qiaomu.ai/...` → `lex`. Not the shared services (rss, api, www…).
pub fn site_id(url: &str) -> Option<String> {
    let c = Regex::new(r"^https?://([a-z0-9-]+)\.qiaomu\.ai(?:/|$)").unwrap().captures(url)?;
    let id = c[1].to_string();
    (!["rss", "api", "www", "xnews", "elon", "blog"].contains(&id.as_str())).then_some(id)
}

/// Square show artwork (the sites' own og images are wide cards).
fn cover_for(id: &str) -> Option<String> {
    let path = match id {
        "lex" => "Podcasts211/v4/ea/42/d7/ea42d7b1-8d4e-dd97-da5d-c508756550ca/mza_16782152345430036863.jpg",
        "allin" => "Podcasts124/v4/c7/d2/92/c7d292ea-44b3-47ff-2f5e-74fa5b23db6c/mza_7005270671777648882.png",
        "nopriors" => "Podcasts221/v4/8f/46/88/8f4688ec-087f-28a1-7f6d-0d401d2e8a00/mza_14043856542292072583.jpg",
        "cogrev" => "Podcasts211/v4/b1/29/0d/b1290dd6-516c-bc2d-53b6-473d18a3dff2/mza_6540782963388625071.jpeg",
        "trainingdata" => "Podcasts221/v4/40/01/bd/4001bd05-9453-3d7f-c034-d815c74d332d/mza_6464974713460854089.png",
        "unsupervised" => "Podcasts221/v4/89/9c/aa/899caaef-e537-c9e8-2e34-788b847b1a15/mza_3258716992355382594.jpg",
        "zhangxiaojun" => "Podcasts221/v4/e6/74/44/e674449c-c2ac-50c4-0099-7e37c330e6c5/mza_16442776193352459849.png",
        "42zhangjing" => "Podcasts126/v4/33/6c/3f/336c3fea-4e5a-9524-6b7d-92c1080a6db9/mza_9247514814536235437.png",
        "latetalk" => "Podcasts211/v4/79/f3/66/79f366d3-dfb2-4c6e-ade6-e95e18261c4f/mza_6811336372335437524.jpg",
        "crossing" => "Podcasts211/v4/d8/14/eb/d814eb12-0a75-e0b4-33a1-c6a2dae2b516/mza_4023160414676912809.png",
        "aialchemy" => "Podcasts116/v4/ad/47/1f/ad471f3e-01ad-b68e-772e-6904f6af6941/mza_15665975703352948829.jpeg",
        "35huan" => "Podcasts126/v4/6e/d0/de/6ed0dedb-d782-a562-e40b-043ce20441d3/mza_5066303992234991879.jpeg",
        "zhixing" => "Podcasts112/v4/b3/96/fc/b396fcc5-6e2b-d34e-8d3d-c849c43b6215/mza_4090324763067558601.jpeg",
        "mianji" => "Podcasts126/v4/a8/d9/5e/a8d95e6e-aba6-3608-9c28-8b8cde840ec4/mza_10150483960934463526.png",
        "nexttoken" => return Some("https://image.xyzcdn.net/FhuQBR26B-8jsdfuZumu54mCylqG.png@small".into()),
        "bannatie" => "Podcasts112/v4/95/33/4d/95334de1-1492-1ab0-aa7b-8f246bf89bde/mza_10728614257690800956.jpeg",
        "latentspace" => "Podcasts221/v4/5c/7d/2a/5c7d2ade-4458-04e8-9838-979d187712ed/mza_16100858243175777898.jpg",
        "podscribe-acquired" => "Podcasts211/v4/a8/e1/de/a8e1deff-9f88-4e55-a541-b0dc793c0cdc/mza_11539673419613154037.jpg",
        "podscribe-pivot" => "Podcasts221/v4/13/22/39/1322397a-976e-3c8f-7764-91cc38d0dd70/mza_12130189257448841692.jpeg",
        "podscribe-freakonomics-radio" => "Podcasts115/v4/f7/0c/c5/f70cc540-ce36-d96f-b111-c970aad5505c/mza_17703422762227531425.jpg",
        "podscribe-invest-like-the-best" => "Podcasts211/v4/61/ae/be/61aebe7a-06e8-7390-3ae5-f2fc5889e36c/mza_10827489189939068066.jpg",
        "podscribe-the-diary-of-a-ceo-with-steven-bartlett" => "Podcasts221/v4/69/af/0d/69af0ddd-1e0f-7ae3-c84b-88f18e31ad0c/mza_14445920128472365296.png",
        _ => return None,
    };
    Some(format!("https://is1-ssl.mzstatic.com/image/thumb/{path}/600x600bb.jpg"))
}

/// The show's own name, so the app matches it with its curated Chinese name and cover.
fn title_for(id: &str) -> Option<&'static str> {
    Some(match id {
        "lex" => "Lex Fridman Podcast", "allin" => "All-In Podcast", "nopriors" => "No Priors", "cogrev" => "The Cognitive Revolution",
        "trainingdata" => "Training Data", "unsupervised" => "Unsupervised Learning", "zhangxiaojun" => "张小珺Jùn｜商业访谈录",
        "42zhangjing" => "42章经", "latetalk" => "晚点聊 LateTalk", "crossing" => "十字路口Crossing", "aialchemy" => "AI炼金术",
        "35huan" => "三五环", "zhixing" => "知行小酒馆", "mianji" => "面基",
        "nexttoken" => "Next Token｜词元之外", "bannatie" => "半拿铁 | 商业沉浮录", "latentspace" => "Latent Space",
        "podscribe-acquired" => "Acquired", "podscribe-pivot" => "Pivot", "podscribe-freakonomics-radio" => "Freakonomics Radio",
        "podscribe-invest-like-the-best" => "Invest Like the Best", "podscribe-the-diary-of-a-ceo-with-steven-bartlett" => "The Diary Of A CEO",
        _ => return None,
    })
}

pub fn feed_url(id: &str) -> String { format!("https://{id}.qiaomu.ai/") }

fn meta(html: &str, prop: &str) -> Option<String> {
    let re = Regex::new(&format!(r#"<meta[^>]+(?:property|name)="{prop}"[^>]+content="([^"]*)""#)).ok()?;
    re.captures(html).map(|c| crate::transcript::decode_entities(&c[1])).filter(|s| !s.is_empty())
}

pub struct Channel { pub show: Show, pub episodes: Vec<Episode>, pub zh_titles: Vec<(String, String)>, pub videos_checked: bool }

pub async fn channel(id: &str) -> Result<Channel> {
    let base = feed_url(id);
    let data_url = format!("{base}data/episodes.json");
    let (home, data) = tokio::join!(net::get_text(&base), net::get_text(&data_url));
    let home = home.unwrap_or_default();
    let v: serde_json::Value = serde_json::from_str(&data?).map_err(|_| anyhow!("精读频道数据格式不对"))?;
    let list = v["episodes"].as_array().cloned().unwrap_or_default();
    if list.is_empty() { return Err(anyhow!("这个精读频道还没有节目")); }
    let title = meta(&home, "og:site_name").or_else(|| meta(&home, "og:title"))
        .or_else(|| Regex::new(r"<title>([^<]+)</title>").unwrap().captures(&home).map(|c| crate::transcript::decode_entities(&c[1])))
        .unwrap_or_else(|| format!("{id}.qiaomu.ai"));
    let abs = |u: &str| if u.starts_with("http") { u.to_string() } else { format!("{}{}", base.trim_end_matches('/'), if u.starts_with('/') { u.to_string() } else { format!("/{u}") }) };
    let show_id = short_hash(&base, 16);
    let show = Show {
        id: show_id.clone(),
        kind: "qiaomu".into(),
        title: title_for(id).map(String::from).unwrap_or_else(|| title.split(" · ").next().unwrap_or(&title).trim().to_string()),
        author: Some("乔木精读".into()),
        cover: cover_for(id).or_else(|| meta(&home, "og:image").map(|u| abs(&u))),
        feed_url: base.clone(),
        link: Some(base.clone()),
        description: meta(&home, "description").or_else(|| meta(&home, "og:description")),
        updated_at: now_ms(),
        unplayed: 0,
    };
    let mut zh_titles = vec![];
    let mut episodes: Vec<Episode> = list.iter().take(crate::feeds::MAX_EPISODES).filter_map(|e| {
        let slug = e["s"].as_str()?.to_string();
        let t = e["t"].as_str()?.to_string();
        if let Some(zh) = e["zh"].as_str().filter(|z| !z.is_empty() && *z != t) { zh_titles.push((t.clone(), zh.to_string())); }
        let link = e["u"].as_str().map(abs).unwrap_or_else(|| format!("{base}ep/{slug}/"));
        let published = e["d"].as_str().and_then(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()).and_then(|d| d.and_hms_opt(12, 0, 0)).map(|d| d.and_utc().timestamp_millis());
        let video = e["y"].as_str().filter(|y| y.len() == 11).map(String::from);
        Some(Episode {
            id: short_hash(&format!("{show_id}:{slug}"), 20),
            show_id: show_id.clone(),
            guid: slug,
            title: t,
            published,
            duration: e["du"].as_f64(),
            audio_url: None,
            cover: e["c"].as_str().map(abs).or_else(|| video.as_ref().map(|y| format!("https://i.ytimg.com/vi/{y}/hqdefault.jpg"))),
            video_id: video,
            link: Some(link),
            notes: e["ds"].as_str().map(String::from),
            transcript_url: None,
            transcript_type: Some("qiaomu".into()),
            deep: true,
            ..Default::default()
        })
    }).collect();
    // Episodes the site has no video for (the Chinese shows, mostly) get one from the show's YouTube channel.
    let videos_checked = crate::curated_video::fill(id, &show.title, &mut episodes).await;
    Ok(Channel { show, episodes, zh_titles, videos_checked })
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Detail { pub audio: Option<String>, pub deep_read: Option<String>, pub transcript: Option<String> }

/// The inner HTML of the first element whose class list contains `class`, tracking nested <div>s / <article>s.
fn inner_html(html: &str, class: &str) -> Option<String> {
    let open = Regex::new(&format!(r#"<(div|article|section|pre)[^>]*class="[^"]*\b{}\b[^"]*"[^>]*>"#, regex::escape(class))).ok()?;
    let m = open.captures(html)?;
    let tag = m[1].to_string();
    let start = m.get(0)?.end();
    let tok = Regex::new(&format!(r"(?i)<(/?){tag}\b[^>]*>")).ok()?;
    let mut depth = 1;
    for t in tok.captures_iter(&html[start..]) {
        depth += if t[1].is_empty() { 1 } else { -1 };
        if depth == 0 { return Some(html[start..start + t.get(0)?.start()].to_string()); }
    }
    None
}

pub async fn detail(link: &str) -> Result<Detail> {
    let html = net::get_text(link).await?;
    let audio = Regex::new(r#"<audio[^>]+src="([^"]+)""#).unwrap().captures(&html).map(|c| crate::transcript::decode_entities(&c[1]))
        .or_else(|| Regex::new(r#"(?:src|href)="(https://[^"]+\.(?:mp3|m4a)[^"]*)""#).unwrap().captures(&html).map(|c| crate::transcript::decode_entities(&c[1])));
    let deep_read = inner_html(&html, "zh-article").map(|h| {
        // The site's own note about AI rewriting is shown by the app; drop scripts and the inline note.
        let h = Regex::new(r"(?is)<script.*?</script>|<style.*?</style>").unwrap().replace_all(&h, "");
        Regex::new(r#"(?is)<p[^>]*class="zh-note"[^>]*>.*?</p>"#).unwrap().replace_all(&h, "").trim().to_string()
    }).filter(|h| h.len() > 40);
    let transcript = inner_html(&html, "transcript-text").map(|t| {
        let t = Regex::new(r"(?s)<[^>]+>").unwrap().replace_all(&t, "");
        crate::transcript::decode_entities(t.trim())
    }).filter(|t| t.len() > 200);
    Ok(Detail { audio, deep_read, transcript })
}

/// Turns an untimed transcript into segments spread across the episode. Header lines ("Machine transcription by…")
/// are dropped. Timing is approximate, so the source is marked as such.
pub fn transcript_segments(text: &str, duration: f64) -> Vec<Segment> {
    let body: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let skip = body.iter().take(3).take_while(|l| l.len() < 160 && (l.contains("Podcast") || l.contains("transcri") || l.contains('#') || l.contains("May contain"))).count();
    let joined = body[skip..].join(" ");
    if joined.is_empty() { return vec![]; }
    crate::transcript::split_timed(&joined, 0.0, duration.max(60.0))
}

// ---- rss.qiaomu.ai sources (乔木改写) ---------------------------------------------------------------
// Feeds the reader follows (Next Token, 半拿铁, Podscribe transcripts of Acquired / Pivot…) where each episode
// may carry a 乔木改写: a Chinese deep-read written from the full transcript.

const RSS: &str = "https://rss.qiaomu.ai";

/// `https://rss.qiaomu.ai/sources/nexttoken` → `nexttoken`.
pub fn rss_source_id(url: &str) -> Option<String> {
    Regex::new(r"^https?://rss\.qiaomu\.ai/sources/([A-Za-z0-9_-]+)/?$").unwrap().captures(url.trim()).map(|c| c[1].to_string())
}
pub fn rss_feed_url(id: &str) -> String { format!("{RSS}/sources/{id}") }

fn norm_title(t: &str) -> String {
    t.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect()
}

/// rss.qiaomu.ai sources that also post every episode to a YouTube channel: source id → channel id.
/// Videos are matched to episodes by the number both titles carry ("#010 …" ↔ "EP010 …").
fn youtube_channel(id: &str) -> Option<&'static str> {
    Some(match id {
        "nexttoken" => "UC_9pr_usdNG29RauPFwIFQw",
        _ => return None,
    })
}

struct YtVideo { id: String, num: Option<u32> }

/// The channel's recent videos from its public feed (no API key, no yt-dlp needed).
async fn youtube_videos(channel_id: &str) -> Vec<YtVideo> {
    let Ok(xml) = net::get_text(&format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}")).await else { return vec![] };
    Regex::new(r"(?s)<yt:videoId>\s*([^<]+?)\s*</yt:videoId>.*?<title>([^<]*)</title>").unwrap()
        .captures_iter(&xml).map(|c| YtVideo { id: c[1].to_string(), num: episode_num(&c[2]) }).collect()
}

/// The episode number both the audio feed ("#010 …") and the video title ("EP010 …") carry.
fn episode_num(title: &str) -> Option<u32> {
    Regex::new(r"(?i)(?:\bEP\s*0*|#\s*0*)(\d{1,4})\b").unwrap().captures(title)?.get(1)?.as_str().parse().ok()
}

/// The show's own feed for each Podscribe source (search by name finds the wrong show for some).
fn original_feed(id: &str) -> Option<&'static str> {
    Some(match id {
        "podscribe-acquired" => "https://feeds.transistor.fm/acquired",
        "podscribe-pivot" => "https://feeds.megaphone.fm/pivot",
        "podscribe-freakonomics-radio" => "https://feeds.simplecast.com/Y8lFbOT4",
        "podscribe-invest-like-the-best" => "https://feeds.megaphone.fm/CLS2859450455",
        "podscribe-the-diary-of-a-ceo-with-steven-bartlett" => "https://rss2.flightcast.com/xmsftuzjjykcmqwolaqn6mdn",
        _ => return None,
    })
}

pub struct Borrowable { pub title: String, pub published: Option<i64>, pub audio: String, pub duration: Option<f64> }

fn words(t: &str) -> std::collections::HashSet<String> {
    t.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 3).map(String::from).collect()
}

fn overlap(a: &str, b: &str) -> f64 {
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() { return 0.0; }
    a.intersection(&b).count() as f64 / a.len().min(b.len()) as f64
}

/// The feed episode a Podscribe entry is a transcript of. Titles often differ (Podscribe sometimes carries the
/// YouTube title), so an exact title wins, then a similar title published within about a day, then the only episode
/// published within half a day, then a near-identical title from any date.
pub fn find_audio<'a>(pool: &'a [Borrowable], title: &str, published: Option<i64>) -> Option<&'a Borrowable> {
    let key = norm_title(title);
    if let Some(b) = pool.iter().find(|b| norm_title(&b.title) == key) { return Some(b); }
    const HOUR: i64 = 3600 * 1000;
    let best = |c: Vec<&'a Borrowable>| c.into_iter().map(|b| (overlap(title, &b.title), b)).max_by(|x, y| x.0.total_cmp(&y.0));
    if let Some(p) = published {
        let within = |h: i64| pool.iter().filter(|b| b.published.is_some_and(|q| (q - p).abs() <= h * HOUR)).collect::<Vec<_>>();
        if let Some((score, b)) = best(within(36)) { if score >= 0.3 { return Some(b); } }
        if let [only] = within(12)[..] { return Some(only); }
    }
    best(pool.iter().collect()).filter(|(s, _)| *s >= 0.8).map(|(_, b)| b)
}

/// Podscribe episodes have transcripts but no audio: borrow it from the show's own feed, its whole archive included.
pub async fn borrowable(id: &str, show_name: &str) -> Vec<Borrowable> {
    let feed = match original_feed(id) {
        Some(f) => f.to_string(),
        None => match crate::feeds::search_podcasts(show_name, "US").await.ok().and_then(|r| r.into_iter().next()) { Some(r) => r.feed_url, None => return vec![] },
    };
    let Ok(xml) = net::get_text(&feed).await else { return vec![] };
    let Ok((_, eps)) = crate::feeds::parse_feed_limit(&xml, &feed, usize::MAX) else { return vec![] };
    eps.into_iter().filter_map(|e| Some(Borrowable { audio: e.audio_url?, title: e.title, published: e.published, duration: e.duration })).collect()
}

pub async fn rss_channel(id: &str) -> Result<Channel> {
    let podscribe = id.starts_with("podscribe-");
    // Everything independent runs at once: the channel list, the entry pages, the original feed (podscribe audio)
    // and the YouTube uploads. Sequentially this took 5–10 s on the larger channels.
    let meta_f = async {
        let sources = net::get_json(&format!("{RSS}/api/sources")).await?;
        sources["sources"].as_array().and_then(|a| a.iter().find(|s| s["id"].as_str() == Some(id)).cloned())
            .ok_or_else(|| anyhow!("rss.qiaomu.ai 上没有这个频道"))
    };
    let pages_f = async {
        let mut entries: Vec<serde_json::Value> = vec![];
        let mut cursor = String::new();
        for _ in 0..3 {
            let url = format!("{RSS}/api/sources/{id}/entries?limit=100{}", if cursor.is_empty() { String::new() } else { format!("&cursor={}", urlencoding::encode(&cursor)) });
            let page = net::get_json(&url).await?;
            entries.extend(page["entries"].as_array().cloned().unwrap_or_default());
            if entries.len() >= crate::feeds::MAX_EPISODES { break; }
            match page["nextCursor"].as_str() { Some(c) if page["hasMore"].as_bool() == Some(true) && !c.is_empty() => cursor = c.to_string(), _ => break }
        }
        Ok::<_, anyhow::Error>(entries)
    };
    let borrow_f = async { if podscribe && original_feed(id).is_some() { borrowable(id, "").await } else { vec![] } };
    let yt_f = async { match youtube_channel(id) { Some(ch) => youtube_videos(ch).await, None => vec![] } };
    let (meta, entries, mut borrowed, yt) = tokio::join!(meta_f, pages_f, borrow_f, yt_f);
    let (meta, entries) = (meta?, entries?);
    let name = meta["name"].as_str().unwrap_or(id).replace(" · 文稿", "");
    if entries.is_empty() { return Err(anyhow!("这个频道还没有节目")); }
    if podscribe && original_feed(id).is_none() { borrowed = borrowable(id, &name).await; }
    let feed = rss_feed_url(id);
    let show_id = short_hash(&feed, 16);
    let mut zh_titles = vec![];
    let mut episodes: Vec<Episode> = entries.iter().take(crate::feeds::MAX_EPISODES).filter_map(|e| {
        let guid = e["id"].as_str()?.to_string();
        let title = e["title"].as_str()?.to_string();
        if let Some(zh) = e["titleZh"].as_str().filter(|z| !z.is_empty() && *z != title) { zh_titles.push((title.clone(), zh.to_string())); }
        let link = e["link"].as_str().map(String::from);
        let (mut audio, mut duration) = (e["audio"]["url"].as_str().map(String::from), None);
        if audio.is_none() { if let Some(b) = find_audio(&borrowed, &title, e["publishedTs"].as_i64()) { audio = Some(b.audio.clone()); duration = b.duration; } }
        let video = link.as_deref().and_then(crate::feeds::youtube_video_id)
            .or_else(|| episode_num(&title).and_then(|n| yt.iter().find(|v| v.num == Some(n))).map(|v| v.id.clone()));
        // Some sources mix in written posts (Latent Space's AINews); without a transcript there is nothing to play or follow.
        if audio.is_none() && video.is_none() && !podscribe { return None; }
        let transcript_url = if podscribe { link.as_deref().and_then(|l| l.strip_prefix("https://podcasts.happyscribe.com/")).map(|p| format!("https://api.qiaomu.ai/podscribe/v1/episodes/{p}/transcript")) } else { None };
        Some(Episode {
            id: short_hash(&format!("{show_id}:{guid}"), 20),
            show_id: show_id.clone(),
            guid,
            title,
            published: e["publishedTs"].as_i64(),
            duration,
            audio_url: audio,
            video_id: video.clone(),
            link,
            cover: e["image"].as_str().map(String::from).or_else(|| video.map(|v| format!("https://i.ytimg.com/vi/{v}/hqdefault.jpg"))),
            notes: e["summary"].as_str().map(String::from),
            transcript_url,
            transcript_type: Some(if podscribe { "podscribe" } else { "qiaomu-rss" }.into()),
            deep: e["rewriteReady"].as_bool().unwrap_or(false),
            ..Default::default()
        })
    }).collect();
    let show = Show {
        id: show_id,
        kind: "qiaomu".into(),
        title: title_for(id).map(String::from).unwrap_or(name),
        author: Some("乔木精读".into()),
        cover: cover_for(id).or_else(|| episodes.iter().find_map(|e| e.cover.clone())),
        feed_url: feed.clone(),
        link: meta["siteUrl"].as_str().map(String::from),
        description: meta["description"].as_str().map(String::from),
        updated_at: now_ms(),
        unplayed: 0,
    };
    let videos_checked = crate::curated_video::fill(id, &show.title, &mut episodes).await;
    Ok(Channel { show, episodes, zh_titles, videos_checked })
}

/// The 乔木改写 for an rss.qiaomu.ai entry, as HTML.
pub async fn rss_rewrite(entry_id: &str) -> Result<Option<String>> {
    let v: serde_json::Value = net::get_json(&format!("{RSS}/api/entry/{entry_id}/rewrite")).await?;
    if v["status"].as_str() != Some("ready") { return Ok(None); }
    let html = v["rewrite"]["bodyHtml"].as_str().filter(|h| !h.is_empty()).map(String::from)
        .or_else(|| v["rewrite"]["body"].as_str().map(markdown_html));
    Ok(html.filter(|h| h.len() > 40))
}

fn esc(s: &str) -> String { s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;") }

fn inline(s: &str) -> String {
    let s = Regex::new(r"!\[[^\]]*\]\([^)]*\)").unwrap().replace_all(s, "");
    let s = esc(&s);
    let s = Regex::new(r"\*\*(.+?)\*\*").unwrap().replace_all(&s, "<strong>$1</strong>");
    let s = Regex::new(r"\[([^\]]+)\]\((https?://[^)\s]+)\)").unwrap().replace_all(&s, r#"<a href="$2">$1</a>"#);
    Regex::new(r"`([^`]+)`").unwrap().replace_all(&s, "<code>$1</code>").into_owned()
}

/// Enough Markdown for the rewrites: headings, quotes, lists, paragraphs, bold, links.
pub fn markdown_html(md: &str) -> String {
    let mut out = String::new();
    let mut list = false;
    for block in md.split("\n\n").map(str::trim).filter(|b| !b.is_empty()) {
        let lines: Vec<&str> = block.lines().collect();
        if lines.iter().all(|l| l.trim_start().starts_with("- ") || l.trim_start().starts_with("* ")) {
            if !list { out.push_str("<ul>"); list = true; }
            for l in lines { out.push_str(&format!("<li>{}</li>", inline(l.trim_start()[2..].trim()))); }
            continue;
        }
        if list { out.push_str("</ul>"); list = false; }
        if let Some(h) = block.strip_prefix("### ") { out.push_str(&format!("<h3>{}</h3>", inline(h))); }
        else if let Some(h) = block.strip_prefix("## ") { out.push_str(&format!("<h2>{}</h2>", inline(h))); }
        else if let Some(h) = block.strip_prefix("# ") { out.push_str(&format!("<h2>{}</h2>", inline(h))); }
        else if block.starts_with('>') {
            let q: Vec<String> = lines.iter().map(|l| inline(l.trim_start_matches('>').trim())).collect();
            out.push_str(&format!("<blockquote><p>{}</p></blockquote>", q.join("<br>")));
        } else { out.push_str(&format!("<p>{}</p>", lines.iter().map(|l| inline(l)).collect::<Vec<_>>().join("<br>"))); }
    }
    if list { out.push_str("</ul>"); }
    out
}

/// A Podscribe transcript with real timestamps.
pub async fn podscribe_transcript(url: &str) -> Result<Vec<Segment>> {
    let v: serde_json::Value = net::get_json(&url).await?;
    let segs = v["data"]["transcript"]["segments"].as_array().cloned().unwrap_or_default();
    let mut out: Vec<Segment> = segs.iter().filter_map(|s| {
        let text = s["text"].as_str()?.trim().to_string();
        (!text.is_empty()).then(|| Segment { start: s["start_seconds"].as_f64().unwrap_or(0.0), end: 0.0, text })
    }).collect();
    for i in 0..out.len() { out[i].end = out.get(i + 1).map(|n| n.start).unwrap_or(out[i].start + 8.0); }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network: how long the big 精读 channels take to load.
    #[tokio::test]
    #[ignore]
    async fn rss_channel_timing() {
        for id in ["podscribe-acquired", "latentspace", "nexttoken"] {
            let t = std::time::Instant::now();
            let ch = rss_channel(id).await.unwrap();
            println!("{id}: {} episodes in {:?}", ch.episodes.len(), t.elapsed());
        }
    }
    const D: i64 = 24 * 3600 * 1000;
    fn b(title: &str, day: i64) -> Borrowable { Borrowable { title: title.into(), published: Some(day * D), audio: format!("a:{title}"), duration: None } }

    #[test]
    fn podscribe_audio_matching() {
        let pool = vec![
            b("Katt Williams: I Lied My Way Into Comedy! List The 300 People Ahead Of You", 100),
            b("What Are The Five Lifestyle Habits Destroying Your Brain Right Now?", 103),
            b("Most Replayed Moment: The Childhood Scripts Still Running Your Life! Chase Hughes", 104),
            b("Ex-CIA Officer: Trump Knew About The China Spy", 107),
            b("Prof Robert Pape: Are We Entering The Eight Most Dangerous Weeks", 96),
        ];
        let at = |t: &str, day: i64| find_audio(&pool, t, Some(day * D)).map(|x| x.title.as_str());
        // Exact title, regardless of punctuation.
        assert_eq!(at("most replayed moment - the childhood scripts still running your life chase hughes", 1), Some(pool[2].title.as_str()));
        // A YouTube-style title on the same day as the only feed episode then.
        assert_eq!(at("Top Neurologist: If You Drink Even Once A Week, Watch This Now!", 103), Some(pool[1].title.as_str()));
        // Different title, same day, shared words.
        assert_eq!(at("Katt Williams: From Homeless Runaway at 13 to Comedy Icon", 100), Some(pool[0].title.as_str()));
        // Nothing near in time or title.
        assert_eq!(at("Completely Unrelated Episode About Gardening", 200), None);
        // No date: only a near-identical title counts.
        assert_eq!(find_audio(&pool, "Ex-CIA Officer: Trump Knew About The China Spy!", None).map(|x| x.title.as_str()), Some(pool[3].title.as_str()));
        assert!(find_audio(&pool, "The China Question", None).is_none());
    }

    #[test]
    fn episode_numbers() {
        assert_eq!(episode_num("#010 黄仁勋辞职，Fable 杀死比赛，语言即世界"), Some(10));
        assert_eq!(episode_num("EP010 Jensen Huang Steps Down, Fable Blows Rivals Away"), Some(10));
        assert_eq!(episode_num("Weekly #001 大模型进入“斩杀线”大战"), Some(1));
        assert_eq!(episode_num("#3 没有前导零"), Some(3));
        assert_eq!(episode_num("没有编号的一期节目 2024"), None);
    }

    #[test]
    fn markdown_drops_images() {
        let h = markdown_html("![No.225 莆田鞋往事](https://image.xyzcdn.net/x.jpeg)\n\n正文 **重点** [链接](https://a.b/c)");
        assert!(!h.contains('!') && !h.contains("xyzcdn"), "{h}");
        assert!(h.contains("<strong>重点</strong>") && h.contains(r#"<a href="https://a.b/c">链接</a>"#), "{h}");
    }
}
