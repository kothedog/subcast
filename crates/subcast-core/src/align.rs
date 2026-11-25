//! Lining subtitles up with the audio that actually plays. Podcast hosts re-cut episodes and swap the ads in them,
//! so the file someone transcribed (or our own earlier download) can differ from the one streaming now: the speech
//! is the same, shifted by whole blocks around the ad breaks. Subtitles remember the file they were made from
//! (`AudioRef`: size, length and acoustic fingerprint). When the playing file is a different one, a `TimeMap` says
//! which stretches the two share and where; the player converts between its own clock and subtitle time with it,
//! so the subtitles, translations and everything pointing into them keep their times.
use crate::{asr, net};
use anyhow::{anyhow, Result};
use base64::Engine;
use rusty_chromaprint::{Configuration, Fingerprinter};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::Duration;

/// chromaprint's default preset (2): one 32-bit item every 1366/11025 s, about 8 a second.
pub const FP_ALGO: &str = "chromaprint2";
pub const ITEM_SECS: f64 = 1366.0 / 11025.0;

/// The audio file an episode's subtitles were made from (blob "audio-ref").
#[derive(Serialize, Deserialize, Clone, Default, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AudioRef {
    pub bytes: u64,
    #[serde(default)]
    pub etag: String,
    pub secs: f64,
    /// Its fingerprint (base64 of little-endian u32 items), once known here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fp: Option<String>,
    /// The shared-library version the subtitles came from, whose fingerprint the server can send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_id: Option<String>,
    #[serde(default)]
    pub has_fp: bool,
}

/// A stretch both files share: subtitle time `ref_start..ref_end` plays from `play_start` in the playing file.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Piece {
    pub ref_start: f64,
    pub ref_end: f64,
    pub play_start: f64,
}

/// How subtitle time maps onto one playing file (blob "tmap"). Before the first piece and after the last, the
/// nearest piece's offset holds. Between pieces, whatever only one side has (an ad, a cut) is a gap.
#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct TimeMap {
    /// Size of the file this map is for.
    pub play_bytes: u64,
    /// same | fingerprint | probe | manual
    pub method: String,
    pub pieces: Vec<Piece>,
}

impl TimeMap {
    pub fn same(play_bytes: u64) -> Self { Self { play_bytes, method: "same".into(), pieces: vec![] } }

    /// Where subtitle time `t` plays in this map's file (src/lib/timemap.ts `toPlay` does the same for the player).
    pub fn to_play(&self, t: f64) -> f64 {
        let (Some(first), Some(last)) = (self.pieces.first(), self.pieces.last()) else { return t };
        if t < first.ref_start { return t + first.play_start - first.ref_start; }
        for (i, p) in self.pieces.iter().enumerate() {
            if t < p.ref_end { return p.play_start + (t - p.ref_start).max(0.0); }
            // Only in the subtitles' file (an old ad): carry on where the next shared stretch starts.
            if let Some(next) = self.pieces.get(i + 1) { if t < next.ref_start { return next.play_start; } }
        }
        t + last.play_start - last.ref_start
    }
}

pub fn encode_fp(items: &[u32]) -> String {
    let bytes: Vec<u8> = items.iter().flat_map(|x| x.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn decode_fp(b64: &str) -> Option<Vec<u32>> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    (bytes.len() % 4 == 0).then(|| bytes.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect())
}

/// The length (seconds) and fingerprint of an audio file, decoded at its own rate and mixed down to mono, so even a
/// three-hour episode never sits in memory.
pub fn fingerprint_file(path: &Path) -> Result<(f64, Vec<u32>)> {
    let config = Configuration::preset_test2();
    let mut printer = Fingerprinter::new(&config);
    let (mut rate, mut frames, mut failed) = (0u32, 0u64, false);
    let mut ints: Vec<i16> = Vec::new();
    asr::decode_mono(path, |r, samples| {
        if rate == 0 {
            rate = r;
            failed = printer.start(r, 1).is_err();
        }
        if failed || r != rate { return; }
        frames += samples.len() as u64;
        ints.clear();
        ints.extend(samples.iter().map(|x| (x.clamp(-1.0, 1.0) * 32767.0) as i16));
        printer.consume(&ints);
    })?;
    if rate == 0 || failed { return Err(anyhow!("无法计算音频指纹")); }
    printer.finish();
    Ok((frames as f64 / rate as f64, printer.fingerprint().to_vec()))
}

/// The pieces two fingerprints share, in order. Each offset at which many items agree is scanned for runs where
/// the two match bit for bit (mostly); runs shorter than a few seconds are dropped (jingles and music beds repeat),
/// then the longest chain that moves forward in both files is kept.
pub fn map_fingerprints(reference: &[u32], playing: &[u32]) -> Result<Vec<Piece>> {
    let segs = matching_runs(reference, playing);
    // Heaviest increasing chain (both offsets move forward, no overlap), O(n²) over a handful of runs.
    let n = segs.len();
    let mut best: Vec<usize> = segs.iter().map(|s| s.2).collect();
    let mut prev: Vec<Option<usize>> = vec![None; n];
    for i in 0..n {
        for j in 0..i {
            let (a, b) = (segs[j], segs[i]);
            if a.0 + a.2 <= b.0 + 8 && a.1 + a.2 <= b.1 + 8 && best[j] + b.2 > best[i] {
                best[i] = best[j] + b.2;
                prev[i] = Some(j);
            }
        }
    }
    let Some(mut at) = (0..n).max_by_key(|&i| best[i]) else { return Ok(vec![]) };
    let mut chain = vec![segs[at]];
    while let Some(p) = prev[at] { chain.push(segs[p]); at = p; }
    chain.reverse();
    let pieces = chain.iter().map(|&(r, p, len)| Piece {
        ref_start: r as f64 * ITEM_SECS,
        ref_end: (r + len) as f64 * ITEM_SECS,
        play_start: p as f64 * ITEM_SECS,
    }).collect();
    Ok(merge(pieces))
}

/// `(reference index, playing index, length)` of every run where the fingerprints match, sorted by reference index.
fn matching_runs(a: &[u32], b: &[u32]) -> Vec<(usize, usize, usize)> {
    const MIN_ITEMS: usize = 40; // about 5 s
    const WINDOW: usize = 16;
    const MAX_BITS: f64 = 10.0; // average differing bits (of 32) for "the same sound"
    if a.len() < MIN_ITEMS || b.len() < MIN_ITEMS { return vec![]; }
    // Votes for each offset (b index − a index), from items whose top 12 bits agree.
    let mut by_hash: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (i, x) in a.iter().enumerate() { by_hash.entry(x >> 20).or_default().push(i); }
    let shift = a.len();
    let mut votes = vec![0u32; a.len() + b.len()];
    for (j, y) in b.iter().enumerate() {
        if let Some(is) = by_hash.get(&(y >> 20)) {
            // A hash shared by a huge share of items (silence) says nothing about where things line up.
            if is.len() > 64 { continue; }
            for &i in is { votes[j + shift - i] += 1; }
        }
    }
    let mut peaks: Vec<(u32, usize)> = (0..votes.len())
        .filter(|&d| votes[d] >= 12 && (d == 0 || votes[d - 1] <= votes[d]) && (d + 1 == votes.len() || votes[d + 1] < votes[d]))
        .map(|d| (votes[d], d)).collect();
    peaks.sort_unstable_by(|x, y| y.cmp(x));
    peaks.truncate(24);
    let mut runs = Vec::new();
    for (_, d) in peaks {
        let off = d as isize - shift as isize; // b = a + off
        let a0 = (-off).max(0) as usize;
        let len = a.len().saturating_sub(a0).min((b.len() as isize - (a0 as isize + off)).max(0) as usize);
        if len < MIN_ITEMS { continue; }
        let bits: Vec<f64> = (0..len).map(|k| (a[a0 + k] ^ b[(a0 as isize + off) as usize + k]).count_ones() as f64).collect();
        // A sliding average over WINDOW items marks where the two sound the same.
        let mut sum: f64 = bits[..WINDOW.min(len)].iter().sum();
        let mut good = vec![false; len];
        for k in 0..len {
            let lo = k.saturating_sub(WINDOW / 2);
            let hi = (lo + WINDOW).min(len);
            if k > 0 { sum = bits[lo..hi].iter().sum(); }
            good[k] = sum / (hi - lo) as f64 <= MAX_BITS;
        }
        let mut k = 0;
        while k < len {
            if !good[k] { k += 1; continue; }
            let s = k;
            while k < len && good[k] { k += 1; }
            if k - s >= MIN_ITEMS { runs.push((a0 + s, (a0 as isize + off) as usize + s, k - s)); }
        }
    }
    runs.sort();
    runs.dedup();
    runs
}

/// Joins neighbouring pieces with the same offset (a match the matcher split in two).
fn merge(pieces: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::new();
    for p in pieces {
        if let Some(last) = out.last_mut() {
            let same_offset = ((p.play_start - p.ref_start) - (last.play_start - last.ref_start)).abs() < 0.5;
            if same_offset && p.ref_start - last.ref_end < 30.0 { last.ref_end = last.ref_end.max(p.ref_end); continue; }
        }
        out.push(p);
    }
    out
}

/// Offsets closer than this are the same one: recognisers time words to within a couple of seconds, and ad breaks
/// move the speech by much more.
pub const ANCHOR_TOLERANCE: f64 = 4.0;

/// Pieces from matched points `(subtitle time, playing time)`: runs of points with the same offset become pieces,
/// and the change between two runs is placed halfway between their nearest points. A point that disagrees with
/// both neighbours while they agree with each other is a phrase said twice, not a cut, and is dropped.
pub fn map_anchors(anchors: &[(f64, f64)]) -> Vec<Piece> {
    let mut pts: Vec<(f64, f64)> = anchors.to_vec();
    pts.sort_by(|a, b| a.1.total_cmp(&b.1));
    let off = |p: (f64, f64)| p.1 - p.0;
    let close = |a: (f64, f64), b: (f64, f64)| (off(a) - off(b)).abs() < ANCHOR_TOLERANCE;
    let pts: Vec<(f64, f64)> = (0..pts.len()).filter(|&i| {
        let (prev, next) = (i.checked_sub(1).map(|j| pts[j]), pts.get(i + 1).copied());
        match (prev, next) {
            (Some(a), Some(b)) => close(pts[i], a) || close(pts[i], b) || !close(a, b),
            // At either end: alone against two that agree.
            (None, Some(b)) => close(pts[i], b) || pts.get(i + 2).is_none_or(|&c| !close(b, c)),
            (Some(a), None) => close(pts[i], a) || i < 2 || !close(a, pts[i - 2]),
            (None, None) => true,
        }
    }).map(|i| pts[i]).collect();
    let mut runs: Vec<Vec<(f64, f64)>> = Vec::new();
    for p in pts {
        match runs.last_mut() {
            Some(run) if (run.iter().map(|&q| off(q)).sum::<f64>() / run.len() as f64 - off(p)).abs() < ANCHOR_TOLERANCE => run.push(p),
            _ => runs.push(vec![p]),
        }
    }
    let offset = |run: &[(f64, f64)]| { let mut o: Vec<f64> = run.iter().map(|q| q.1 - q.0).collect(); o.sort_by(f64::total_cmp); o[o.len() / 2] };
    let mut out: Vec<Piece> = Vec::new();
    for (i, run) in runs.iter().enumerate() {
        let off = offset(run);
        let first_play = run[0].1;
        let last_play = run[run.len() - 1].1;
        let start_play = if i == 0 { first_play } else { (runs[i - 1][runs[i - 1].len() - 1].1 + first_play) / 2.0 };
        let end_play = if i + 1 == runs.len() { last_play } else { (last_play + runs[i + 1][0].1) / 2.0 };
        let mut ref_start = start_play - off;
        let mut play_start = start_play;
        if let Some(prev) = out.last() {
            // Subtitle time only moves forward: a later run that starts "earlier" leaves a gap in the playing file.
            if ref_start < prev.ref_end { ref_start = prev.ref_end; play_start = ref_start + off; }
        }
        out.push(Piece { ref_start, ref_end: (end_play - off).max(ref_start), play_start });
    }
    out
}

/// The size and ETag of the file an address serves (after redirects), without downloading it.
pub async fn remote_size(url: &str) -> Option<(u64, String)> {
    let client = net::client();
    let etag = |h: &reqwest::header::HeaderMap| h.get("etag").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if let Ok(r) = client.head(url).timeout(Duration::from_secs(15)).send().await {
        let len = r.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
        if r.status().is_success() && len.is_some_and(|n| n > 0) { return Some((len.unwrap(), etag(r.headers()))); }
    }
    // Some hosts refuse HEAD: one byte of the file says its size in Content-Range.
    let r = client.get(url).header("Range", "bytes=0-0").timeout(Duration::from_secs(15)).send().await.ok()?;
    let total = r.headers().get("content-range").and_then(|v| v.to_str().ok()).and_then(|v| v.rsplit('/').next()).and_then(|v| v.parse::<u64>().ok());
    let tag = etag(r.headers());
    total.filter(|&n| n > 0).map(|n| (n, tag))
}

// ---- lining up by listening ------------------------------------------------------------------------------
// For subtitles that don't say which file they were made from (shared before 0.3.0, publishers' own): a few
// short stretches of the playing file are transcribed and found in the subtitles' text.

/// Seconds of audio per probe, each in its own 30 s slot (speech recognisers work in 30 s windows, so a probe
/// never straddles two of them).
pub const PROBE_SECS: f64 = 24.0;
pub const SLOT_SECS: f64 = 30.0;

/// Words (and single CJK characters) of a text, lowercased, each with its share of the way through the text.
fn tokens(text: &str) -> Vec<(String, f64)> {
    let n = text.chars().count().max(1) as f64;
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut at = 0usize;
    for (i, c) in text.chars().enumerate() {
        let cjk = ('\u{3040}'..='\u{30ff}').contains(&c) || ('\u{4e00}'..='\u{9fff}').contains(&c) || ('\u{ac00}'..='\u{d7af}').contains(&c);
        if c.is_alphanumeric() && !cjk {
            if cur.is_empty() { at = i; }
            cur.extend(c.to_lowercase());
            continue;
        }
        if !cur.is_empty() { out.push((std::mem::take(&mut cur), at as f64 / n)); }
        if cjk { out.push((c.to_string(), i as f64 / n)); }
    }
    if !cur.is_empty() { out.push((cur, at as f64 / n)); }
    out
}

/// Timed tokens of timed lines.
fn timed_tokens(lines: &[crate::model::Segment]) -> Vec<(String, f64)> {
    lines.iter().flat_map(|s| tokens(&s.text).into_iter().map(move |(w, f)| (w, s.start + f * (s.end - s.start).max(0.0)))).collect()
}

/// Matched points `(subtitle time, playing time)`, one per probe that could be placed: what was heard in probe
/// `i` (lines timed within the concatenated probe file, slot `i` playing from `starts[i]`) is looked up in the
/// subtitles four words at a time; the offset most of those agree on wins.
pub fn probe_anchors(subtitles: &[crate::model::Segment], heard: &[crate::model::Segment], starts: &[f64]) -> Vec<(f64, f64)> {
    const N: usize = 4;
    let reference = timed_tokens(subtitles);
    let mut index: std::collections::HashMap<&[String], Vec<usize>> = std::collections::HashMap::new();
    let words: Vec<String> = reference.iter().map(|(w, _)| w.clone()).collect();
    for i in 0..words.len().saturating_sub(N - 1) { index.entry(&words[i..i + N]).or_default().push(i); }
    let mut out = Vec::new();
    for (slot, &start) in starts.iter().enumerate() {
        let lo = slot as f64 * SLOT_SECS;
        let lines: Vec<crate::model::Segment> = heard.iter().filter(|s| s.start >= lo && s.start < lo + SLOT_SECS).cloned().collect();
        let got = timed_tokens(&lines);
        let got_words: Vec<String> = got.iter().map(|(w, _)| w.clone()).collect();
        let mut pairs: Vec<(f64, f64)> = Vec::new();
        for j in 0..got_words.len().saturating_sub(N - 1) {
            let Some(hits) = index.get(&got_words[j..j + N]) else { continue };
            if hits.len() > 3 { continue; } // a stock phrase, not a place
            let play = start + (got[j].1 - lo);
            for &h in hits { pairs.push((reference[h].1, play)); }
        }
        // The offset with the most others within 2 s of it; its neighbours' median is the probe's offset.
        let offs: Vec<f64> = pairs.iter().map(|(r, p)| p - r).collect();
        let Some(best) = offs.iter().copied().max_by_key(|o| offs.iter().filter(|x| (*x - o).abs() <= 2.0).count()) else { continue };
        let mut near: Vec<(f64, f64)> = pairs.into_iter().filter(|(r, p)| (p - r - best).abs() <= 2.0).collect();
        if near.len() < 3 { continue; }
        near.sort_by(|a, b| (a.1 - a.0).total_cmp(&(b.1 - b.0)));
        out.push(near[near.len() / 2]);
    }
    out
}

/// Where to listen first: every `step` seconds through a file of `secs`.
pub fn probe_starts(secs: f64, step: f64) -> Vec<f64> {
    let mut v = Vec::new();
    let mut t = 15.0_f64.min(secs / 4.0);
    while t + PROBE_SECS <= secs { v.push(t); t += step; }
    v
}

/// Where to listen again: between neighbouring probes that disagree, so the change lands within a few seconds.
pub fn refine_starts(anchors: &[(f64, f64)]) -> Vec<f64> {
    let mut pts = anchors.to_vec();
    pts.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut v = Vec::new();
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if ((b.1 - b.0) - (a.1 - a.0)).abs() < ANCHOR_TOLERANCE || b.1 - a.1 < PROBE_SECS * 1.5 { continue; }
        let n = (((b.1 - a.1) / PROBE_SECS).floor() as usize).clamp(1, 4);
        for k in 1..=n { v.push(a.1 + (b.1 - a.1) * k as f64 / (n + 1) as f64 - PROBE_SECS / 2.0); }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, seed: u32) -> Vec<u32> {
        let mut x = seed.wrapping_mul(2654435761).wrapping_add(1);
        (0..n).map(|_| { x ^= x << 13; x ^= x >> 17; x ^= x << 5; x }).collect()
    }

    #[test]
    fn fingerprints_find_an_inserted_ad() {
        // Subtitles' file: 3000 items of speech. Playing file: a 400-item ad replaces nothing, inserted at 1000.
        let speech = noise(3000, 7);
        let ad = noise(400, 99);
        let playing: Vec<u32> = [&speech[..1000], &ad[..], &speech[1000..]].concat();
        let pieces = map_fingerprints(&speech, &playing).unwrap();
        assert_eq!(pieces.len(), 2, "{pieces:?}");
        let (a, b) = (&pieces[0], &pieces[1]);
        assert!(a.ref_start < 1.0 && (a.play_start - a.ref_start).abs() < 0.2, "{a:?}");
        assert!((b.ref_start - 1000.0 * ITEM_SECS).abs() < 2.0, "{b:?}");
        assert!(((b.play_start - b.ref_start) - 400.0 * ITEM_SECS).abs() < 0.2, "{b:?}");
    }

    #[test]
    fn fingerprints_find_a_replaced_ad() {
        // An old 300-item ad at 1200 becomes a 500-item one; the episode opens with a new 100-item spot.
        let a = noise(1200, 1);
        let old_ad = noise(300, 2);
        let b = noise(1500, 3);
        let reference: Vec<u32> = [&a[..], &old_ad[..], &b[..]].concat();
        let playing: Vec<u32> = [&noise(100, 4)[..], &a[..], &noise(500, 5)[..], &b[..]].concat();
        let pieces = map_fingerprints(&reference, &playing).unwrap();
        assert_eq!(pieces.len(), 2, "{pieces:?}");
        assert!(((pieces[0].play_start - pieces[0].ref_start) - 100.0 * ITEM_SECS).abs() < 0.2);
        assert!(((pieces[1].play_start - pieces[1].ref_start) - 300.0 * ITEM_SECS).abs() < 0.2);
        assert!(pieces[1].ref_start >= 1490.0 * ITEM_SECS, "the old ad is a gap in the subtitles: {pieces:?}");
    }

    #[test]
    fn identical_files_are_one_piece() {
        let f = noise(2000, 11);
        let pieces = map_fingerprints(&f, &f).unwrap();
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].play_start, pieces[0].ref_start);
    }

    #[test]
    fn anchors_make_pieces() {
        // Offset 0 until ~830 s, then +16.5 s (a longer mid-roll), as in a real re-cut episode.
        let pts = [(100.0, 100.0), (300.0, 300.2), (800.0, 800.0), (950.0, 966.5), (1500.0, 1516.4)];
        let pieces = map_anchors(&pts);
        assert_eq!(pieces.len(), 2, "{pieces:?}");
        assert!((pieces[1].play_start - pieces[1].ref_start - 16.5).abs() < 0.2);
        assert!(pieces[1].ref_start >= pieces[0].ref_end);
        // The other way round: the playing file lost 20 s.
        let pieces = map_anchors(&[(100.0, 100.0), (500.0, 500.0), (700.0, 680.0), (900.0, 880.0)]);
        assert_eq!(pieces.len(), 2);
        assert!((pieces[1].ref_start - pieces[0].ref_end - 20.0).abs() < 0.5, "{pieces:?}");
    }

    #[test]
    fn maps_subtitle_time_to_the_playing_file() {
        let m = TimeMap { play_bytes: 1, method: "fingerprint".into(), pieces: vec![
            Piece { ref_start: 10.0, ref_end: 100.0, play_start: 10.0 },
            Piece { ref_start: 130.0, ref_end: 300.0, play_start: 150.0 },
        ] };
        assert_eq!(m.to_play(5.0), 5.0);
        assert_eq!(m.to_play(50.0), 50.0);
        assert_eq!(m.to_play(110.0), 150.0, "a stretch only the subtitles have jumps to what comes next");
        assert_eq!(m.to_play(200.0), 220.0);
        assert_eq!(m.to_play(400.0), 420.0);
        assert_eq!(TimeMap::same(1).to_play(42.0), 42.0);
    }

    #[test]
    fn probes_find_their_place() {
        use crate::model::Segment;
        let text = "the quick brown fox jumps over the lazy dog while the band plays a slow tune for everyone in the hall tonight";
        let words: Vec<&str> = text.split(' ').collect();
        // Subtitles: one line per 4 words, 2 s each.
        let subs: Vec<Segment> = words.chunks(4).enumerate().map(|(i, c)| Segment { start: i as f64 * 2.0, end: i as f64 * 2.0 + 2.0, text: c.join(" ") }).collect();
        // Heard: the same speech 30 s later in the playing file (probe at 30 s), as one line in slot 0.
        let heard = vec![Segment { start: 0.0, end: subs.last().unwrap().end, text: text.to_string() }];
        let a = probe_anchors(&subs, &heard, &[30.0]);
        assert_eq!(a.len(), 1);
        assert!(((a[0].1 - a[0].0) - 30.0).abs() < 1.0, "{a:?}");
        assert!(refine_starts(&[(100.0, 100.0), (190.0, 206.0)]).len() >= 1);
        assert!(refine_starts(&[(100.0, 100.0), (190.0, 192.5)]).is_empty(), "timing jitter is not a cut");
        // A phrase said twice lands far away once; it is not a cut.
        let p = map_anchors(&[(100.0, 100.0), (200.0, 201.5), (300.0, -74.0 + 374.0 + 300.0), (400.0, 399.0), (500.0, 516.5), (600.0, 617.0)]);
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(tokens("Hello, 世界!")[1].0, "世");
    }

    #[test]
    fn fp_round_trip() {
        let f = noise(10, 3);
        assert_eq!(decode_fp(&encode_fp(&f)).unwrap(), f);
    }
}

/// Real audio: `SUBCAST_ALIGN_A` is an episode, `SUBCAST_ALIGN_B` the same episode re-cut (an ad inserted at 600 s,
/// 20 s removed at 1200 s, re-encoded), e.g. made with ffmpeg from A.
#[cfg(test)]
mod real {
    use super::*;

    #[test]
    #[ignore]
    fn real_recut_episode() {
        let (Ok(a), Ok(b)) = (std::env::var("SUBCAST_ALIGN_A"), std::env::var("SUBCAST_ALIGN_B")) else { return };
        let t = std::time::Instant::now();
        let (sa, fa) = fingerprint_file(Path::new(&a)).unwrap();
        let (sb, fb) = fingerprint_file(Path::new(&b)).unwrap();
        eprintln!("fingerprints: {:.0}s / {:.0}s, {} / {} items, {:?}", sa, sb, fa.len(), fb.len(), t.elapsed());
        let t = std::time::Instant::now();
        let pieces = map_fingerprints(&fa, &fb).unwrap();
        eprintln!("matched in {:?}", t.elapsed());
        for p in &pieces { eprintln!("  ref {:7.1}–{:7.1}  offset {:+.2}", p.ref_start, p.ref_end, p.play_start - p.ref_start); }
        let off_at = |r: f64| pieces.iter().find(|p| p.ref_start <= r && r < p.ref_end).map(|p| p.play_start - p.ref_start);
        assert!(off_at(300.0).is_some_and(|o| o.abs() < 0.3));
        assert!(off_at(900.0).is_some_and(|o| (o - 37.3).abs() < 0.3));
        assert!(off_at(1500.0).is_some_and(|o| (o - 17.3).abs() < 0.3));
    }
}

/// Real audio, by listening: `SUBCAST_PROBE_AUDIO` (the playing file), `SUBCAST_PROBE_SUBS` (the subtitles' segments
/// as JSON, made from another cut), `SUBCAST_PROBE_BIN` and `SUBCAST_PROBE_MODEL` (CrispASR and a Qwen3-ASR model).
#[cfg(test)]
mod real_probe {
    use super::*;
    use crate::model::Segment;

    #[tokio::test]
    #[ignore]
    async fn real_probe_alignment() {
        let var = |k: &str| std::env::var(k).ok();
        let (Some(audio), Some(subs), Some(bin), Some(model)) = (var("SUBCAST_PROBE_AUDIO"), var("SUBCAST_PROBE_SUBS"), var("SUBCAST_PROBE_BIN"), var("SUBCAST_PROBE_MODEL")) else { return };
        let subs: Vec<Segment> = serde_json::from_str(&std::fs::read_to_string(subs).unwrap()).unwrap();
        let path = std::path::PathBuf::from(&audio);
        let (secs, _) = fingerprint_file(&path).unwrap();
        let work = std::env::temp_dir().join(format!("subcast-probe-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&work).unwrap();
        let t0 = std::time::Instant::now();
        let mut anchors = Vec::new();
        let mut starts = probe_starts(secs, 90.0);
        for round in 0..2 {
            if starts.is_empty() { break; }
            let slot = (SLOT_SECS * 16000.0) as usize;
            let mut all = Vec::new();
            for &s in &starts {
                let mut v = crate::clip::cut(Box::new(std::fs::File::open(&path).unwrap()), Some("mp3"), s, s + PROBE_SECS).unwrap_or_default();
                v.resize(slot, 0.0);
                all.extend(v);
            }
            let wav = work.join(format!("p{round}.wav"));
            crate::asr::write_wav(&wav, &all).unwrap();
            let heard = crate::asr::run_crispasr(Path::new(&bin), Path::new(&model), &wav, "en", &Default::default(), Default::default(), |_| {}).await.unwrap();
            let got = probe_anchors(&subs, &heard, &starts);
            eprintln!("round {round}: {} probes, {} placed, {:?}", starts.len(), got.len(), t0.elapsed());
            for (r, p) in &got { eprintln!("  play {p:7.1}  subs {r:7.1}  offset {:+.2}", p - r); }
            anchors.extend(got);
            starts = refine_starts(&anchors);
        }
        let pieces = map_anchors(&anchors);
        for p in &pieces { eprintln!("  ref {:7.1}–{:7.1}  offset {:+.2}", p.ref_start, p.ref_end, p.play_start - p.ref_start); }
        let _ = std::fs::remove_dir_all(&work);
        let off = |r: f64| pieces.iter().rev().find(|p| p.ref_start <= r).map(|p| p.play_start - p.ref_start).unwrap();
        assert!(off(400.0).abs() < 2.0);
        assert!((off(1300.0) - 16.5).abs() < 2.0);
    }
}
