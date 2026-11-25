//! The word book as an Anki deck package (.apkg): double-click to import, each card with the sentence in the
//! speaker's own voice. Written in Anki's long-standing collection format (schema 11), which every current Anki
//! (desktop, AnkiMobile, AnkiDroid) imports. Note ids (guid) come from the word, so importing again updates the
//! notes instead of adding copies.
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::json;
use sha1::{Digest, Sha1};

/// Fixed, so a second import lands in the same deck and note type.
const MODEL_ID: i64 = 1_731_900_000_101;
const DECK_ID: i64 = 1_731_900_000_202;
const FIELDS: [&str; 8] = ["Word", "IPA", "Meaning", "Context", "Sentence", "Translation", "Source", "Audio"];

/// One note: HTML-ready fields (the interface escapes them), and the clip to attach, if any.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Note {
    pub id: String,
    pub word: String,
    pub ipa: String,
    pub meaning: String,
    pub context: String,
    pub sentence: String,
    pub translation: String,
    pub source: String,
    pub tags: String,
    pub clip: Option<String>,
}

const FRONT: &str = r#"<div class="word">{{Word}}</div>
<div class="sentence">{{Sentence}}</div>
{{tts en_US:Word}} {{Audio}}"#;
const BACK: &str = r#"{{FrontSide}}
<hr id="answer">
{{#IPA}}<div class="ipa">/{{IPA}}/</div>{{/IPA}}
<div class="meaning">{{Meaning}}</div>
{{#Context}}<div class="context">{{Context}}</div>{{/Context}}
{{#Translation}}<div class="translation">{{Translation}}</div>{{/Translation}}
<div class="source">{{Source}}</div>"#;
const CSS: &str = r#".card { font-family: -apple-system, "PingFang SC", "Helvetica Neue", sans-serif; font-size: 18px; line-height: 1.55; text-align: center; color: #1d1d1f; background: #fff; padding: 12px; }
.word { font-size: 34px; font-weight: 700; letter-spacing: -.01em; }
.sentence { margin: 14px auto 0; max-width: 34em; color: #48484a; }
.sentence b { color: #e8582a; font-weight: 600; }
.ipa { color: #8e8e93; margin-top: 4px; }
.meaning { margin: 10px auto 0; max-width: 30em; }
.context { margin: 10px auto 0; max-width: 30em; color: #48484a; font-size: 16px; }
.translation { margin: 10px auto 0; max-width: 34em; color: #8e8e93; font-size: 15px; }
.source { margin-top: 14px; color: #aeaeb2; font-size: 12px; }
.nightMode.card, .night_mode .card { color: #f2f2f7; background: #1c1c1e; }
.nightMode .sentence, .night_mode .sentence, .nightMode .context, .night_mode .context { color: #d1d1d6; }
.nightMode .sentence b, .night_mode .sentence b { color: #ff7a45; }"#;

fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut tag = false;
    for c in s.chars() { match c { '<' => tag = true, '>' => tag = false, _ if !tag => out.push(c), _ => {} } }
    out.replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'")
}

fn checksum(first: &str) -> i64 {
    let h = Sha1::digest(strip_html(first).as_bytes());
    i64::from(u32::from_be_bytes([h[0], h[1], h[2], h[3]]))
}

/// Writes the package and returns how many notes have their sentence audio.
pub fn write(out: &Path, deck: &str, model: &str, notes: &[Note]) -> Result<usize> {
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_millis() as i64;
    let now = now_ms / 1000;
    let work = std::env::temp_dir().join(format!("subcast-anki-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&work)?;
    let result = (|| -> Result<usize> {
        let db_path = work.join("collection.anki2");
        let conn = rusqlite::Connection::open(&db_path)?;
        conn.execute_batch(SCHEMA)?;
        let flds: Vec<_> = FIELDS.iter().enumerate().map(|(i, n)| json!({ "name": n, "ord": i, "sticky": false, "rtl": false, "font": "Arial", "size": 20, "media": [] })).collect();
        let models = json!({ MODEL_ID.to_string(): {
            "id": MODEL_ID, "name": model, "type": 0, "mod": now, "usn": -1, "sortf": 0, "did": DECK_ID,
            "tmpls": [{ "name": "Card 1", "ord": 0, "qfmt": FRONT, "afmt": BACK, "did": null, "bqfmt": "", "bafmt": "" }],
            "flds": flds, "css": CSS,
            "latexPre": "\\documentclass[12pt]{article}\n\\special{papersize=3in,5in}\n\\usepackage[utf8]{inputenc}\n\\usepackage{amssymb,amsmath}\n\\pagestyle{empty}\n\\setlength{\\parindent}{0in}\n\\begin{document}\n",
            "latexPost": "\\end{document}", "latexsvg": false, "req": [[0, "any", [0]]], "tags": [], "vers": [],
        }});
        let deck_json = |id: i64, name: &str| json!({ "id": id, "name": name, "desc": "", "mod": now, "usn": -1, "collapsed": false, "browserCollapsed": false,
            "newToday": [0, 0], "revToday": [0, 0], "lrnToday": [0, 0], "timeToday": [0, 0], "dyn": 0, "extendNew": 10, "extendRev": 50, "conf": 1 });
        let decks = json!({ "1": deck_json(1, "Default"), DECK_ID.to_string(): deck_json(DECK_ID, deck) });
        let dconf = json!({ "1": { "id": 1, "name": "Default", "mod": 0, "usn": 0, "maxTaken": 60, "autoplay": true, "timer": 0, "replayq": true, "dyn": false,
            "new": { "delays": [1, 10], "ints": [1, 4, 7], "initialFactor": 2500, "separate": true, "order": 1, "perDay": 20, "bury": true },
            "lapse": { "delays": [10], "mult": 0, "minInt": 1, "leechFails": 8, "leechAction": 0 },
            "rev": { "perDay": 200, "ease4": 1.3, "fuzz": 0.05, "minSpace": 1, "ivlFct": 1, "maxIvl": 36500, "bury": true } } });
        let conf = json!({ "activeDecks": [1], "curDeck": 1, "newSpread": 0, "collapseTime": 1200, "timeLim": 0, "estTimes": true, "dueCounts": true,
            "curModel": null, "nextPos": notes.len() + 1, "sortType": "noteFld", "sortBackwards": false, "addToCur": true });
        conn.execute("INSERT INTO col VALUES (1,?1,?2,?2,11,0,0,0,?3,?4,?5,?6,'{}')",
            rusqlite::params![now - now % 86400, now_ms, conf.to_string(), models.to_string(), decks.to_string(), dconf.to_string()])?;

        let mut media = serde_json::Map::new();
        let mut files: Vec<(String, PathBuf)> = vec![];
        for (i, n) in notes.iter().enumerate() {
            let mut audio = String::new();
            if let Some(clip) = n.clip.as_deref().map(PathBuf::from).filter(|p| p.is_file()) {
                let name = format!("subcast-{}", clip.file_name().and_then(|f| f.to_str()).unwrap_or("clip.wav"));
                audio = format!("[sound:{name}]");
                let key = files.len().to_string();
                media.insert(key.clone(), json!(name));
                files.push((key, clip));
            }
            let fields = [&n.word, &n.ipa, &n.meaning, &n.context, &n.sentence, &n.translation, &n.source, &audio];
            let flds = fields.iter().map(|f| f.replace('\u{1f}', " ")).collect::<Vec<_>>().join("\u{1f}");
            let nid = now_ms + i as i64;
            conn.execute("INSERT INTO notes VALUES (?1,?2,?3,?4,-1,?5,?6,?7,?8,0,'')",
                rusqlite::params![nid, format!("subcast:{}", n.id), MODEL_ID, now, format!(" {} ", n.tags.trim()), flds, strip_html(&n.word), checksum(&n.word)])?;
            conn.execute("INSERT INTO cards VALUES (?1,?2,?3,0,?4,-1,0,0,?5,0,0,0,0,0,0,0,0,'')",
                rusqlite::params![nid, nid, DECK_ID, now, i as i64 + 1])?;
        }
        drop(conn);

        let file = std::fs::File::create(out).context("不能写入这个位置")?;
        let mut zip = zip::ZipWriter::new(file);
        let deflate = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let stored = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("collection.anki2", deflate)?;
        zip.write_all(&std::fs::read(&db_path)?)?;
        zip.start_file("media", deflate)?;
        zip.write_all(serde_json::Value::Object(media).to_string().as_bytes())?;
        for (key, path) in &files {
            zip.start_file(key.as_str(), stored)?;
            zip.write_all(&std::fs::read(path)?)?;
        }
        zip.finish()?;
        Ok(files.len())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

const SCHEMA: &str = "
CREATE TABLE col (id integer primary key, crt integer not null, mod integer not null, scm integer not null, ver integer not null, dty integer not null, usn integer not null, ls integer not null, conf text not null, models text not null, decks text not null, dconf text not null, tags text not null);
CREATE TABLE notes (id integer primary key, guid text not null, mid integer not null, mod integer not null, usn integer not null, tags text not null, flds text not null, sfld integer not null, csum integer not null, flags integer not null, data text not null);
CREATE TABLE cards (id integer primary key, nid integer not null, did integer not null, ord integer not null, mod integer not null, usn integer not null, type integer not null, queue integer not null, due integer not null, ivl integer not null, factor integer not null, reps integer not null, lapses integer not null, left integer not null, odue integer not null, odid integer not null, flags integer not null, data text not null);
CREATE TABLE revlog (id integer primary key, cid integer not null, usn integer not null, ease integer not null, ivl integer not null, lastIvl integer not null, factor integer not null, time integer not null, type integer not null);
CREATE TABLE graves (usn integer not null, oid integer not null, type integer not null);
CREATE INDEX ix_notes_usn on notes (usn);
CREATE INDEX ix_cards_usn on cards (usn);
CREATE INDEX ix_revlog_usn on revlog (usn);
CREATE INDEX ix_cards_nid on cards (nid);
CREATE INDEX ix_cards_sched on cards (did, queue, due);
CREATE INDEX ix_revlog_cid on revlog (cid);
CREATE INDEX ix_notes_csum on notes (csum);
";

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn writes_a_package() {
        let dir = std::env::temp_dir().join(format!("subcast-anki-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let clip = dir.join("abc.wav");
        crate::asr::write_wav(&clip, &[0.0; 1600]).unwrap();
        let note = |id: &str, clip: Option<String>| Note { id: id.into(), word: id.into(), ipa: "sə'vænə".into(), meaning: "n. 热带草原".into(), context: String::new(),
            sentence: format!("in the <b>{id}</b>, then"), translation: "在草原上".into(), source: "Show · Ep · 22:47".into(), tags: "subcast 六级".into(), clip };
        let out = dir.join("t.apkg");
        let n = write(&out, "SubCast 生词本", "SubCast 单词", &[note("savanna", Some(clip.to_string_lossy().into())), note("gist", None)]).unwrap();
        assert_eq!(n, 1);
        let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        let mut media = String::new();
        std::io::Read::read_to_string(&mut z.by_name("media").unwrap(), &mut media).unwrap();
        assert_eq!(media, r#"{"0":"subcast-abc.wav"}"#);
        assert!(z.by_name("0").is_ok() && z.by_name("collection.anki2").is_ok());
        assert_eq!(strip_html("in the <b>x</b> &amp; y"), "in the x & y");
        if let Ok(keep) = std::env::var("ANKI_OUT") { std::fs::copy(&out, keep).unwrap(); }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
