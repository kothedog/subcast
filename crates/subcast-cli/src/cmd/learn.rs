//! Vocabulary book (FSRS-5 + ECDICT + CSV/Anki export), TTS, and shadowing commands.
use crate::cmd::resolve_or_ingest_episode;
use crate::exit::CliError;
use crate::output::Out;
use clap::{Args, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use subcast_core::{local_tts, now_ms, ops, settings, shadow, speech, vocab, CoreCtx, ProgressSink};

#[derive(Args)]
pub struct VocabArgs {
    #[command(subcommand)]
    pub action: Option<VocabAction>,
}

#[derive(Subcommand)]
pub enum VocabAction {
    /// List saved vocabulary words
    List {
        #[arg(long)]
        due: bool,
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Add a word to the vocabulary book (enriches IPA/meaning via ECDICT if available)
    Add {
        word: String,
        #[arg(long)]
        meaning: Option<String>,
        #[arg(long)]
        ipa: Option<String>,
        #[arg(long)]
        sentence: Option<String>,
        #[arg(long)]
        sentence_zh: Option<String>,
        #[arg(long)]
        episode: Option<String>,
    },
    /// Remove a word from the vocabulary book
    Remove { word_or_id: String },
    /// Look up a word in offline ECDICT (`en.json`)
    Lookup {
        word: String,
        #[arg(long)]
        dict: Option<PathBuf>,
    },
    /// View due review queue or rate one word (`again` | `hard` | `good` | `easy`)
    Review {
        word_or_id: Option<String>,
        #[arg(long)]
        rate: Option<String>,
        #[arg(long, default_value_t = 20)]
        daily_new: u32,
    },
    /// Show vocabulary statistics (total, mastered, due, daily log)
    Stats,
    /// Export vocabulary book to CSV or Anki `.apkg`
    Export {
        /// Format: csv | anki
        #[arg(long, default_value = "csv")]
        format: String,
        #[arg(short = 'o', long = "output")]
        output: PathBuf,
        #[arg(long, default_value = "SubCast 词汇本")]
        deck: String,
        #[arg(long, default_value = "SubCast 单词卡片")]
        model: String,
        #[arg(long)]
        with_audio: bool,
    },
}

#[derive(Args)]
pub struct TtsArgs {
    #[command(subcommand)]
    pub action: TtsAction,
}

#[derive(Subcommand)]
pub enum TtsAction {
    /// Synthesize speech to an audio file (cached under tts-cache/)
    Speak {
        text: String,
        #[arg(long)]
        engine: Option<String>,
        #[arg(long)]
        voice: Option<String>,
        #[arg(long)]
        rate: Option<f64>,
        #[arg(long)]
        model: Option<String>,
        #[arg(short = 'o', long = "output")]
        output: Option<PathBuf>,
    },
    /// List available macOS/system voices
    Voices,
    /// Check offline local TTS (MeloTTS / Kokoro) status
    Status,
    /// Install offline local TTS model (`melo` or `kokoro`)
    Install {
        model: String,
        #[arg(long)]
        no_mirror: bool,
    },
    /// Delete an installed offline local TTS model
    Delete { model: String },
    /// Clear the TTS synthesis cache
    ClearCache,
}

#[derive(Args)]
pub struct ShadowArgs {
    #[command(subcommand)]
    pub action: ShadowAction,
}

#[derive(Subcommand)]
pub enum ShadowAction {
    /// Check if microphone ASR model for shadowing is ready
    Ready {
        #[arg(long)]
        model: Option<String>,
    },
    /// Record from the default microphone for N seconds and transcribe what was said
    Record {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
        #[arg(long)]
        model: Option<String>,
        #[arg(long, default_value = "en")]
        language: String,
    },
    /// Clear buffered shadowing audio
    Clear,
}

pub async fn run_vocab(ctx: &CoreCtx, args: VocabArgs, out: Out) -> Result<(), CliError> {
    let action = args.action.unwrap_or(VocabAction::List { due: false, limit: 50 });
    let now = now_ms();
    match action {
        VocabAction::List { due, limit } => {
            let book = vocab::load_book(ctx)?;
            let mut words: Vec<vocab::Word> = book.words.into_values().collect();
            if due {
                words.retain(|w| vocab::is_due(w, now));
            }
            words.sort_by(|a, b| b.added.cmp(&a.added));
            words.truncate(limit);
            out.ok(&words, || {
                if words.is_empty() {
                    println!("Vocabulary book is empty.");
                    return;
                }
                for w in &words {
                    let mastered = if vocab::is_mastered(w) { " [mastered]" } else { "" };
                    println!("{:<18}  /{}/  {}{}", w.word, w.ipa, w.meaning.replace('\n', "; "), mastered);
                }
            });
        }
        VocabAction::Add { word, meaning, ipa, sentence, sentence_zh, episode } => {
            let dict_entry = vocab::Dict::find_path(&ctx.data, None)
                .and_then(|p| vocab::Dict::load(&p).ok())
                .and_then(|d| d.lookup(&word));
            let ep = if let Some(ref ep_ref) = episode {
                Some(resolve_or_ingest_episode(ctx, ep_ref).await?)
            } else {
                None
            };
            let w = vocab::mutate_book(ctx, |book| {
                let id = dict_entry
                    .as_ref()
                    .map(|d| d.lemma.clone())
                    .unwrap_or_else(|| word.trim().to_lowercase());
                let entry = vocab::Word {
                    id: id.clone(),
                    word: dict_entry.as_ref().map(|d| d.lemma.clone()).unwrap_or_else(|| word.trim().to_string()),
                    ipa: ipa.unwrap_or_else(|| dict_entry.as_ref().map(|d| d.ipa.clone()).unwrap_or_default()),
                    meaning: meaning.unwrap_or_else(|| dict_entry.as_ref().map(|d| d.meaning.clone()).unwrap_or_default()),
                    ctx: None,
                    tag: dict_entry.as_ref().map(|d| d.tag_label.clone()).unwrap_or_default(),
                    sentence: sentence.unwrap_or_default(),
                    sentence_zh,
                    episode_id: ep.as_ref().map(|e| e.id.clone()).unwrap_or_default(),
                    episode_title: ep.as_ref().map(|e| e.title.clone()).unwrap_or_default(),
                    show_title: ep.as_ref().and_then(|e| e.show_title.clone()),
                    start: 0.0,
                    end: 0.0,
                    added: now,
                    card: vocab::create_empty_card(now),
                };
                book.words.insert(id, entry.clone());
                Ok(entry)
            })?;
            out.ok(&w, || println!("Added word: {} — {}", w.word, w.meaning));
        }
        VocabAction::Remove { word_or_id } => {
            let key = word_or_id.trim().to_lowercase();
            let removed = vocab::mutate_book(ctx, |book| {
                Ok(book.words.remove(&key).or_else(|| {
                    let found = book
                        .words
                        .iter()
                        .find(|(_, w)| w.word.eq_ignore_ascii_case(&key))
                        .map(|(k, _)| k.clone())?;
                    book.words.remove(&found)
                }))
            })?;
            match removed {
                Some(w) => out.ok(&json!({ "removed": w.id }), || println!("Removed: {}", w.word)),
                None => return Err(CliError::not_found(format!("生词本中找不到：{word_or_id}"))),
            }
        }
        VocabAction::Lookup { word, dict } => {
            let path = vocab::Dict::find_path(&ctx.data, dict.as_deref())
                .ok_or_else(|| CliError::missing("找不到离线词典 dict/en.json"))?;
            let d = vocab::Dict::load(&path)?;
            let entry = d
                .lookup(&word)
                .ok_or_else(|| CliError::not_found(format!("词典未收录：{word}")))?;
            out.ok(&entry, || {
                println!("{}  /{}/  [{}]", entry.lemma, entry.ipa, entry.tag_label);
                println!("{}", entry.meaning);
            });
        }
        VocabAction::Review { word_or_id, rate, daily_new } => {
            if let (Some(id), Some(r_str)) = (word_or_id, rate) {
                let grade = vocab::parse_rating(&r_str)
                    .ok_or_else(|| CliError::usage("rating must be again | hard | good | easy (or 1..4)"))?;
                let updated = vocab::rate_word(ctx, &id, grade, now)?;
                out.ok(&updated, || {
                    println!(
                        "Reviewed {}: state={} reps={} scheduled_days={}",
                        updated.word, updated.card.state, updated.card.reps, updated.card.scheduled_days
                    );
                });
            } else {
                let book = vocab::load_book(ctx)?;
                let q = vocab::review_queue(&book, daily_new, now);
                out.ok(&q, || {
                    if q.is_empty() {
                        println!("No due reviews right now.");
                        return;
                    }
                    println!("Due review queue ({}):", q.len());
                    for w in &q {
                        println!("  {:<18}  {}", w.id, w.meaning.replace('\n', "; "));
                    }
                });
            }
        }
        VocabAction::Stats => {
            let book = vocab::load_book(ctx)?;
            let total = book.words.len();
            let mastered = book.words.values().filter(|w| vocab::is_mastered(w)).count();
            let due = book.words.values().filter(|w| vocab::is_due(w, now)).count();
            let payload = json!({
                "total": total,
                "mastered": mastered,
                "due": due,
                "day": book.day,
                "newToday": book.new_today,
                "log": book.log,
            });
            out.ok(&payload, || {
                println!("Total    : {total}");
                println!("Mastered : {mastered}");
                println!("Due now  : {due}");
                println!("New today: {} ({})", book.new_today, book.day);
            });
        }
        VocabAction::Export { format, output, deck, model, with_audio } => {
            match format.trim().to_ascii_lowercase().as_str() {
                "csv" => {
                    let book = vocab::load_book(ctx)?;
                    let mut list: Vec<vocab::Word> = book.words.into_values().collect();
                    list.sort_by(|a, b| b.added.cmp(&a.added));
                    let csv = vocab::to_csv(&list);
                    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&output, csv)?;
                    out.ok(&json!({ "format": "csv", "count": list.len(), "path": output }), || {
                        println!("Exported {} words to {}", list.len(), output.display());
                    });
                }
                "anki" | "apkg" => {
                    let count = vocab::export_anki_from_book(ctx, &output, &deck, &model, with_audio).await?;
                    out.ok(&json!({ "format": "anki", "count": count, "path": output }), || {
                        println!("Exported {count} Anki cards to {}", output.display());
                    });
                }
                other => return Err(CliError::usage(format!("unsupported vocab export format: {other}"))),
            }
        }
    }
    Ok(())
}

pub async fn run_tts(
    ctx: &CoreCtx,
    args: TtsArgs,
    sink: &dyn ProgressSink,
    out: Out,
) -> Result<(), CliError> {
    match args.action {
        TtsAction::Speak { text, engine, voice, rate, model, output } => {
            let req = settings::resolve_tts_request(
                ctx,
                &text,
                engine.as_deref(),
                voice.as_deref(),
                rate,
                model.as_deref(),
            )?;
            let res = speech::synthesize(&ctx.data, req).await?;
            let path = if let Some(dest) = output {
                if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&res.path, &dest)?;
                dest.to_string_lossy().into_owned()
            } else {
                res.path.clone()
            };
            out.ok(&json!({ "path": path, "seconds": res.seconds }), || {
                println!("Synthesized ({:.1}s): {path}", res.seconds);
            });
        }
        TtsAction::Voices => {
            let voices = speech::list_system_voices();
            out.ok(&voices, || {
                for v in &voices {
                    println!("{:<28}  [{:<8}]  {}", v.id, v.lang, v.name);
                }
            });
        }
        TtsAction::Status => {
            let st = local_tts::status(&ctx.data);
            out.ok(&st, || {
                println!("Supported: {}  Runtime: {}", st.supported, st.runtime);
                for m in &st.models {
                    println!("  {:<8}  installed={}  ({} MB)", m.id, m.installed, m.bytes / 1_048_576);
                }
            });
        }
        TtsAction::Install { model, no_mirror } => {
            ops::local_tts_install(ctx, &model, !no_mirror, sink).await?;
            out.ok(&json!({ "installed": model }), || println!("Installed local TTS model: {model}"));
        }
        TtsAction::Delete { model } => {
            local_tts::delete(&ctx.data, &model)?;
            out.ok(&json!({ "deleted": model }), || println!("Deleted local TTS model: {model}"));
        }
        TtsAction::ClearCache => {
            speech::clear_cache(&ctx.data)?;
            out.ok(&json!({ "cleared": true }), || println!("Cleared TTS cache"));
        }
    }
    Ok(())
}

pub async fn run_shadow(ctx: &CoreCtx, args: ShadowArgs, out: Out) -> Result<(), CliError> {
    let eff = settings::load_effective(ctx)?;
    let default_model = eff["asrModel"].as_str().unwrap_or("qwen3-asr").to_string();
    match args.action {
        ShadowAction::Ready { model } => {
            let wanted = model.as_deref().unwrap_or(&default_model);
            let ready = shadow::ready(&ctx.data, wanted);
            out.ok(&json!({ "model": wanted, "ready": ready }), || {
                println!("Shadowing model {wanted}: {}", if ready { "ready" } else { "not installed" });
            });
        }
        ShadowAction::Record { seconds, model, language } => {
            let wanted = model.as_deref().unwrap_or(&default_model);
            shadow::clear();
            shadow::start(|_| {})?;
            tokio::time::sleep(std::time::Duration::from_secs(seconds.clamp(1, 60))).await;
            shadow::stop();
            let text = shadow::hear(&ctx.data, wanted, &language).await?;
            out.ok(&json!({ "text": text, "seconds": seconds }), || println!("{text}"));
        }
        ShadowAction::Clear => {
            shadow::clear();
            out.ok(&json!({ "cleared": true }), || println!("Cleared shadow audio buffer"));
        }
    }
    Ok(())
}
