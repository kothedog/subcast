//! Sentence splitting and subtitle export (SRT, WebVTT, ASS, LRC, TXT, Markdown), mirrored from
//! `src/lib/pieces.ts` and `src/lib/subexport.ts`.
use crate::align::TimeMap;
use crate::model::Segment;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubFormat {
    Srt,
    Vtt,
    Ass,
    Lrc,
    Txt,
    Md,
}

impl SubFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "srt" => Some(Self::Srt),
            "vtt" | "webvtt" => Some(Self::Vtt),
            "ass" => Some(Self::Ass),
            "lrc" => Some(Self::Lrc),
            "txt" | "text" => Some(Self::Txt),
            "md" | "markdown" => Some(Self::Md),
            _ => None,
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Vtt => "vtt",
            Self::Ass => "ass",
            Self::Lrc => "lrc",
            Self::Txt => "txt",
            Self::Md => "md",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubContent {
    Bilingual,
    Original,
    Translation,
}

impl SubContent {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "bilingual" | "bi" | "both" => Some(Self::Bilingual),
            "original" | "orig" => Some(Self::Original),
            "translation" | "tr" | "zh" => Some(Self::Translation),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Cue {
    pub start: f64,
    pub end: f64,
    pub orig: String,
    pub zh: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ExportMeta {
    pub title: String,
    pub show: Option<String>,
    pub url: Option<String>,
}

pub fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| ('\u{3400}'..='\u{9fff}').contains(&c) || ('\u{f900}'..='\u{faff}').contains(&c))
}

static ABBREV_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(?:[A-Z]\.)+(?:[A-Z]\.?)?$|^(?:Mr|Mrs|Ms|Dr|St|vs|etc|Inc|Co|Jr|Sr|No|Vol)\.$").unwrap()
});
static END_PUNCT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r#"[。！？.!?]["”')）]*$"#).unwrap());
static COMMA_PUNCT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[，；：,;:]$").unwrap());

fn split_cjk_units(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = text.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        cur.push(c);
        let next_is_digit = chars.get(i + 1).is_some_and(|n| n.is_ascii_digit());
        let is_break = matches!(c, '，' | '。' | '！' | '？' | '；' | '：' | '、' | ',' | '!' | '?' | ';')
            || (c == '.' && !next_is_digit);
        if is_break {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn split_ws_units(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in text.chars() {
        cur.push(c);
        if c.is_whitespace() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Splits a subtitle block into reading-sized pieces (mirrors `pieces.ts`).
pub fn pieces(text: &str, cjk: bool) -> Vec<String> {
    let (soft, hard) = if cjk { (14usize, 30usize) } else { (38usize, 80usize) };
    let units = if cjk { split_cjk_units(text) } else { split_ws_units(text) };
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for n in 0..units.len() {
        let u = &units[n];
        let next_start = units.get(n + 1).and_then(|s| s.trim_start().chars().next());
        let continues = !cjk
            && u.trim().ends_with('.')
            && next_start.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
        if !cur.is_empty() && format!("{cur}{u}").trim().chars().count() > hard {
            out.push(cur.trim().to_string());
            cur.clear();
        }
        cur.push_str(u);
        let t = cur.trim();
        let last = u.trim();
        let abbrev = !cjk && ABBREV_RE.is_match(last);
        let t_chars = t.chars().count();
        if (!abbrev && !continues && END_PUNCT_RE.is_match(t))
            || (t_chars >= soft && COMMA_PUNCT_RE.is_match(t))
        {
            out.push(t.to_string());
            cur.clear();
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    if out.is_empty() { vec![text.to_string()] } else { out }
}

const MIN_CUE: f64 = 1.0;

#[derive(Clone, Debug)]
pub struct TimedPiece {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

pub fn timed_pieces(text: &str, cjk: bool, start: f64, end: f64) -> Vec<TimedPiece> {
    let ps = pieces(text, cjk);
    let total = ps.iter().map(|p| p.chars().count()).sum::<usize>().max(1) as f64;
    let mut out: Vec<TimedPiece> = Vec::new();
    let mut acc = 0usize;
    for p in ps {
        let s = start + (end - start) * (acc as f64 / total);
        acc += p.chars().count();
        let e = start + (end - start) * (acc as f64 / total);
        if let Some(last) = out.last_mut() {
            if last.end - last.start < MIN_CUE {
                last.end = e;
                if !cjk {
                    last.text.push(' ');
                }
                last.text.push_str(&p);
                continue;
            }
        }
        out.push(TimedPiece { start: s, end: e, text: p });
    }
    if out.len() > 1 && out.last().is_some_and(|l| l.end - l.start < MIN_CUE) {
        let last = out.pop().unwrap();
        if let Some(prev) = out.last_mut() {
            prev.end = last.end;
            if !cjk {
                prev.text.push(' ');
            }
            prev.text.push_str(&last.text);
        }
    }
    out
}

pub fn build_cues(segments: &[Segment], zh: Option<&[Option<String>]>, content: SubContent) -> Vec<Cue> {
    let mut out = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        let tr = zh.and_then(|arr| arr.get(i)).and_then(|x| x.as_deref()).map(str::trim).filter(|s| !s.is_empty());
        if content == SubContent::Translation {
            if let Some(t) = tr {
                for p in timed_pieces(t, true, seg.start, seg.end) {
                    out.push(Cue { start: p.start, end: p.end, orig: String::new(), zh: Some(p.text) });
                }
                continue;
            }
        }
        let os = timed_pieces(&seg.text, has_cjk(&seg.text), seg.start, seg.end);
        let zs = if content == SubContent::Bilingual {
            tr.map(|t| timed_pieces(t, true, seg.start, seg.end)).unwrap_or_default()
        } else {
            vec![]
        };
        for o in os {
            let inside: Vec<&TimedPiece> = zs
                .iter()
                .filter(|z| {
                    let m = (z.start + z.end) / 2.0;
                    m >= o.start && m < o.end
                })
                .collect();
            let mid = (o.start + o.end) / 2.0;
            let zh_line = if !inside.is_empty() {
                Some(inside.into_iter().map(|z| z.text.as_str()).collect::<String>())
            } else {
                zs.iter()
                    .find(|z| z.start <= mid && mid < z.end)
                    .map(|z| z.text.clone())
            };
            out.push(Cue {
                start: o.start,
                end: o.end,
                orig: o.text,
                zh: zh_line,
            });
        }
    }
    out
}

pub fn build_blocks(segments: &[Segment], zh: Option<&[Option<String>]>, content: SubContent) -> Vec<Cue> {
    segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let tr = zh.and_then(|arr| arr.get(i)).and_then(|x| x.as_deref()).map(str::trim).filter(|s| !s.is_empty());
            match content {
                SubContent::Original => Cue { start: s.start, end: s.end, orig: s.text.clone(), zh: None },
                SubContent::Translation => match tr {
                    Some(t) => Cue { start: s.start, end: s.end, orig: String::new(), zh: Some(t.to_string()) },
                    None => Cue { start: s.start, end: s.end, orig: s.text.clone(), zh: None },
                },
                SubContent::Bilingual => Cue { start: s.start, end: s.end, orig: s.text.clone(), zh: tr.map(String::from) },
            }
        })
        .collect()
}

pub fn on_clock(cues: &[Cue], map: Option<&TimeMap>) -> Vec<Cue> {
    let Some(m) = map else { return cues.to_vec() };
    let mut out: Vec<Cue> = cues
        .iter()
        .map(|c| {
            let start = m.to_play(c.start).max(0.0);
            let end_mapped = m.to_play(c.end).max(0.0);
            let dur = if end_mapped - start > c.end - c.start + 1.0 {
                start + (c.end - c.start)
            } else {
                end_mapped
            };
            Cue { start, end: dur.max(start + 0.2), orig: c.orig.clone(), zh: c.zh.clone() }
        })
        .collect();
    for i in 0..out.len().saturating_sub(1) {
        if out[i].end > out[i + 1].start {
            out[i].end = out[i + 1].start.max(out[i].start + 0.1);
        }
    }
    out
}

fn stamp(t: f64, sep: char) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    let h = ms / 3_600_000;
    let m = (ms / 60_000) % 60;
    let s = (ms / 1_000) % 60;
    let f = ms % 1_000;
    format!("{h:02}:{m:02}:{s:02}{sep}{f:03}")
}

pub fn clock_of(t: f64) -> String {
    let s = t.max(0.0).floor() as u64;
    let h = s / 3600;
    let m = (s / 60) % 60;
    let x = s % 60;
    if h > 0 { format!("{h}:{m:02}:{x:02}") } else { format!("{m:02}:{x:02}") }
}

fn lines_of(c: &Cue) -> Vec<&str> {
    let mut out = Vec::new();
    if let Some(z) = c.zh.as_deref().filter(|s| !s.is_empty()) { out.push(z); }
    if !c.orig.is_empty() { out.push(&c.orig); }
    out
}

fn ass_text(s: &str) -> String {
    s.replace(['{', '}'], "").replace("\r\n", "\\N").replace('\n', "\\N")
}

fn ass_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    let h = cs / 360_000;
    let m = (cs / 6_000) % 60;
    let s = (cs / 100) % 60;
    let f = cs % 100;
    format!("{h}:{m:02}:{s:02}.{f:02}")
}

pub fn render(format: SubFormat, cues: &[Cue], blocks: &[Cue], meta: &ExportMeta) -> String {
    match format {
        SubFormat::Srt => cues
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}\n{} --> {}\n{}\n", i + 1, stamp(c.start, ','), stamp(c.end, ','), lines_of(c).join("\n")))
            .collect::<Vec<_>>()
            .join("\n"),
        SubFormat::Vtt => {
            let body = cues
                .iter()
                .map(|c| {
                    let ls = lines_of(c).into_iter().map(|l| l.replace("-->", "→")).collect::<Vec<_>>().join("\n");
                    format!("{} --> {}\n{ls}\n", stamp(c.start, '.'), stamp(c.end, '.'))
                })
                .collect::<Vec<_>>()
                .join("\n");
            format!("WEBVTT\n\n{body}")
        }
        SubFormat::Ass => {
            let mut lines = vec![
                "[Script Info]".to_string(),
                format!("Title: {}", ass_text(&meta.title)),
                "ScriptType: v4.00+".into(),
                "PlayResX: 1920".into(),
                "PlayResY: 1080".into(),
                "WrapStyle: 0".into(),
                "ScaledBorderAndShadow: yes".into(),
                "".into(),
                "[V4+ Styles]".into(),
                "Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding".into(),
                "Style: Default,PingFang SC,60,&H00FFFFFF,&H000000FF,&H00000000,&H64000000,-1,0,0,0,100,100,0,0,1,3,1,2,80,80,60,1".into(),
                "Style: Original,Arial,42,&H00E6E6E6,&H000000FF,&H00000000,&H64000000,0,0,0,0,100,100,0,0,1,2.5,1,2,80,80,60,1".into(),
                "".into(),
                "[Events]".into(),
                "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text".into(),
            ];
            for c in cues {
                let text = match c.zh.as_deref().filter(|s| !s.is_empty()) {
                    Some(zh) if !c.orig.is_empty() => format!("{}\\N{{\\rOriginal}}{}", ass_text(zh), ass_text(&c.orig)),
                    Some(zh) => ass_text(zh),
                    None => ass_text(&c.orig),
                };
                let style = if c.zh.as_deref().is_some_and(|s| !s.is_empty()) { "Default" } else { "Original" };
                lines.push(format!("Dialogue: 0,{},{},{style},,0,0,0,,{text}", ass_time(c.start), ass_time(c.end)));
            }
            format!("{}\n", lines.join("\n"))
        }
        SubFormat::Lrc => {
            let mut lines = vec![format!("[ti:{}]", meta.title)];
            if let Some(show) = meta.show.as_deref().filter(|s| !s.is_empty()) {
                lines.push(format!("[ar:{show}]"));
            }
            lines.push("[by:SubCast]".into());
            for c in cues {
                let cs = (c.start.max(0.0) * 100.0).round() as u64;
                let stamp = format!("[{:02}:{:02}.{:02}]", cs / 6000, (cs / 100) % 60, cs % 100);
                for l in lines_of(c) {
                    lines.push(format!("{stamp}{l}"));
                }
            }
            format!("{}\n", lines.join("\n"))
        }
        SubFormat::Txt => {
            let out = blocks
                .iter()
                .map(|b| {
                    let mut ls = Vec::new();
                    if !b.orig.is_empty() { ls.push(b.orig.as_str()); }
                    if let Some(z) = b.zh.as_deref().filter(|s| !s.is_empty()) { ls.push(z); }
                    format!("[{}] {}", clock_of(b.start), ls.join("\n"))
                })
                .collect::<Vec<_>>()
                .join("\n\n");
            format!("{out}\n")
        }
        SubFormat::Md => {
            let mut lines = vec![format!("# {}", meta.title), String::new()];
            let info: Vec<&str> = [meta.show.as_deref(), meta.url.as_deref()]
                .into_iter()
                .flatten()
                .filter(|s| !s.is_empty())
                .collect();
            if !info.is_empty() {
                lines.push(info.join(" · "));
                lines.push(String::new());
            }
            for b in blocks {
                let mut ls = Vec::new();
                if !b.orig.is_empty() { ls.push(b.orig.as_str()); }
                if let Some(z) = b.zh.as_deref().filter(|s| !s.is_empty()) { ls.push(z); }
                lines.push(format!("**{}** {}", clock_of(b.start), ls.join("  \n")));
                lines.push(String::new());
            }
            lines.join("\n")
        }
    }
}

pub fn safe_filename(title: &str, ext: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { ' ' } else { c })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let base: String = collapsed.chars().take(100).collect();
    let base = if base.trim().is_empty() { "subtitles".to_string() } else { base };
    format!("{base}.{ext}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::align::Piece;

    #[test]
    fn splits_abbreviations_and_decimals_cleanly() {
        let ps = pieces("Mr. Smith raised it to 1.5x in the U.S. market.", false);
        assert_eq!(ps.len(), 1, "should keep Mr., 1.5, U.S. together: {ps:?}");
        let zh = pieces("这是第一句完整的中文表达。接下来看第二句，这里虽然有点长但是也能按逗号合理断句。", true);
        assert!(zh.len() >= 2);
    }

    #[test]
    fn renders_all_six_formats_and_applies_timemap() {
        let segs = vec![Segment { start: 10.0, end: 15.0, text: "Hello world. How are you?".into() }];
        let zh = vec![Some("你好世界。你好吗？".to_string())];
        let cues = build_cues(&segs, Some(&zh), SubContent::Bilingual);
        let blocks = build_blocks(&segs, Some(&zh), SubContent::Bilingual);
        let tmap = TimeMap { play_bytes: 100, method: "manual".into(), pieces: vec![Piece { ref_start: 0.0, ref_end: 100.0, play_start: 30.0 }] };
        let moved = on_clock(&cues, Some(&tmap));
        assert!((moved[0].start - 40.0).abs() < 0.01);

        let meta = ExportMeta { title: "Ep 1".into(), show: Some("Test Show".into()), url: Some("https://example.com".into()) };
        assert!(render(SubFormat::Srt, &cues, &blocks, &meta).contains("00:00:10,000 -->"));
        assert!(render(SubFormat::Vtt, &cues, &blocks, &meta).starts_with("WEBVTT"));
        assert!(render(SubFormat::Ass, &cues, &blocks, &meta).contains("[V4+ Styles]"));
        assert!(render(SubFormat::Lrc, &cues, &blocks, &meta).contains("[by:SubCast]"));
        assert!(render(SubFormat::Txt, &cues, &blocks, &meta).contains("[00:10] Hello world."));
        assert!(render(SubFormat::Md, &cues, &blocks, &meta).contains("# Ep 1"));
    }
}
