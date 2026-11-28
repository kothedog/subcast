// 跟读打分: the microphone, heard by the local speech model. The listener repeats a sentence after the speaker; the
// interface asks what was heard so far (every second or so while they speak, and once more when they stop) and
// lights up the words they got. Audio never leaves the computer.
//
// The stream lives on its own thread (cpal streams aren't Send on every platform). It keeps the last 30 s at the
// device's own rate; `hear` resamples to 16 kHz for the model.
use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use parking_lot::Mutex;
use std::sync::{mpsc, Arc};

const KEEP_SECS: usize = 30;

struct Mic {
    stop: mpsc::Sender<()>,
    buf: Arc<Mutex<Vec<f32>>>,
    rate: u32,
}

static MIC: Mutex<Option<Mic>> = Mutex::new(None);

/// Opens the default microphone (again, if open) and starts sending its level as "shadow-level" events (0–1,
/// about 12 a second), which the interface uses to see speech start and stop.
pub fn start(on_level: impl Fn(f32) + Send + 'static) -> Result<()> {
    stop();
    let buf = Arc::new(Mutex::new(Vec::<f32>::new()));
    let level = Arc::new(AtomicU32::new(0));
    let (stop_tx, stop_rx) = mpsc::channel::<()>();
    let (ready_tx, ready_rx) = mpsc::channel::<Result<u32>>();
    let (b, lv) = (buf.clone(), level.clone());
    std::thread::spawn(move || {
        let opened = (|| -> Result<(cpal::Stream, u32)> {
            let dev = cpal::default_host().default_input_device().ok_or_else(|| anyhow!("没有找到麦克风"))?;
            let cfg = dev.default_input_config().context("麦克风打不开")?;
            let rate = cfg.sample_rate().0;
            let ch = cfg.channels().max(1) as usize;
            let keep = rate as usize * KEEP_SECS;
            let push = move |mono: &mut dyn Iterator<Item = f32>| {
                let mut peak = 0f32;
                let mut g = b.lock();
                for s in mono { peak = peak.max(s.abs()); g.push(s); }
                if g.len() > keep { let cut = g.len() - keep; g.drain(..cut); }
                // Highest peak since the level was last read.
                let was = f32::from_bits(lv.load(Ordering::Relaxed));
                if peak > was { lv.store(peak.to_bits(), Ordering::Relaxed); }
            };
            let err = |e| eprintln!("mic: {e}");
            let stream = match cfg.sample_format() {
                cpal::SampleFormat::F32 => dev.build_input_stream(&cfg.into(), move |d: &[f32], _: &_| push(&mut d.chunks(ch).map(|f| f.iter().sum::<f32>() / ch as f32)), err, None),
                cpal::SampleFormat::I16 => dev.build_input_stream(&cfg.into(), move |d: &[i16], _: &_| push(&mut d.chunks(ch).map(|f| f.iter().map(|&x| x as f32 / 32768.0).sum::<f32>() / ch as f32)), err, None),
                cpal::SampleFormat::U16 => dev.build_input_stream(&cfg.into(), move |d: &[u16], _: &_| push(&mut d.chunks(ch).map(|f| f.iter().map(|&x| (x as f32 - 32768.0) / 32768.0).sum::<f32>() / ch as f32)), err, None),
                other => return Err(anyhow!("不支持这个麦克风的格式：{}", format!("{other:?}"))),
            }.context("麦克风打不开")?;
            stream.play().context("麦克风打不开")?;
            Ok((stream, rate))
        })();
        let stream = match opened {
            Ok((s, rate)) => { let _ = ready_tx.send(Ok(rate)); s }
            Err(e) => { let _ = ready_tx.send(Err(e)); return; }
        };
        loop {
            match stop_rx.recv_timeout(std::time::Duration::from_millis(80)) {
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let peak = f32::from_bits(level.swap(0, Ordering::Relaxed));
                    on_level(peak);
                }
                _ => break,
            }
        }
        drop(stream);
    });
    let rate = ready_rx.recv().map_err(|_| anyhow!("麦克风打不开"))??;
    *MIC.lock() = Some(Mic { stop: stop_tx, buf, rate });
    Ok(())
}

pub fn stop() {
    if let Some(m) = MIC.lock().take() { let _ = m.stop.send(()); }
}

/// Forgets what was recorded so far: called when the listener's turn starts (the speaker was playing until now).
pub fn clear() {
    if let Some(m) = MIC.lock().as_ref() { m.buf.lock().clear(); }
}

/// What was recorded since `clear`, at 16 kHz.
fn take() -> Option<Vec<f32>> {
    let g = MIC.lock();
    let m = g.as_ref()?;
    let raw = m.buf.lock().clone();
    Some(crate::asr::resample(&raw, m.rate, 16000))
}

/// The installed model to hear with: the one picked in settings, else any other installed one (Qwen3-ASR only when
/// its program is there).
fn pick(data: &Path, wanted: &str) -> Option<crate::asr::Resolved> {
    let usable = |r: &crate::asr::Resolved| r.path.is_file() && (r.engine != "qwen3" || crate::asr::find_crispasr(data).is_some());
    std::iter::once(wanted.to_string()).chain(crate::asr::MODELS.iter().map(|m| m.id.to_string()))
        .filter_map(|id| crate::asr::resolve(data, &id))
        .find(usable)
}

pub fn ready(data: &Path, wanted: &str) -> bool { pick(data, wanted).is_some() }

/// Text of what the listener said since their turn started; "" for less than half a second of sound.
pub async fn hear(data: &Path, wanted: &str, language: &str) -> Result<String> {
    let pcm = take().ok_or_else(|| anyhow!("麦克风没有打开"))?;
    if pcm.len() < 8000 { return Ok(String::new()); }
    let model = pick(data, wanted).ok_or_else(|| anyhow!("先在「语音识别」里下载一个本机模型，才能跟读打分"))?;
    let work = data.join("work").join(format!("shadow-{}", uuid::Uuid::new_v4().simple()));
    tokio::fs::create_dir_all(&work).await?;
    let wav = work.join("said.wav");
    crate::asr::write_wav(&wav, &pcm)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let heard = if model.engine == "qwen3" {
        let bin = crate::asr::find_crispasr(data).ok_or_else(|| anyhow!("Qwen3-ASR 识别程序还没准备好"))?;
        crate::asr::run_crispasr(&bin, &model.path, &wav, language, &crate::asr::AsrContext::default(), cancel, |_| {}).await
    } else {
        crate::asr::run_worker(&wav, &model.path, language, "", cancel, |_| {}).await.map(|r| r.segments)
    };
    let _ = tokio::fs::remove_dir_all(&work).await;
    Ok(heard?.into_iter().map(|s| s.text).collect::<Vec<_>>().join(" "))
}

/// System Settings › Privacy › Microphone, for a microphone that only gives silence (access turned off).
pub fn open_mic_settings() {
    #[cfg(target_os = "macos")]
    { let _ = std::process::Command::new("open").arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone").spawn(); }
    #[cfg(windows)]
    { let _ = std::process::Command::new("cmd").args(["/C", "start", "ms-settings:privacy-microphone"]).spawn(); }
}

#[cfg(test)]
mod tests {
    /// Real microphone and model: `say` speaks a sentence through the speakers while the microphone records, then
    /// the local model hears it. Needs microphone access for the test runner and an installed model.
    /// cargo test --lib shadow::tests::mic_hears_say -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn mic_hears_say() {
        let data = dirs::data_dir().unwrap().join("ai.qiaomu.subcast");
        super::start(|_| {}).expect("mic");
        super::clear();
        let st = std::process::Command::new("say").args(["-v", "Samantha", "I guess I really want us to be about technology companies again."]).status().unwrap();
        assert!(st.success());
        std::thread::sleep(std::time::Duration::from_millis(400));
        let pcm = super::take().unwrap();
        let peak = pcm.iter().fold(0f32, |m, x| m.max(x.abs()));
        println!("recorded {:.1}s, peak {peak:.3}", pcm.len() as f32 / 16000.0);
        let t0 = std::time::Instant::now();
        let text = super::hear(&data, "qwen3-asr", "en").await.unwrap();
        println!("heard in {:?}: {text}", t0.elapsed());
        super::stop();
    }
}
