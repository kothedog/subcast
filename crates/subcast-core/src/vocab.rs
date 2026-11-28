//! Word book, FSRS scheduling, ECDICT lookup, CSV & Anki export — 100% schema-compatible
//! with `src/lib/words.ts` (`learn/words` JSON blob).
use crate::{anki, ctx::CoreCtx, db, ops};
use anyhow::{anyhow, Result};
use chrono::Local;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

const FSRS_W: [f64; 19] = [
    0.40255, 1.18385, 3.173, 15.69105, 7.1949, 0.5345, 1.4604, 0.0046, 1.54575, 0.1192, 1.01925,
    1.9395, 0.11, 0.29605, 2.2698, 0.2315, 2.9898, 0.51655, 0.6621,
];
const REQUEST_RETENTION: f64 = 0.9;
const MAX_INTERVAL_DAYS: u32 = 36_500;
const DECAY: f64 = -0.5;
const FACTOR: f64 = 19.0 / 81.0; // (0.9^(-1/0.5) - 1) = 19/81 for FSRS-4.5/5
const MASTERED_DAYS: u32 = 21;

/// `SavedCard` uses exact snake_case matching `src/lib/words.ts`!
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct SavedCard {
    pub due: i64,
    pub stability: f64,
    pub difficulty: f64,
    pub scheduled_days: u32,
    pub learning_steps: u32,
    pub reps: u32,
    pub lapses: u32,
    /// 0 = New, 1 = Learning, 2 = Review, 3 = Relearning
    pub state: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_review: Option<i64>,
}

/// `Word` uses camelCase matching `src/lib/words.ts`!
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Word {
    pub id: String,
    pub word: String,
    #[serde(default)]
    pub ipa: String,
    #[serde(default)]
    pub meaning: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ctx: Option<String>,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub sentence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentence_zh: Option<String>,
    #[serde(default)]
    pub episode_id: String,
    #[serde(default)]
    pub episode_title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_title: Option<String>,
    #[serde(default)]
    pub start: f64,
    #[serde(default)]
    pub end: f64,
    pub added: i64,
    pub card: SavedCard,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct DayLog {
    pub r: u32,
    pub a: u32,
    #[serde(default)]
    pub ids: Vec<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Book {
    #[serde(default = "default_v")]
    pub v: u32,
    #[serde(default)]
    pub words: HashMap<String, Word>,
    #[serde(default = "today_str")]
    pub day: String,
    #[serde(default)]
    pub new_today: u32,
    #[serde(default)]
    pub log: HashMap<String, DayLog>,
    #[serde(default)]
    pub known: HashMap<String, i64>,
}

fn default_v() -> u32 { 1 }
pub fn today_str() -> String { Local::now().format("%Y-%m-%d").to_string() }
pub fn day_of_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&Local).format("%Y-%m-%d").to_string())
        .unwrap_or_else(today_str)
}

impl Default for Book {
    fn default() -> Self {
        Self { v: 1, words: HashMap::new(), day: today_str(), new_today: 0, log: HashMap::new(), known: HashMap::new() }
    }
}

pub fn create_empty_card(now_ms: i64) -> SavedCard {
    SavedCard {
        due: now_ms,
        stability: 0.0,
        difficulty: 0.0,
        scheduled_days: 0,
        learning_steps: 0,
        reps: 0,
        lapses: 0,
        state: 0,
        last_review: None,
    }
}

fn clamp_d(d: f64) -> f64 { d.clamp(1.0, 10.0) }

fn init_stability(g: u8) -> f64 { FSRS_W[(g as usize).saturating_sub(1).min(3)].max(0.1) }

fn init_difficulty(g: u8) -> f64 {
    clamp_d(FSRS_W[4] - (FSRS_W[5] * (g as f64 - 1.0)).exp() + 1.0)
}

fn next_interval(s: f64) -> u32 {
    let raw = (s / FACTOR) * (REQUEST_RETENTION.powf(1.0 / DECAY) - 1.0);
    raw.round().clamp(1.0, MAX_INTERVAL_DAYS as f64) as u32
}

fn next_difficulty(d: f64, g: u8) -> f64 {
    let delta = -FSRS_W[6] * (g as f64 - 3.0);
    let next = d + delta * (10.0 - d) / 9.0;
    let d0_4 = init_difficulty(4);
    clamp_d(FSRS_W[7] * d0_4 + (1.0 - FSRS_W[7]) * next)
}

fn next_recall_stability(d: f64, s: f64, r: f64, g: u8) -> f64 {
    let hard_penalty = if g == 2 { FSRS_W[15] } else { 1.0 };
    let easy_bonus = if g == 4 { FSRS_W[16] } else { 1.0 };
    s * ((FSRS_W[8].exp() * (11.0 - d) * s.powf(-FSRS_W[9]) * ((FSRS_W[10] * (1.0 - r)).exp() - 1.0) * hard_penalty * easy_bonus) + 1.0)
}

fn next_forget_stability(d: f64, s: f64, r: f64) -> f64 {
    (FSRS_W[11] * d.powf(-FSRS_W[12]) * ((s + 1.0).powf(FSRS_W[13]) - 1.0) * (FSRS_W[14] * (1.0 - r)).exp()).max(0.1).min(s)
}

/// FSRS-5 card scheduler (`grade`: 1=Again, 2=Hard, 3=Good, 4=Easy).
pub fn schedule_next(card: &SavedCard, now_ms: i64, grade: u8) -> SavedCard {
    let g = grade.clamp(1, 4);
    let elapsed_days = card
        .last_review
        .map(|lr| ((now_ms - lr).max(0) as f64) / 86_400_000.0)
        .unwrap_or(0.0);
    let mut next = card.clone();
    next.reps += 1;
    next.last_review = Some(now_ms);

    match card.state {
        0 => {
            next.difficulty = init_difficulty(g);
            next.stability = init_stability(g);
            match g {
                1 => { next.state = 1; next.learning_steps = 0; next.scheduled_days = 0; next.due = now_ms + 60_000; }
                2 => { next.state = 1; next.learning_steps = 1; next.scheduled_days = 0; next.due = now_ms + 6 * 60_000; }
                3 => { next.state = 1; next.learning_steps = 1; next.scheduled_days = 0; next.due = now_ms + 10 * 60_000; }
                _ => { let ivl = next_interval(next.stability); next.state = 2; next.scheduled_days = ivl; next.due = now_ms + (ivl as i64) * 86_400_000; }
            }
        }
        1 | 3 => {
            let s = if card.stability > 0.0 { card.stability } else { init_stability(g) };
            let d = if card.difficulty > 0.0 { card.difficulty } else { init_difficulty(g) };
            next.difficulty = next_difficulty(d, g);
            next.stability = if elapsed_days < 1.0 {
                (s * (FSRS_W[17] * (g as f64 - 3.0 + FSRS_W[18])).exp()).max(0.1)
            } else {
                let r = (1.0 + FACTOR * elapsed_days / s).powf(DECAY);
                if g == 1 { next_forget_stability(next.difficulty, s, r) } else { next_recall_stability(next.difficulty, s, r, g) }
            };
            match g {
                1 => { next.learning_steps = 0; next.scheduled_days = 0; next.due = now_ms + 5 * 60_000; }
                2 => { next.learning_steps = card.learning_steps; next.scheduled_days = 0; next.due = now_ms + 10 * 60_000; }
                _ => {
                    let ivl = next_interval(next.stability);
                    next.state = 2;
                    next.learning_steps = 0;
                    next.scheduled_days = ivl;
                    next.due = now_ms + (ivl as i64) * 86_400_000;
                }
            }
        }
        _ => {
            let s = card.stability.max(0.1);
            let d = card.difficulty.clamp(1.0, 10.0);
            let r = (1.0 + FACTOR * elapsed_days.max(0.0) / s).powf(DECAY);
            next.difficulty = next_difficulty(d, g);
            if g == 1 {
                next.lapses += 1;
                next.stability = next_forget_stability(next.difficulty, s, r);
                next.state = 3;
                next.learning_steps = 0;
                next.scheduled_days = 0;
                next.due = now_ms + 5 * 60_000;
            } else {
                next.stability = next_recall_stability(next.difficulty, s, r, g);
                let ivl = next_interval(next.stability);
                next.state = 2;
                next.scheduled_days = ivl;
                next.due = now_ms + (ivl as i64) * 86_400_000;
            }
        }
    }
    next
}

pub fn is_mastered(w: &Word) -> bool {
    w.card.state == 2 && w.card.scheduled_days >= MASTERED_DAYS
}

pub fn is_due(w: &Word, now_ms: i64) -> bool {
    w.card.state != 0 && w.card.due <= now_ms
}

pub fn review_queue(book: &Book, daily_new: u32, now_ms: i64) -> Vec<Word> {
    let mut due: Vec<Word> = book.words.values().filter(|w| is_due(w, now_ms)).cloned().collect();
    due.sort_by_key(|w| w.card.due);
    let today = today_str();
    let introduced = if book.day == today { book.new_today } else { 0 };
    let left = daily_new.saturating_sub(introduced) as usize;
    let mut fresh: Vec<Word> = book.words.values().filter(|w| w.card.state == 0).cloned().collect();
    fresh.sort_by_key(|w| w.added);
    fresh.truncate(left);
    due.extend(fresh);
    due
}

pub fn load_book(ctx: &CoreCtx) -> Result<Book> {
    let raw = db::get_blob(&ctx.db.lock(), "learn", "words")?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

pub fn mutate_book<F, R>(ctx: &CoreCtx, f: F) -> Result<R>
where
    F: FnOnce(&mut Book) -> Result<R>,
{
    let mut conn = ctx.db.lock();
    let mut out: Option<R> = None;
    db::update_blob(&mut conn, "learn", "words", |cur| {
        let mut book: Book = cur.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        out = Some(f(&mut book)?);
        Ok(serde_json::to_string(&book)?)
    })?;
    Ok(out.unwrap())
}

pub fn rate_word(ctx: &CoreCtx, id: &str, grade: u8, now_ms: i64) -> Result<Word> {
    let key = id.trim().to_lowercase();
    mutate_book(ctx, |book| {
        let d = today_str();
        let w = book.words.get_mut(&key).ok_or_else(|| anyhow!("生词本里没有：{id}"))?; // i18n-ignore
        let was_new = w.card.state == 0;
        w.card = schedule_next(&w.card, now_ms, grade);
        let updated = w.clone();
        let prev_day = std::mem::replace(&mut book.day, d.clone());
        if prev_day != d {
            book.new_today = 0;
        }
        if was_new {
            book.new_today += 1;
        }
        let log = book.log.entry(d).or_default();
        log.r += 1;
        if grade == 1 {
            log.a += 1;
        }
        if !log.ids.contains(&key) {
            log.ids.push(key);
        }
        Ok(updated)
    })
}

pub fn parse_rating(s: &str) -> Option<u8> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "again" | "forgot" => Some(1),
        "2" | "hard" => Some(2),
        "3" | "good" | "ok" => Some(3),
        "4" | "easy" => Some(4),
        _ => None,
    }
}

pub fn to_csv(words: &[Word]) -> String {
    let cell = |s: &str| {
        if s.contains([',', '"', '\n']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_string()
        }
    };
    let mut rows = vec!["Word,IPA,Meaning,Context,Sentence,Translation,Show,Episode,Time,Added".to_string()];
    for w in words {
        let sec = w.start.max(0.0).floor() as u64;
        let at = format!("{}:{:02}", sec / 60, sec % 60);
        let added = day_of_ms(w.added);
        rows.push(
            [
                &w.word,
                &w.ipa,
                &w.meaning.replace('\n', " "),
                w.ctx.as_deref().unwrap_or(""),
                &w.sentence,
                w.sentence_zh.as_deref().unwrap_or(""),
                w.show_title.as_deref().unwrap_or(""),
                &w.episode_title,
                &at,
                &added,
            ]
            .into_iter()
            .map(cell)
            .collect::<Vec<_>>()
            .join(","),
        );
    }
    format!("\u{FEFF}{}\n", rows.join("\n"))
}

// ---- ECDICT offline dictionary lookup ------------------------------------------------------------

#[derive(Deserialize)]
struct DictFile {
    #[serde(default)]
    words: HashMap<String, (String, String, u32, u32)>,
    #[serde(default)]
    forms: HashMap<String, String>,
}

pub struct Dict(DictFile);

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DictEntry {
    pub lemma: String,
    pub ipa: String,
    pub meaning: String,
    pub tags: u32,
    pub tag_label: String,
    pub rank: u32,
}

const TAG_NAMES: &[(u32, &str)] = &[
    (1 << 0, "中考"), // i18n-ignore
    (1 << 1, "高考"), // i18n-ignore
    (1 << 2, "四级"), // i18n-ignore
    (1 << 3, "六级"), // i18n-ignore
    (1 << 4, "考研"), // i18n-ignore
    (1 << 5, "托福"), // i18n-ignore
    (1 << 6, "雅思"), // i18n-ignore
    (1 << 7, "GRE"),
];

pub fn tag_label(tags: u32) -> &'static str {
    for &(bit, label) in TAG_NAMES {
        if (tags & bit) != 0 {
            return label;
        }
    }
    ""
}

impl Dict {
    pub fn find_path(data: &Path, explicit: Option<&Path>) -> Option<PathBuf> {
        if let Some(p) = explicit.filter(|p| p.is_file()) {
            return Some(p.to_path_buf());
        }
        let mut candidates = vec![data.join("dict/en.json"), PathBuf::from("public/dict/en.json")];
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                candidates.push(dir.join("dict/en.json"));
                candidates.push(dir.join("../Resources/dict/en.json"));
            }
        }
        candidates.into_iter().find(|p| p.is_file())
    }

    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        Ok(Self(serde_json::from_slice(&bytes)?))
    }

    pub fn lemma_of(&self, token: &str) -> Option<String> {
        let mut w = token.to_lowercase().replace(['’', '‘'], "'").trim_matches('\'').to_string();
        if let Some(base) = w.strip_suffix("'s") {
            w = base.to_string();
        }
        if self.0.words.contains_key(&w) {
            return Some(w);
        }
        if let Some(f) = self.0.forms.get(&w) {
            if self.0.words.contains_key(f) {
                return Some(f.clone());
            }
        }
        None
    }

    pub fn lookup(&self, token: &str) -> Option<DictEntry> {
        let lemma = self.lemma_of(token)?;
        let (ipa, meaning, tags, rank) = self.0.words.get(&lemma)?;
        Some(DictEntry {
            lemma,
            ipa: ipa.clone(),
            meaning: meaning.clone(),
            tags: *tags,
            tag_label: tag_label(*tags).to_string(),
            rank: *rank,
        })
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

static WORD_BOUNDARY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[A-Za-z'’-]+").unwrap());

fn highlight_word_html(sentence: &str, target: &str) -> String {
    let mut out = String::new();
    let mut last = 0usize;
    let tl = target.to_lowercase();
    for m in WORD_BOUNDARY_RE.find_iter(sentence) {
        out.push_str(&html_escape(&sentence[last..m.start()]));
        let w = m.as_str();
        if w.to_lowercase() == tl {
            out.push_str(&format!("<b>{}</b>", html_escape(w)));
        } else {
            out.push_str(&html_escape(w));
        }
        last = m.end();
    }
    out.push_str(&html_escape(&sentence[last..]));
    out
}

pub async fn export_anki_from_book(ctx: &CoreCtx, out_path: &Path, deck: &str, model: &str, with_audio: bool) -> Result<usize> {
    let book = load_book(ctx)?;
    let mut words: Vec<Word> = book.words.into_values().collect();
    words.sort_by_key(|w| w.added);
    let mut notes = Vec::with_capacity(words.len());
    for w in words {
        let clip = if with_audio && !w.episode_id.is_empty() && w.end > w.start {
            ops::learn_clip(ctx, &w.episode_id, w.start, w.end).await.ok()
        } else {
            None
        };
        notes.push(anki::Note {
            id: w.id.clone(),
            word: html_escape(&w.word),
            ipa: html_escape(&w.ipa),
            meaning: html_escape(&w.meaning).replace('\n', "<br>"),
            context: w.ctx.as_deref().map(html_escape).unwrap_or_default(),
            sentence: highlight_word_html(&w.sentence, &w.word),
            translation: w.sentence_zh.as_deref().map(html_escape).unwrap_or_default(),
            source: html_escape(&[w.show_title.as_deref().unwrap_or(""), &w.episode_title].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")),
            tags: w.tag.clone(),
            clip,
        });
    }
    let out = out_path.to_path_buf();
    let deck = deck.to_string();
    let model = model.to_string();
    tokio::task::spawn_blocking(move || anki::write(&out, &deck, &model, &notes)).await?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fsrs_progresses_intervals_monotonically_and_matches_desktop_json() {
        let now = 1_700_000_000_000i64;
        let c0 = create_empty_card(now);
        let c_good = schedule_next(&c0, now, 3);
        let c_easy = schedule_next(&c0, now, 4);
        assert!(c_easy.due > c_good.due);

        let w = Word {
            id: "serendipity".into(),
            word: "serendipity".into(),
            ipa: "ˌserənˈdɪpəti".into(),
            meaning: "意外发现珍奇事物的本领".into(),
            ctx: None,
            tag: "GRE".into(),
            sentence: "Pure serendipity brought us here.".into(),
            sentence_zh: Some("纯粹的机缘巧合把我们带到了这里。".into()),
            episode_id: "ep1".into(),
            episode_title: "Ep 1".into(),
            show_title: Some("Show".into()),
            start: 10.0,
            end: 13.0,
            added: now,
            card: c_easy,
        };
        let mut book = Book::default();
        book.words.insert(w.id.clone(), w);
        let json = serde_json::to_value(&book).unwrap();
        assert!(json["newToday"].is_u64());
        assert!(json["words"]["serendipity"]["episodeId"].is_string());
        assert!(json["words"]["serendipity"]["card"]["scheduled_days"].is_u64());
        assert!(to_csv(&book.words.values().cloned().collect::<Vec<_>>()).contains("serendipity"));
    }
}
