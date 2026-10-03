//! The voices ozen knows, for the menu bar's Voices window and `ozen voices`.
//!
//! Three kinds: people (named by your tags or matched to their voiceprint), the voices you ignored,
//! and this transcriber run's unnamed speakers (S1, S2...), which only mean something within one run.
//! Renaming onto an existing name merges the two; forgetting clears the tags on this Mac only (the
//! shared registry drops the person when no Mac tags them any more).
use crate::fixes::{lines, read, write};
use crate::ignore::is_ignored;
use serde_json::{Map, Value, json};

const TAGS: &str = "tags.json";
const LABELS: &str = "labels.json";
const FIXES: &str = "fixes.json";
const RECENT: usize = 3; // lines shown per voice, newest first

type Row = Map<String, Value>;

pub(crate) fn anon(name: &str) -> bool {
    name.len() > 1 && name.starts_with('S') && name[1..].bytes().all(|b| b.is_ascii_digit())
}

/// Who a line is shown as: your tag, else its voiceprint match, else the transcriber's label.
pub(crate) fn speaker<'a>(r: &'a Row, tags: &'a Row, labels: &'a Row) -> &'a str {
    let id = r.get("id").and_then(Value::as_str).unwrap_or("");
    tags.get(id)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| labels.get(id).and_then(|l| l["spk"].as_str()))
        .or_else(|| r.get("spk").and_then(Value::as_str))
        .unwrap_or("?")
}

/// One entry per voice: people by line count, then this run's unnamed speakers, then the ignored ones.
fn summarize(rows: &[Row], tags: &Row, labels: &Row, fixes: &Row) -> Vec<Value> {
    let run = rows.iter().filter_map(|r| r.get("run")?.as_i64()).max();
    let mut by: Vec<(String, &'static str, Vec<&Row>)> = Vec::new();
    for r in rows {
        let name = speaker(r, tags, labels);
        let kind = match name {
            "?" => continue,
            n if is_ignored(n) => "ignored",
            // S1 in an earlier run was a different voice: only this run's unnamed speakers are listed.
            n if anon(n) && (run.is_none() || r.get("run").and_then(Value::as_i64) != run) => {
                continue;
            }
            n if anon(n) => "unnamed",
            _ => "person",
        };
        match by.iter_mut().find(|(n, _, _)| n == name) {
            Some((_, _, v)) => v.push(r),
            None => by.push((name.to_string(), kind, vec![r])),
        }
    }
    let order = |k: &str| {
        ["person", "unnamed", "ignored"]
            .iter()
            .position(|x| *x == k)
    };
    by.sort_by(|a, b| {
        order(a.1)
            .cmp(&order(b.1))
            .then(b.2.len().cmp(&a.2.len()))
            .then(a.0.cmp(&b.0))
    });
    by.into_iter()
        .map(|(name, kind, mut v)| {
            v.sort_by(|a, b| {
                b["t"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&a["t"].as_f64().unwrap_or(0.0))
            });
            let id = |r: &Row| r["id"].as_str().unwrap_or("").to_string();
            let recent: Vec<Value> = v
                .iter()
                .take(RECENT)
                .map(|r| {
                    let text = fixes
                        .get(&id(r))
                        .and_then(Value::as_str)
                        .unwrap_or(r["text"].as_str().unwrap_or(""));
                    json!({"id": id(r), "t": r["t"], "text": text})
                })
                .collect();
            let mut out = json!({"name": name, "kind": kind, "lines": v.len(), "recent": recent});
            if kind == "unnamed" {
                out["ids"] = json!(v.iter().map(|r| id(r)).collect::<Vec<_>>()); // naming or ignoring one tags these
            }
            out
        })
        .collect()
}

pub fn list() -> Vec<Value> {
    summarize(&lines(), &read(TAGS), &read(LABELS), &read(FIXES))
}

/// Replace every tag `from` with `to` (empty clears them) in tags.json. The caller retrains.
pub fn retag(from: &str, to: &str) -> Result<usize, String> {
    let mut tags = read(TAGS);
    let mut n = 0;
    for v in tags.values_mut().filter(|v| v.as_str() == Some(from)) {
        *v = json!(to.trim());
        n += 1;
    }
    if n == 0 {
        return Err(format!("no lines are tagged {from:?}"));
    }
    write(TAGS, &Value::Object(tags));
    Ok(n)
}

#[cfg(test)]
#[path = "voices_tests.rs"]
mod tests;
