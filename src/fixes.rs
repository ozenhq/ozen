//! Transcript fixes and what ozen learns from them, plus the transcript view that shows them.
//!
//! `ozen fix <line-id> "right text"` stores the fix in fixes.json (empty text clears it) and relearns
//! learned.json, which the transcriber reads live: words the fixes added go into Whisper's prompt so it
//! spells them right, and a correction made FIX_REPEAT times is applied to new lines automatically.
//! Each fix also keeps its chunk audio (fixes/) with the right text, for fine-tuning a model later.
use regex::Regex;
use serde_json::{Map, Value, json};
use similar::{Algorithm, DiffTag, capture_diff_slices};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

const LINES: &str = "lines.jsonl";
const FIXES: &str = "fixes.json";
const LEARNED: &str = "learned.json";
const FIX_AUDIO: &str = "fixes";
const FIX_REPEAT: usize = 2; // the same correction this many times becomes an automatic replacement
const LEARNED_VOCAB: usize = 30; // Whisper's prompt is ~220 tokens, shared with vocab.txt, names and the previous line

static WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\w+(?:['"״׳]\w+)*"#).unwrap()); // ג'ירה, צה"ל stay whole

fn read(path: &str) -> Map<String, Value> {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write(path: &str, v: &Value) {
    fs::write(path, serde_json::to_string_pretty(v).expect("json") + "\n")
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
}

/// Transcript lines in file order (only lines with a voiceprint, like train.py).
fn lines() -> Vec<Map<String, Value>> {
    fs::read_to_string(LINES)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Map<String, Value>>(l).ok())
        .filter(|r| r.get("e").is_some_and(|e| !e.is_null()))
        .collect()
}

/// Whisper's own text, before automatic corrections (the transcriber keeps it as "heard" when they changed it).
fn heard(r: &Map<String, Value>) -> &str {
    r.get("heard")
        .and_then(Value::as_str)
        .unwrap_or(str_of(r, "text"))
}

fn str_of<'a>(r: &'a Map<String, Value>, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or("")
}

/// Count keeping first-seen order, so ties rank by which came first.
fn bump(counts: &mut Vec<(String, usize)>, key: String) {
    match counts.iter_mut().find(|(k, _)| *k == key) {
        Some((_, n)) => *n += 1,
        None => counts.push((key, 1)),
    }
}

/// What (heard, right) pairs teach: the words the fixes added, most used first, and each correction made
/// FIX_REPEAT+ times (the most common one, when a phrase was fixed different ways). A correction is
/// skipped while any fixed line still keeps the "wrong" phrase: there it was right, so replacing it is unsafe.
pub fn rules(pairs: &[(&str, &str)]) -> Value {
    let (mut added, mut fixed) = (vec![], vec![]);
    for (heard, right) in pairs {
        let a: Vec<&str> = WORD.find_iter(heard).map(|m| m.as_str()).collect();
        let b: Vec<&str> = WORD.find_iter(right).map(|m| m.as_str()).collect();
        for op in capture_diff_slices(Algorithm::Myers, &a, &b) {
            let (tag, old, new) = op.as_tag_tuple();
            if matches!(tag, DiffTag::Replace | DiffTag::Insert) {
                for w in b[new.clone()].iter().filter(|w| w.chars().count() > 1) {
                    bump(&mut added, w.to_string());
                }
            }
            if tag == DiffTag::Replace {
                bump(
                    &mut fixed,
                    format!("{}\t{}", a[old].join(" "), b[new].join(" ")),
                );
            }
        }
    }
    added.sort_by_key(|x| std::cmp::Reverse(x.1)); // stable: ties keep first-seen order
    fixed.sort_by_key(|x| std::cmp::Reverse(x.1));
    let mut replace = Map::new();
    for (pair, _) in fixed.into_iter().filter(|(_, n)| *n >= FIX_REPEAT) {
        let (wrong, right) = pair.split_once('\t').unwrap();
        let kept = pairs.iter().any(|(_, r)| {
            WORD.find_iter(r)
                .map(|m| m.as_str())
                .collect::<Vec<_>>()
                .windows(wrong.split(' ').count())
                .any(|w| w.join(" ") == wrong)
        });
        if !kept {
            replace.entry(wrong).or_insert(json!(right));
        }
    }
    let vocab: Vec<String> = added
        .into_iter()
        .take(LEARNED_VOCAB)
        .map(|(w, _)| w)
        .collect();
    json!({"vocab": vocab, "replace": replace})
}

/// Chunk file of a line id "<ms>-<source>-<n>", and the ms it starts at.
fn chunk_of(id: &str) -> Option<(String, f64)> {
    let mut p = id.splitn(3, '-');
    let (ms, src) = (p.next()?, p.next()?);
    Some((format!("{ms}-{src}.wav"), ms.parse::<f64>().ok()? / 1000.0))
}

pub fn fix(id: &str, text: &str) -> Result<(), String> {
    let all = lines();
    let line = all
        .iter()
        .find(|r| str_of(r, "id") == id)
        .ok_or(format!("no line {id}"))?;
    let mut fixes = read(FIXES);
    if !text.is_empty() && text != str_of(line, "text") {
        fixes.insert(id.into(), json!(text));
        // recent/ rotates: keep the audio while it's still there
        if let Some((chunk, _)) = chunk_of(id)
            && Path::new("recent").join(&chunk).exists()
        {
            fs::create_dir_all(FIX_AUDIO).map_err(|e| e.to_string())?;
            fs::copy(
                Path::new("recent").join(&chunk),
                Path::new(FIX_AUDIO).join(&chunk),
            )
            .map_err(|e| e.to_string())?;
        }
    } else {
        fixes.remove(id);
    }
    write(FIXES, &Value::Object(fixes.clone()));

    let by_id = |s: &str| all.iter().find(|r| str_of(r, "id") == s);
    // Learn from what Whisper heard, before automatic corrections: undoing a wrong one then just cancels it.
    let pairs: Vec<(&str, &str)> = fixes
        .iter()
        .filter_map(|(s, t)| Some((heard(by_id(s)?), t.as_str()?)))
        .collect();
    let learned = rules(&pairs);
    write(LEARNED, &learned);

    if Path::new(FIX_AUDIO).exists() {
        // the line's span inside its kept chunk, with the right text
        let rows: String = fixes
            .iter()
            .filter_map(|(s, t)| {
                let (chunk, start) = chunk_of(s)?;
                let r = by_id(s)?;
                let offset = r.get("t")?.as_f64()? - start;
                let row = json!({"audio": chunk, "start": (offset * 100.0).round() / 100.0,
                                 "duration": r.get("d"), "text": t});
                Path::new(FIX_AUDIO)
                    .join(&chunk)
                    .exists()
                    .then(|| row.to_string() + "\n")
            })
            .collect();
        fs::write(Path::new(FIX_AUDIO).join("dataset.jsonl"), rows).map_err(|e| e.to_string())?;
    }
    println!(
        "{}",
        json!({"fixes": fixes.len(), "vocab": learned["vocab"].as_array().map_or(0, Vec::len),
                          "replace": learned["replace"]})
    );
    Ok(())
}

/// Last n lines, time-ordered, with tagged/relabeled speakers and fixed text.
pub fn show(n: usize) {
    let (tags, labels, fixes) = (read("tags.json"), read("labels.json"), read(FIXES));
    let mut all = lines();
    all.sort_by(|a, b| {
        a["t"]
            .as_f64()
            .partial_cmp(&b["t"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let tz = jiff::tz::TimeZone::system();
    for r in &all[all.len().saturating_sub(n)..] {
        let id = str_of(r, "id");
        let t = r["t"].as_f64().unwrap_or(0.0);
        let ts = jiff::Timestamp::from_millisecond((t * 1000.0) as i64)
            .map(|s| s.to_zoned(tz.clone()).strftime("%H:%M:%S").to_string())
            .unwrap_or_default();
        let tag = tags
            .get(id)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        let label = labels.get(id);
        let guess = label.and_then(|l| l["spk"].as_str());
        let spk = tag.or(guess).unwrap_or(str_of(r, "spk"));
        let mark = if tag.is_some() {
            " ✓"
        } else if label.is_some_and(|l| l["unsure"] == true) {
            " ?"
        } else {
            ""
        };
        let text = fixes
            .get(id)
            .and_then(Value::as_str)
            .unwrap_or(str_of(r, "text"));
        println!("[{ts}] {spk}{mark} ({}): {text}", str_of(r, "src"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learns_added_words_and_repeated_corrections() {
        let r = rules(&[
            ("נפתח קב על זה", "נפתח ג'ירה על זה"),
            ("תשאל את קב, בסדר", "תשאל את Kev, בסדר"),
            ("ה-פי אר מוכן", "ה-PR מוכן"),
            ("פי אר חדש", "PR חדש"),
        ]);
        assert_eq!(r["replace"], json!({"פי אר": "PR"})); // קב was fixed once each way: no rule
        assert_eq!(r["vocab"], json!(["PR", "ג'ירה", "Kev"]));
    }

    #[test]
    fn no_replacement_where_a_fix_kept_the_phrase() {
        let r = rules(&[
            ("קב אמר", "Kev אמר"),
            ("שאלתי את קב", "שאלתי את Kev"),
            ("קב הזמן", "קב הזמן, בדיוק"),
        ]);
        assert_eq!(r["replace"], json!({}));
        let r = rules(&[("קב אמר", "Kev אמר"), ("שאלתי את קב", "שאלתי את Kev")]);
        assert_eq!(r["replace"], json!({"קב": "Kev"}));
    }
}
