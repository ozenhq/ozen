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

fn map_line(name: &str, m: &Map<String, Value>, gone_n: &mut usize) -> String {
    let deleted = m.values().filter(|v| entry_gone(v)).count();
    *gone_n += deleted;
    format!("  {name:<7}{}", m.len() - deleted)
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
            if p.get("lat").is_some_and(|l| !l.is_null()) {
                format!("{label} (with its map location)")
            } else {
                label.to_string()
            }
        })
        .collect();
    deleted += s.places.len() - places.len();
    let mut out = vec![
        "Turning on sync shares these with this vault's other Macs, and only them:".to_string(),
        format!(
            "  lines  {}{range}: {notes} note{}; {voiced} carry the voice embedding of who spoke",
            lines.len(),
            if notes == 1 { "" } else { "s" }
        ),
        map_line("tags", &s.tags, &mut deleted),
        map_line("fixes", &s.fixes, &mut deleted),
        map_line("vocab", &s.vocab, &mut deleted),
        format!(
            "  places {}{}",
            places.len(),
            if places.is_empty() {
                String::new()
            } else {
                format!(": {}", places.join(", "))
            }
        ),
    ];
    if deleted > 0 {
        out.push(format!(
            "  and {deleted} deleted records, as markers with no content"
        ));
    }
    out.push(
        "Never shared: audio (chunks/, recent/, fixes/), the voiceprint registry (voices/), where this Mac \
is now (here.json), transcript.txt, context/ copies, labels and stats."
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
