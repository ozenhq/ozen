//! Every recorded chunk and whether it's transcribed yet, for the menu bar's Timebar window (src/bin/bar/timebar.rs)
//! and `ozen timebar`.
//!
//! A chunk is one 15s stream file (`<start ms>-<call|mic|local>.wav`). Waiting ones are the files still in
//! `chunks/`; done ones come from `pace.jsonl`, which the transcriber appends after each chunk (when, how long
//! it took, lines written, or the error that skipped it). Chunks from before that log existed show as done at
//! an unknown time when they produced a line, since line ids start with their chunk's name.
use crate::fixes::lines;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::fs;

const PACE: &str = "pace.jsonl";

/// Chunks sorted by start, each `{t, tag, state}` where state is done | waiting | error | old, plus
/// `sec, done, took, lines` (and `error`) from the transcriber's log when it has them.
fn build(pace: &str, line_ids: &[&str], waiting: &[String]) -> Vec<Value> {
    let mut by: BTreeMap<(i64, String), Map<String, Value>> = BTreeMap::new();
    let key = |name: &str| -> Option<(i64, String)> {
        let mut parts = name.splitn(3, '-');
        Some((parts.next()?.parse().ok()?, parts.next()?.to_string()))
    };
    let row = |(ms, tag): &(i64, String), state: &str| {
        let mut m = Map::new();
        m.insert("t".into(), json!(*ms as f64 / 1000.0));
        m.insert("tag".into(), json!(tag));
        m.insert("state".into(), json!(state));
        m
    };
    for id in line_ids {
        if let Some(k) = key(id) {
            by.entry(k.clone()).or_insert_with(|| row(&k, "old"));
        }
    }
    for name in waiting {
        if let Some(k) = key(name.trim_end_matches(".wav")) {
            by.insert(k.clone(), row(&k, "waiting"));
        }
    }
    // Last, so a chunk logged as done wins over its file lingering a moment before the transcriber deletes it.
    for l in pace.lines() {
        let Ok(p) = serde_json::from_str::<Map<String, Value>>(l) else {
            continue;
        };
        let (Some(ms), Some(tag)) = (
            p.get("ms").and_then(Value::as_i64),
            p.get("tag").and_then(Value::as_str),
        ) else {
            continue;
        };
        let k = (ms, tag.to_string());
        let mut m = row(
            &k,
            if p.contains_key("error") {
                "error"
            } else {
                "done"
            },
        );
        for f in ["sec", "done", "took", "lines", "error"] {
            if let Some(v) = p.get(f) {
                m.insert(f.into(), v.clone());
            }
        }
        by.insert(k, m);
    }
    by.into_values().map(Value::Object).collect()
}

pub fn json() -> Value {
    let waiting: Vec<String> = fs::read_dir("chunks").map_or(vec![], |d| {
        d.flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".wav"))
            .collect()
    });
    let rows = lines();
    let ids: Vec<&str> = rows.iter().filter_map(|r| r.get("id")?.as_str()).collect();
    let pace = fs::read_to_string(PACE).unwrap_or_default();
    json!({"now": crate::now(), "chunks": build(&pace, &ids, &waiting)})
}

#[cfg(test)]
#[path = "timebar_tests.rs"]
mod tests;
