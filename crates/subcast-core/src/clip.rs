//! One sentence of an episode in the speaker's own voice, kept as a small local file for the word book.
//! Only the bytes around the sentence are fetched (HTTP range requests feed the decoder, which seeks by time),
//! so a clip from a two-hour episode costs a few hundred KB, and replaying it later is instant and offline.
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use sha1::{Digest, Sha1};
use symphonia::core::{audio::SampleBuffer, codecs::{DecoderOptions, CODEC_TYPE_NULL}, errors::Error, formats::{FormatOptions, SeekMode, SeekTo}, io::{MediaSource, MediaSourceStream}, meta::MetadataOptions, probe::Hint, units::Time};

use crate::asr::Resampler;

/// Lead-in and tail around the sentence, so the first and last words aren't clipped.
const BEFORE: f64 = 0.25;
const AFTER: f64 = 0.35;
/// Sentences are short; anything longer is a bad timestamp, not a sentence.
const MAX_LEN: f64 = 40.0;
const RATE: u32 = 16000;
const CHUNK: u64 = 96 * 1024;
/** Bytes fetched over the network for clips since launch (tests read it). */
pub static FETCHED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn dir(data: &Path) -> PathBuf { data.join("clips") }

/// Where the clip for `[start, end]` of an episode lives (whether or not it has been made yet).
pub fn path_for(data: &Path, episode_id: &str, start: f64, end: f64) -> PathBuf {
    let key = format!("{episode_id}|{:.2}|{:.2}", start, end);
    let hash = Sha1::digest(key.as_bytes());
    let name: String = hash.iter().take(10).map(|b| format!("{b:02x}")).collect();
    dir(data).join(format!("{name}.wav"))
}

/// Where to read the episode from: a downloaded file, or a URL fetched in ranges.
pub enum Source { File(PathBuf), Url(String) }

/// Makes the clip unless it exists, and returns its path.
pub async fn make(data: &Path, source: Source, episode_id: &str, start: f64, end: f64) -> Result<PathBuf> {
    let out = path_for(data, episode_id, start, end);
    if out.exists() { return Ok(out); }
    if !(end > start) || end - start > MAX_LEN { bail!("这一句的时间不对"); } // i18n-ignore (never shown: the interface falls back to the system voice)
    std::fs::create_dir_all(dir(data))?;
    let rt = tokio::runtime::Handle::current();
    let samples = tokio::task::spawn_blocking(move || -> Result<Vec<f32>> {
        match source {
            Source::File(p) => {
                let ext = p.extension().and_then(|e| e.to_str()).map(str::to_string);
                cut(Box::new(std::fs::File::open(&p)?), ext.as_deref(), start, end)
            }
            Source::Url(u) => {
                let ext = ext_of(&u);
                cut(Box::new(Ranged::open(rt, u)?), ext.as_deref(), start, end)
            }
        }
    }).await??;
    if samples.len() < (RATE as usize) / 10 { bail!("没有截到声音"); } // i18n-ignore (never shown: the interface falls back to the system voice)
    // Written beside, then renamed: a half-written file is never taken for a clip.
    let tmp = out.with_extension("part");
    crate::asr::write_wav(&tmp, &samples)?;
    std::fs::rename(&tmp, &out)?;
    Ok(out)
}

/// The file extension in a URL's path, as a hint for the decoder (YouTube's progressive streams are MP4).
fn ext_of(url: &str) -> Option<String> {
    if url.contains("googlevideo.com") { return Some("mp4".into()); }
    let path = url.split(['?', '#']).next()?;
    let ext = path.rsplit('/').next()?.rsplit_once('.')?.1.to_ascii_lowercase();
    (ext.len() <= 4 && ext.chars().all(|c| c.is_ascii_alphanumeric())).then_some(ext)
}

/// Decodes `[start - BEFORE, end + AFTER]` of the first audio track, mixed to mono at 16 kHz.
pub fn cut(src: Box<dyn MediaSource>, ext: Option<&str>, start: f64, end: f64) -> Result<Vec<f32>> {
    let mss = MediaSourceStream::new(src, Default::default());
    let mut hint = Hint::new();
    if let Some(e) = ext { hint.with_extension(e); }
    let probed = symphonia::default::get_probe().format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default()).map_err(|e| anyhow!("无法识别音频格式：{e}"))?; // i18n-ignore (never shown: the interface falls back to the system voice)
    let mut format = probed.format;
    // Video files carry a video track too: the audio one is the one with a sample rate.
    let track = format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL && t.codec_params.sample_rate.is_some()).ok_or_else(|| anyhow!("没有音轨"))?; // i18n-ignore (never shown: the interface falls back to the system voice)
    let (id, rate, tb) = (track.id, track.codec_params.sample_rate.unwrap_or(44100), track.codec_params.time_base);
    let mut decoder = symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| anyhow!("不支持的音频编码：{e}"))?; // i18n-ignore (never shown: the interface falls back to the system voice)
    let from = (start - BEFORE).max(0.0);
    let to = end + AFTER;
    // Without a seek the decoder would read the whole episode up to the sentence: give up instead.
    if from > 1.0 {
        let t = Time::new(from.floor() as u64, from.fract());
        format.seek(SeekMode::Coarse, SeekTo::Time { time: t, track_id: Some(id) }).map_err(|e| anyhow!("这个音频不能跳转：{e}"))?; // i18n-ignore (never shown: the interface falls back to the system voice)
        decoder.reset();
    }
    let secs = |ts: u64| match tb { Some(tb) => { let t = tb.calc_time(ts); t.seconds as f64 + t.frac } None => ts as f64 / rate as f64 };
    let mut r = Resampler::new(rate, RATE);
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(anyhow!("读取音频失败：{e}")), // i18n-ignore (never shown: the interface falls back to the system voice)
        };
        if packet.track_id() != id { continue; }
        let t0 = secs(packet.ts());
        if t0 > to { break; }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(anyhow!("解码失败：{e}")), // i18n-ignore (never shown: the interface falls back to the system voice)
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        if buf.as_ref().is_none_or(|b| b.capacity() < decoded.capacity() * channels) { buf = Some(SampleBuffer::new(decoded.capacity() as u64, spec)); }
        let b = buf.as_mut().unwrap();
        b.copy_interleaved_ref(decoded);
        for (i, frame) in b.samples().chunks(channels).enumerate() {
            let t = t0 + i as f64 / spec.rate as f64;
            if t >= from && t <= to { r.push(frame.iter().sum::<f32>() / channels as f32); }
        }
    }
    let mut out = r.finish();
    // Short fades, so the clip doesn't start or stop with a click.
    let fade = (RATE as usize / 50).min(out.len() / 2);
    let n = out.len();
    for i in 0..fade { let g = i as f32 / fade as f32; out[i] *= g; out[n - 1 - i] *= g; }
    Ok(out)
}

/// A remote file read through HTTP range requests in 96 KB pieces, two at a time, for the decoder (which only needs the
/// container's index and the bytes around the sentence).
struct Ranged { rt: tokio::runtime::Handle, /** After redirects (podcast hosts bounce through trackers on every request). */ url: String, len: u64, pos: u64, chunks: HashMap<u64, Vec<u8>> }

impl Ranged {
    fn open(rt: tokio::runtime::Handle, url: String) -> Result<Self> {
        let mut me = Self { rt, url, len: 0, pos: 0, chunks: HashMap::new() };
        let (first, total, landed) = me.fetch(0, CHUNK - 1)?;
        me.len = total.ok_or_else(|| anyhow!("这个音频地址不支持分段读取"))?; // i18n-ignore (never shown: the interface falls back to the system voice)
        me.url = landed;
        me.chunks.insert(0, first);
        Ok(me)
    }

    /// Bytes `a..=b` and the file's total length (from Content-Range).
    fn fetch(&self, a: u64, b: u64) -> Result<(Vec<u8>, Option<u64>, String)> {
        let url = self.url.clone();
        self.rt.block_on(async move {
            let mut last = None;
            for attempt in 0..3u64 {
                if attempt > 0 { tokio::time::sleep(std::time::Duration::from_millis(300 * attempt)).await; }
                let res = crate::net::client().get(&url).header(reqwest::header::RANGE, format!("bytes={a}-{b}"))
                    .timeout(std::time::Duration::from_secs(20)).send().await;
                match res {
                    Ok(r) if r.status().as_u16() == 206 => {
                        let landed = r.url().to_string();
                        let total = r.headers().get(reqwest::header::CONTENT_RANGE).and_then(|v| v.to_str().ok())
                            .and_then(|v| v.rsplit('/').next()).and_then(|v| v.trim().parse::<u64>().ok());
                        let bytes = r.bytes().await.context("读取音频失败")?.to_vec(); // i18n-ignore (never shown: the interface falls back to the system voice)
                        FETCHED.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
                        return Ok((bytes, total, landed));
                    }
                    Ok(r) if r.status().is_server_error() || r.status().as_u16() == 429 => last = Some(anyhow!("音频地址返回 {}", r.status().as_u16())), // i18n-ignore (never shown: the interface falls back to the system voice)
                    Ok(r) => bail!("这个音频地址不支持分段读取（{}）", r.status().as_u16()), // i18n-ignore (never shown: the interface falls back to the system voice)
                    Err(e) => last = Some(e.into()),
                }
            }
            Err(last.unwrap_or_else(|| anyhow!("读取音频失败"))) // i18n-ignore (never shown: the interface falls back to the system voice)
        })
    }

    fn chunk(&mut self, i: u64) -> std::io::Result<&Vec<u8>> {
        if !self.chunks.contains_key(&i) {
            // Reads run forward, so the next piece is fetched alongside: on a slow line the wait is mostly
            // latency, and two requests at once cost about as much as one.
            let want: Vec<u64> = [i, i + 1].into_iter().filter(|j| j * CHUNK < self.len && !self.chunks.contains_key(j)).collect();
            let jobs: Vec<_> = want.iter().map(|&j| {
                let me = Self { rt: self.rt.clone(), url: self.url.clone(), len: self.len, pos: 0, chunks: HashMap::new() };
                std::thread::spawn(move || { let a = j * CHUNK; me.fetch(a, (a + CHUNK).min(me.len) - 1).map(|r| (j, r.0)) })
            }).collect();
            let mut failed = None;
            for job in jobs {
                match job.join() {
                    Ok(Ok((j, bytes))) => { self.chunks.insert(j, bytes); }
                    Ok(Err(err)) => { failed.get_or_insert(err.to_string()); }
                    Err(_) => { failed.get_or_insert("读取音频失败".into()); } // i18n-ignore (never shown: the interface falls back to the system voice)
                }
            }
            // Only the piece asked for matters; a failed look-ahead is fetched again when it is reached.
            if !self.chunks.contains_key(&i) { return Err(std::io::Error::other(failed.unwrap_or_default())); }
        }
        Ok(&self.chunks[&i])
    }
}

impl Read for Ranged {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len || out.is_empty() { return Ok(0); }
        let i = self.pos / CHUNK;
        let off = (self.pos - i * CHUNK) as usize;
        let c = self.chunk(i)?;
        if off >= c.len() { return Ok(0); }
        let n = (c.len() - off).min(out.len());
        out[..n].copy_from_slice(&c[off..off + n]);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Ranged {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        let p = match to { SeekFrom::Start(p) => p as i128, SeekFrom::End(d) => self.len as i128 + d as i128, SeekFrom::Current(d) => self.pos as i128 + d as i128 };
        if p < 0 { return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "seek before start")); }
        self.pos = p as u64;
        Ok(self.pos)
    }
}

impl MediaSource for Ranged {
    fn is_seekable(&self) -> bool { true }
    fn byte_len(&self) -> Option<u64> { Some(self.len) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_and_hints() {
        let d = Path::new("/d");
        assert_eq!(path_for(d, "e", 1.0, 2.0), path_for(d, "e", 1.001, 2.004));
        assert_ne!(path_for(d, "e", 1.0, 2.0), path_for(d, "e", 1.0, 2.5));
        assert_eq!(ext_of("https://x.com/a/b.MP3?x=1").as_deref(), Some("mp3"));
        assert_eq!(ext_of("https://rr1.googlevideo.com/videoplayback?id=1").as_deref(), Some("mp4"));
        assert_eq!(ext_of("https://x.com/feed/episode"), None);
    }

    /// A real episode, cut from the middle: `CLIP_URL=… cargo test --lib clip::tests::cuts_a_remote_sentence -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn cuts_a_remote_sentence() {
        let url = std::env::var("CLIP_URL").expect("CLIP_URL");
        let at: f64 = std::env::var("CLIP_AT").ok().and_then(|s| s.parse().ok()).unwrap_or(1800.0);
        let rt = tokio::runtime::Runtime::new().unwrap();
        let data = std::env::temp_dir().join("subcast-clip-test");
        let _ = std::fs::remove_dir_all(&data);
        let t = std::time::Instant::now();
        let p = rt.block_on(make(&data, Source::Url(url), "test", at, at + 5.0)).unwrap();
        let size = std::fs::metadata(&p).unwrap().len();
        eprintln!("{} bytes in {:?}, fetched {} → {}", size, t.elapsed(), FETCHED.load(std::sync::atomic::Ordering::Relaxed), p.display());
        assert!(size > 100_000);
    }
}
