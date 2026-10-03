//! Voices to ignore: a video playing next to the Mac isn't part of the meeting.
//!
//! `ozen ignore <line-id>...` tags lines with the reserved name IGNORE. After every retrain, `apply` gathers
//! those lines' voiceprints into ignore.json (local only, never pushed to the voices registry), which the
//! transcriber reads live to drop that voice, and labels earlier untagged lines that sound like it.
//! Ignored prints are matched one by one, not averaged: a video has many voices.
//! Each ignore starts its own voice (IGNORE, "Ignored 2", "Ignored 3"...) so you can stop ignoring one alone.
use crate::fixes::{lines, read, write};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;

pub const IGNORE: &str = "Ignored"; // reserved tag, same in src/transcribe.rs, src/train.rs and src/bin/bar

/// IGNORE or one of its numbered voices ("Ignored 2"), same as src/bin/bar/voices.rs is_ignored.
pub fn is_ignored(name: &str) -> bool {
    name == IGNORE
        || name
            .strip_prefix(IGNORE)
            .and_then(|n| n.strip_prefix(' '))
            .is_some_and(|n| n.parse::<u32>().is_ok())
}

/// The first ignored voice name no line is tagged with yet: IGNORE, then "Ignored 2", "Ignored 3"...
pub fn fresh(tags: &Map<String, Value>) -> String {
    let used = |n: &str| tags.values().any(|v| v.as_str() == Some(n));
    (2..)
        .map(|i| format!("{IGNORE} {i}"))
        .find(|n| !used(n))
        .filter(|_| used(IGNORE))
        .unwrap_or_else(|| IGNORE.into())
}
/// Ignoring needs the same-voice threshold plus this: dropping someone's speech costs more than keeping noise.
/// At the bare threshold a different voice (0.43) was dropped; the ignored voice itself scores 0.87-0.90.
pub const MARGIN: f32 = 0.1; // src/transcribe.rs uses it
const TAGS: &str = "tags.json";
const LABELS: &str = "labels.json";
const STATS: &str = "stats.json";
const IGNORES: &str = "ignore.json";
const REGISTRY: &str = "voices"; // clone of the voices registry; src/train.rs writes the people's prints here
const ECAPA: &str = "speechbrain/spkrec-ecapa-voxceleb";
const DEFAULT_THRESHOLD: f32 = 0.4; // src/train.rs's until it has calibrated one

/// Tag `ids` as `name` (empty clears) in tags.json, keeping every other tag.
pub fn tag(ids: &[String], name: &str) {
    let mut tags = read(TAGS);
    for id in ids {
        tags.insert(id.clone(), name.trim().into());
    }
    write(TAGS, &Value::Object(tags));
}

fn unit(v: &Value) -> Option<Vec<f32>> {
    let v: Vec<f32> = v
        .as_array()?
        .iter()
        .map(|x| x.as_f64().unwrap_or(0.0) as f32)
        .collect();
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    (n > 0.0).then(|| v.iter().map(|x| x / n).collect())
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn best(prints: &[Vec<f32>], e: &[f32]) -> f32 {
    prints.iter().map(|p| dot(p, e)).fold(-1.0, f32::max)
}

/// Untagged lines that sound like an ignored line: at least threshold + MARGIN, and closer to it than to
/// any person. Returns (line id, the nearest ignored line's voice, similarity).
fn matches<'a>(
    lines: &[(String, Vec<f32>)],
    tags: &Map<String, Value>,
    ignored: &[(&'a str, Vec<f32>)],
    people: &[Vec<f32>],
    threshold: f32,
) -> Vec<(String, &'a str, f32)> {
    lines
        .iter()
        .filter(|(id, _)| !tags.contains_key(id))
        .filter_map(|(id, e)| {
            let (voice, near) = ignored
                .iter()
                .map(|(v, p)| (*v, dot(p, e)))
                .max_by(|a, b| a.1.total_cmp(&b.1))?;
            (near >= threshold + MARGIN && near > best(people, e))
                .then(|| (id.clone(), voice, near))
        })
        .collect()
}

/// People's prints and the same-voice threshold, as src/train.rs last wrote them to the registry.
fn registry() -> (Vec<Vec<f32>>, f32) {
    let people = fs::read_dir(format!("{REGISTRY}/voices"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|f| {
            let v = read(f.path().to_str()?);
            (v.get("model")?.as_str()? == ECAPA).then(|| unit(v.get("embedding")?))?
        })
        .collect();
    let threshold = read(&format!("{REGISTRY}/config.json"))
        .get("same_speaker")
        .and_then(Value::as_f64)
        .map_or(DEFAULT_THRESHOLD, |t| t as f32);
    (people, threshold)
}

/// Run after src/train.rs retrains: rewrite ignore.json and label lines that sound like an ignored voice (never
/// unsure, so Review skips them).
pub fn apply() {
    let tags = read(TAGS);
    // Keyed by id like src/train.rs: a line written twice (two transcribers on one chunk) counts once.
    let lines: Vec<(String, Vec<f32>)> = lines()
        .into_iter()
        .filter_map(|r| Some((r.get("id")?.as_str()?.to_string(), unit(r.get("e")?)?)))
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect();
    // The transcriber only drops these, so ignore.json keeps bare prints; labels name the voice.
    let ignored: Vec<(&str, Vec<f32>)> = lines
        .iter()
        .filter_map(|(id, e)| Some((tags.get(id)?.as_str().filter(|n| is_ignored(n))?, e.clone())))
        .collect();
    write(
        IGNORES,
        &json!(ignored.iter().map(|(_, e)| e).collect::<Vec<_>>()),
    );
    let (people, threshold) = registry();
    let found = matches(&lines, &tags, &ignored, &people, threshold);

    let mut labels = read(LABELS);
    let mut stats = read(STATS);
    for (id, voice, sim) in &found {
        let was_unsure = labels
            .get(id)
            .and_then(|l| l.get("unsure"))
            .and_then(Value::as_bool)
            == Some(true);
        if was_unsure && let Some(n) = stats.get("unsure").and_then(Value::as_u64) {
            stats.insert("unsure".into(), json!(n.saturating_sub(1)));
        }
        let sim = (sim * 1000.0).round() / 1000.0;
        labels.insert(
            id.clone(),
            json!({"spk": voice, "sim": sim, "margin": 0, "unsure": false}),
        );
    }
    stats.insert("ignored".into(), json!(ignored.len()));
    fs::write(LABELS, Value::Object(labels).to_string()).expect("write labels.json");
    write(STATS, &Value::Object(stats));
}

#[cfg(test)]
#[path = "ignore_tests.rs"]
mod tests;
