//! Whisper's text filters and your corrections, ported from asr.py: what the transcriber drops as invented
//! (noise, loops) and how `ozen fix` corrections rewrite a line. Also flags old lines these filters would have
//! dropped, in junk.json, which the panel, `ozen mcp` and the meeting export hide.
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::fs;

/// Python's `\w` on str: letters, digits and `_`.
fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// asr.py's `words_of`: lowercase, punctuation to spaces, split.
pub fn words_of(text: &str) -> Vec<String> {
    text.to_lowercase()
        .chars()
        .map(|c| {
            if word_char(c) || c.is_whitespace() {
                c
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// What Whisper invents on noise, per language. ivrit.ai's Hebrew model is trained heavily on Knesset
/// recordings: on clicks and bumps it answers with the parliamentary openers.
const NOISE: &[&str] = &[
    "thank you",
    "thanks",
    "you",
    "bye", // en
    "תודה",
    "תודה רבה",
    "רבה",
    "תודה לכם",
    "ביי",
    "חברי הכנסת",
    "חברות וחברי הכנסת",
    "חברות וחברות הכנסת",
    "אדוני היושב-ראש",
    "גבירתי היושבת-ראש", // he
];

fn filler() -> HashSet<String> {
    NOISE.iter().flat_map(|p| words_of(p)).collect()
}

/// "תודה. תודה רבה." on silence: every word is known filler (any language).
pub fn noise(text: &str) -> bool {
    let f = filler();
    words_of(text).iter().all(|w| f.contains(w))
}

/// "Amen. Amen. Amen. Amen." on noise: one phrase said 3+ times that makes up most of the line.
pub fn looped(text: &str) -> bool {
    let phrases: Vec<String> = text
        .split(['.', ',', '!', '?', ';', ':'])
        .map(|p| p.trim().to_lowercase())
        .filter(|p| !p.is_empty())
        .collect();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for p in &phrases {
        *counts.entry(p).or_default() += 1;
    }
    let top = counts.values().copied().max().unwrap_or(0);
    top >= 3 && top as f64 / phrases.len() as f64 >= 0.6
}

/// Applies your repeated corrections (learned.json "replace"), whole words only, in order.
pub fn corrected(text: &str, replace: &serde_json::Map<String, Value>) -> String {
    let mut text = text.to_string();
    for (wrong, right) in replace {
        let Some(right) = right.as_str() else {
            continue;
        };
        if wrong.is_empty() {
            continue;
        }
        let (mut out, mut i) = (String::new(), 0);
        while let Some(off) = text[i..].find(wrong.as_str()) {
            let (s, e) = (i + off, i + off + wrong.len());
            let whole = !text[..s].chars().next_back().is_some_and(word_char)
                && !text[e..].chars().next().is_some_and(word_char);
            if whole {
                out.push_str(&text[i..s]);
                out.push_str(right);
                i = e;
            } else {
                // not a whole word: search again one character on, as re.sub does
                let step = s + text[s..].chars().next().map_or(1, char::len_utf8);
                out.push_str(&text[i..step]);
                i = step;
            }
        }
        out.push_str(&text[i..]);
        text = out;
    }
    text
}

/// Lines written before the filters existed that they would have dropped: filler, phrase loops, and Whisper
/// reading back the people's names that used to be in its prompt ("אורן דן, בן נחושתן, תודה רבה."). Their ids go
/// to junk.json; lines.jsonl is untouched, so deleting junk.json brings them back.
pub fn mark_junk() {
    let names: HashSet<String> = fs::read_dir("voices/voices")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|f| serde_json::from_slice::<Value>(&fs::read(f.path()).ok()?).ok())
        .filter(|v| v["model"] == crate::train::ECAPA)
        .filter_map(|v| v["name"].as_str().map(words_of))
        .flatten()
        .collect();
    let Ok(raw) = fs::read_to_string("lines.jsonl") else {
        return;
    };
    let ids: Vec<Value> = raw
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|r| {
            junk(
                r["heard"].as_str().or(r["text"].as_str()).unwrap_or(""),
                &names,
            )
        })
        .map(|r| r["id"].clone())
        .collect();
    let _ = fs::write("junk.json", Value::from(ids).to_string());
}

fn junk(text: &str, names: &HashSet<String>) -> bool {
    let words: HashSet<String> = words_of(text).into_iter().collect();
    let f = filler();
    noise(text)
        || looped(text)
        || (words.iter().any(|w| names.contains(w))
            && words.iter().all(|w| names.contains(w) || f.contains(w)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn filters_match_asr_py() {
        assert_eq!(words_of("Hello, World!  x_y"), ["hello", "world", "x_y"]);
        assert!(noise("תודה. תודה רבה.") && noise("Thank you.") && noise(""));
        assert!(!noise("Thank you, Kev."));
        // asr.py's doctest
        assert!(looped("Amen. Amen. Amen. Amen. Yeah."));
        assert!(!looped("Yeah. Yeah.") && !looped("זה קורה קורה קורה."));
    }

    #[test]
    fn corrections_replace_whole_words_only() {
        let r = json!({"cave": "Kev", "p r": "PR"});
        let r = r.as_object().unwrap();
        assert_eq!(corrected("cave caves cave, p r.", r), "Kev caves Kev, PR.");
        assert_eq!(corrected("caveman cave", r), "caveman Kev"); // retries past a rejected match
        assert_eq!(corrected("שלום cave", r), "שלום Kev");
    }

    #[test]
    fn junk_is_filler_loops_and_names_read_back() {
        let names: HashSet<String> = ["אורן", "דן"].map(String::from).into();
        assert!(junk("אורן דן, תודה רבה.", &names));
        assert!(!junk("דן, תביא את הקובץ", &names));
        assert!(junk("Okay. Okay. Okay.", &names));
    }
}
