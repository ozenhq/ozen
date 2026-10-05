//! `ozen sync preview`: what turning sync on would share with this vault's other Macs, read from the
//! files here (crate::merge::read_synced, the same records sync sends) without connecting anywhere.
//! Users, and their employers, can see it before the first exchange; no server could tell them.
use crate::merge::{Synced, read_synced};
use serde_json::{Map, Value};

/// A deleted row: {"id", "v", "del": true} (crdt.rs), shared as a marker with no content.
fn row_gone(r: &Map<String, Value>) -> bool {
    r.get("del").and_then(Value::as_bool) == Some(true)
}

/// A deleted map entry: an entry object with a "v" but no "val" (crdt.rs). Plain values are live.
fn entry_gone(e: &Value) -> bool {
    e.as_object()
        .is_some_and(|o| o.contains_key("v") && !o.contains_key("val"))
}

/// The UTC day of a line's time (seconds since 1970).
fn date(t: f64) -> String {
    chrono::DateTime::from_timestamp(t as i64, 0).map_or("?".into(), |d| d.date_naive().to_string())
}

/// A line saying how many live entries `m` has, adding its deleted ones to `gone_n`.
fn map_line(one: &str, many: &str, m: &Map<String, Value>, gone_n: &mut usize) -> String {
    let deleted = m.values().filter(|v| entry_gone(v)).count();
    *gone_n += deleted;
    let k = m.len() - deleted;
    format!("- {k} {}", if k == 1 { one } else { many })
}

/// What a place does there, in words (places.rs `action`).
fn action(a: Option<&str>) -> &'static str {
    match a {
        Some("record") => "records everything",
        Some("meetings") => "records meetings",
        Some("off") => "doesn't record",
        _ => "no setting",
    }
}

/// The preview of `s`, as `ozen sync preview` prints it.
pub fn render(s: &Synced) -> String {
    let mut deleted = 0;
    let lines: Vec<&Map<String, Value>> = s.lines.iter().filter(|r| !row_gone(r)).collect();
    deleted += s.lines.len() - lines.len();
    let notes = lines
        .iter()
        .filter(|r| r.get("src").and_then(Value::as_str) == Some("note"))
        .count();
    let voiced = lines
        .iter()
        .filter(|r| r.get("e").is_some_and(|e| !e.is_null()))
        .count();
    let times: Vec<f64> = lines.iter().filter_map(|r| r.get("t")?.as_f64()).collect();
    let range = match (
        times.iter().copied().reduce(f64::min),
        times.iter().copied().reduce(f64::max),
    ) {
        (Some(a), Some(b)) => format!(", {} to {} (UTC)", date(a), date(b)),
        _ => String::new(),
    };
    let places: Vec<String> = s
        .places
        .iter()
        .filter(|p| !row_gone(p))
        .map(|p| {
            let label = p
                .get("label")
                .and_then(Value::as_str)
                .unwrap_or("(no name)");
            let does = action(p.get("action").and_then(Value::as_str));
            if p.get("lat").is_some_and(|l| !l.is_null()) {
                format!("{label} ({does}; with its map area)")
            } else {
                format!("{label} ({does})")
            }
        })
        .collect();
    deleted += s.places.len() - places.len();
    let n = |k: usize, one: &str, many: &str| format!("{k} {}", if k == 1 { one } else { many });
    let mut out = vec![
        "Turning on sync shares these with this vault's other Macs, and only them:".to_string(),
        format!(
            "- {}{range}, each with its full text, speaker name and timing. {} you wrote; {} a \
voiceprint (numbers describing the speaker's voice)",
            n(lines.len(), "transcript line", "transcript lines"),
            n(notes, "is a note", "are notes"),
            n(voiced, "carries", "carry"),
        ),
        map_line(
            "speaker name set on a line",
            "speaker names set on lines",
            &s.tags,
            &mut deleted,
        ),
        map_line(
            "correction of line text",
            "corrections of line text",
            &s.fixes,
            &mut deleted,
        ),
        map_line(
            "vocabulary word",
            "vocabulary words",
            &s.vocab,
            &mut deleted,
        ),
        if places.is_empty() {
            "- 0 places".into()
        } else {
            format!(
                "- {}: {}",
                n(places.len(), "place", "places"),
                places.join(", ")
            )
        },
    ];
    if deleted > 0 {
        out.push(format!(
            "- {}, sent without their text (a deleted place keeps its name)",
            n(deleted, "deleted record", "deleted records")
        ));
    }
    out.push(
        "Never shared: audio (chunks/, recent/, fixes/), the voiceprint registry (voices/), where this Mac \
is now (here.json), voices you ignore (ignore.json), what the transcriber learned (learned.json), \
transcript.txt, context/ copies, labels, stats, and logs."
            .into(),
    );
    out.push("This preview only read files here; nothing was sent.".into());
    out.join("\n")
}

/// `ozen sync preview`, for the ozen folder (the working directory).
pub fn preview() -> String {
    render(&read_synced(""))
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
