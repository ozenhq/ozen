//! What the menu bar panel shows, decided here so it's tested with the rest of ozen: the tag menu for a line
//! (`ozen tag-menu`), the lines Review asks about (`ozen unsure`), the control buttons (`ozen controls`) and the
//! transcript itself with its timeline and footer (`ozen transcript`).
//! The menu bar app (src/bin/bar) only draws these.
use crate::fixes::read;
use crate::ignore::is_ignored;
use crate::voices::{anon, speaker};
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::fs;

type Row = Map<String, Value>;

const LINES: &str = "lines.jsonl";
const TAGS: &str = "tags.json";
const LABELS: &str = "labels.json";
const STATS: &str = "stats.json";
const JUNK: &str = "junk.json";
const REGISTRY: &str = "voices/voices";
const UNSURE: f64 = 0.08; // src/train.rs UNSURE: this close to the threshold, or to a second person
const REVIEW_AGE: f64 = 600.0; // after 10 minutes nobody remembers who said what
const SHOWN: usize = 400; // transcript lines in the panel
const HISTORY: usize = 5000; // lines.jsonl rows the timeline spans
const SAME_HOUR: f64 = 3600.0; // older lines carry no run: an S-label's lines within an hour are one voice

/// Every transcript line the panel shows: all of lines.jsonl (notes too) but the old echoes in junk.json.
fn rows() -> Vec<Row> {
    let junk: BTreeSet<String> = fs::read(JUNK)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    fs::read_to_string(LINES)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Row>(l).ok())
        .filter(|r| {
            crate::crdt::is_live(r)
                && r.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !junk.contains(id))
        })
        .collect()
}

fn str_of<'a>(r: &'a Row, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or("")
}

fn item(title: &str, action: &str, arg: Value) -> Value {
    json!({"title": title, "action": action, "arg": arg})
}

/// The menu for tagging line `id`: people you can pick, a new person, ignoring the voice (as a new ignored voice or
/// one already ignored), every line of this run's unnamed speaker at once, and clearing the tag.
/// Actions: "tag" (arg: name), "new", "ignore" (arg: line ids), "separator".
fn tag_menu(id: &str, rows: &[Row], tags: &Row, labels: &Row, registry: &[String]) -> Vec<Value> {
    let tagged = tags.values().filter_map(Value::as_str);
    let people: BTreeSet<&str> = registry
        .iter()
        .map(String::as_str)
        .chain(tagged.clone())
        .filter(|n| !n.is_empty() && !is_ignored(n))
        .collect();
    let mut menu: Vec<Value> = people.iter().map(|n| item(n, "tag", json!(n))).collect();
    if !menu.is_empty() {
        menu.push(item("", "separator", Value::Null));
    }
    menu.push(item("New person…", "new", Value::Null));
    menu.push(item("Ignore this voice", "ignore", json!([id])));
    let ignored: BTreeSet<&str> = tagged.filter(|n| is_ignored(n)).collect();
    for n in ignored {
        menu.push(item(&format!("Same voice as {n}"), "tag", json!(n)));
    }
    // Only session labels: a named person (you) is never one click from being ignored.
    if let Some(line) = rows.iter().find(|r| str_of(r, "id") == id)
        && let spk = speaker(line, tags, labels)
        && anon(spk)
    {
        let run = line.get("run").and_then(Value::as_i64);
        let t = line.get("t").and_then(Value::as_f64).unwrap_or(0.0);
        // S1, S2... restart with the transcriber, so only that run's lines are the same voice.
        let same: Vec<&str> = rows
            .iter()
            .filter(|r| speaker(r, tags, labels) == spk)
            .filter(|r| match run {
                Some(_) => r.get("run").and_then(Value::as_i64) == run,
                None => (r.get("t").and_then(Value::as_f64).unwrap_or(0.0) - t).abs() < SAME_HOUR,
            })
            .map(|r| str_of(r, "id"))
            .collect();
        let s = if same.len() == 1 { "" } else { "s" };
        menu.push(item(
            &format!("Ignore all {} line{s} by {spk}", same.len()),
            "ignore",
            json!(same),
        ));
    }
    menu.push(item("", "separator", Value::Null));
    menu.push(item("Clear tag", "tag", json!("")));
    menu
}

/// Untagged lines ozen isn't sure who said, most uncertain first: the last retrain's verdict once there is one,
/// before that the transcriber's own doubt (same formula), so new lines reach Review without waiting for a tag.
/// Each says when it leaves Review (`until`).
fn unsure(rows: &[Row], tags: &Row, labels: &Row, threshold: Option<f64>) -> Vec<Value> {
    let mut out: Vec<(f64, Value)> = rows
        .iter()
        .filter_map(|r| {
            let id = str_of(r, "id");
            if tags
                .get(id)
                .and_then(Value::as_str)
                .is_some_and(|n| !n.is_empty())
            {
                return None;
            }
            let by = match labels.get(id) {
                None => r
                    .get("doubt")
                    .and_then(Value::as_f64)
                    .filter(|d| *d < UNSURE)?,
                Some(g) if g["unsure"] != true => return None,
                Some(g) => match (g["sim"].as_f64(), threshold) {
                    (Some(sim), Some(thr)) => {
                        (sim - thr).abs().min(g["margin"].as_f64().unwrap_or(0.0))
                    }
                    _ => 0.0,
                },
            };
            let t = r.get("t").and_then(Value::as_f64)?;
            let by = (by * 1000.0).round() / 1000.0;
            Some((by, json!({"id": id, "by": by, "until": t + REVIEW_AGE})))
        })
        .collect();
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out.into_iter().map(|(_, v)| v).collect()
}

/// The panel's buttons for recorder `state`, with transcription off (split: record now, transcribe later) or on,
/// and `queued` chunks not yet transcribed.
fn controls(state: &str, split: bool, queued: usize) -> Value {
    json!({
        "start": {
            "title": if state == "paused" { "Resume" } else { "Start" },
            "enabled": matches!(state, "stopped" | "paused" | "processing"),
        },
        // a recording without a transcriber has nothing to keep loaded: Stop is the same
        "pause": {"enabled": state == "recording", "hidden": split},
        "stop": {"enabled": matches!(state, "recording" | "paused")},
        "queued": queued,
    })
}

fn registry() -> Vec<String> {
    fs::read_dir(REGISTRY)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|f| {
            read(f.path().to_str()?)
                .get("name")?
                .as_str()
                .map(String::from)
        })
        .collect()
}

pub fn tag_menu_json(id: &str) -> Value {
    json!(tag_menu(
        id,
        &rows(),
        &crate::crdt::read_map(TAGS),
        &read(LABELS),
        &registry()
    ))
}

pub fn unsure_json() -> Value {
    let threshold = read(STATS).get("threshold").and_then(Value::as_f64);
    json!(unsure(
        &rows(),
        &crate::crdt::read_map(TAGS),
        &read(LABELS),
        threshold
    ))
}

pub fn controls_json(state: &str, split: bool) -> Value {
    let queued = fs::read_dir("chunks").map_or(0, |d| {
        d.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".wav"))
            .count()
    });
    controls(state, split, queued)
}

/// The transcript as the panel shows it, from the last HISTORY rows of lines.jsonl (`raw`, file order):
/// speakers corrected by tags (and a guess from the last retrain), text by fixes, the unsure ones marked.
/// `pending` holds what the panel just set and `ozen tag` / `ozen fix` haven't written yet:
/// {"tags": {id: name}, "fixes": {id: text}}. Returns {"lines" (the last SHOWN), "segments" (timeline bars of the latest
/// meeting, ignored voices left out), "review" (unsure lines that are shown: id, until), "footer"}.
fn transcript<'a>(
    raw: &'a [Row],
    junk: &BTreeSet<String>,
    tags: &'a Row,
    labels: &'a Row,
    fixes: &'a Row,
    unsure: &[Value],
    stats: &Row,
) -> Value {
    let mut lines: Vec<&Row> = raw
        .iter()
        .filter(|r| {
            r.get("t").and_then(Value::as_f64).is_some()
                && r.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !junk.contains(id))
        })
        .collect();
    lines.sort_by(|a, b| {
        a["t"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&b["t"].as_f64().unwrap_or(0.0))
    }); // stable, like Swift's
    let unsure_ids: BTreeSet<&str> = unsure.iter().filter_map(|u| u["id"].as_str()).collect();
    let view = |r: &'a Row| speaker_of(r, tags, labels, &unsure_ids);
    // The timeline shows one meeting, the latest: the lines after the last silence that ends a meeting.
    let start = lines
        .windows(2)
        .rposition(|w| {
            w[1]["t"].as_f64().unwrap_or(0.0) - w[0]["t"].as_f64().unwrap_or(0.0)
                > crate::meetings::GAP
        })
        .map_or(0, |i| i + 1);
    let segments: Vec<Value> = lines[start..]
        .iter()
        .filter_map(|r| {
            let (id, _, speaker, unsure) = view(r);
            let heard = str_of(r, "text");
            let text = fixes.get(id).and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or(heard);
            // older lines carry no duration: estimate it from the text's length
            let d = r.get("d").and_then(Value::as_f64).unwrap_or_else(|| (heard.chars().count() as f64 / 14.0).clamp(1.0, 15.0));
            (!is_ignored(speaker)).then(|| json!({"id": id, "t": r["t"], "d": d, "speaker": speaker, "text": text, "unsure": unsure}))
        })
        .collect();
    let shown: Vec<Value> = lines[lines.len().saturating_sub(SHOWN)..]
        .iter()
        .map(|r| {
            let (id, tagged, speaker, unsure) = view(r);
            let heard = str_of(r, "text");
            let said = fixes.get(id).and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or(heard);
            let time = local(r["t"].as_f64().unwrap_or(0.0)).format("%H:%M:%S").to_string();
            json!({"id": id, "time": time, "speaker": speaker, "mark": if tagged { "✓" } else if unsure { "?" } else { "" },
                   "src": str_of(r, "src"), "text": said, "heard": heard, "ignored": is_ignored(speaker), "unsure": unsure,
                   "rtl": said.chars().any(|c| ('\u{0590}'..='\u{05FF}').contains(&c))})
        })
        .collect();
    let shown_ids: BTreeSet<&str> = shown.iter().filter_map(|l| l["id"].as_str()).collect();
    let review: Vec<Value> = unsure
        .iter()
        .filter(|u| u["id"].as_str().is_some_and(|id| shown_ids.contains(id)))
        .map(|u| json!({"id": u["id"], "until": u["until"]}))
        .collect();
    let pct = |x: f64| format!("{}%", (x * 100.0).round() as i64);
    let mut acc = match stats.get("accuracy").and_then(Value::as_f64) {
        Some(a) => format!(
            "accuracy {} on {} checks",
            pct(a),
            stats.get("evaluated").cloned().unwrap_or(json!(0))
        ),
        None => "accuracy after 2 tags of one person".into(),
    };
    if let (Some(first), Some(now)) = (
        stats.get("accuracy_first").and_then(Value::as_f64),
        stats.get("accuracy").and_then(Value::as_f64),
    ) && first != now
    {
        acc += &format!(" (started at {})", pct(first)); // the first retrain's, not the last one's
    }
    let tagged = stats.get("tagged").and_then(Value::as_i64).unwrap_or(0);
    let ignored = match stats.get("ignored").and_then(Value::as_i64) {
        Some(n) if n > 0 => format!(" · {n} ignored"),
        _ => String::new(),
    };
    json!({"lines": shown, "segments": segments, "review": review,
           "footer": format!("  {acc} · {tagged} tagged{ignored} · orange ? = unsure, tag it to teach ozen · click text to fix it")})
}

/// (id, tagged, speaker, unsure): the speaker by tag, else the last retrain's guess, else the transcriber's.
fn speaker_of<'a>(
    r: &'a Row,
    tags: &'a Row,
    labels: &'a Row,
    unsure: &BTreeSet<&str>,
) -> (&'a str, bool, &'a str, bool) {
    let id = str_of(r, "id");
    let tag = tags
        .get(id)
        .and_then(Value::as_str)
        .filter(|n| !n.is_empty());
    let speaker = tag
        .or_else(|| labels.get(id).and_then(|g| g["spk"].as_str()))
        .unwrap_or_else(|| r.get("spk").and_then(Value::as_str).unwrap_or("?"));
    (
        id,
        tag.is_some(),
        speaker,
        tag.is_none() && unsure.contains(id),
    )
}

fn local(t: f64) -> chrono::DateTime<chrono::Local> {
    chrono::DateTime::from_timestamp(t as i64, 0)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
}

/// `ozen transcript [PENDING]`: see `transcript`.
pub fn transcript_json(pending: &str) -> Value {
    let pending: Row = serde_json::from_str(pending).unwrap_or_default();
    let merged = |file: &str, key: &str| {
        let mut m = crate::crdt::read_map(file);
        if let Some(p) = pending.get(key).and_then(Value::as_object) {
            m.extend(p.clone());
        }
        m
    };
    let (tags, fixes) = (merged(TAGS, "tags"), merged("fixes.json", "fixes"));
    let raw_text = fs::read_to_string(LINES).unwrap_or_default();
    let all: Vec<&str> = raw_text.split('\n').filter(|l| !l.is_empty()).collect();
    let raw: Vec<Row> = all[all.len().saturating_sub(HISTORY)..]
        .iter()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(crate::crdt::is_live)
        .collect();
    let junk: BTreeSet<String> = fs::read(JUNK)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let threshold = read(STATS).get("threshold").and_then(Value::as_f64);
    // Review leaves out lines the panel just tagged (unsure() only knows the written tags)
    let unsure: Vec<Value> = unsure(
        &rows(),
        &crate::crdt::read_map(TAGS),
        &read(LABELS),
        threshold,
    )
    .into_iter()
    .filter(|u| {
        u["id"].as_str().is_some_and(|id| {
            !pending
                .get("tags")
                .and_then(|t| t.get(id))
                .and_then(Value::as_str)
                .is_some_and(|n| !n.is_empty())
        })
    })
    .collect();
    transcript(
        &raw,
        &junk,
        &tags,
        &read(LABELS),
        &fixes,
        &unsure,
        &read(STATS),
    )
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
