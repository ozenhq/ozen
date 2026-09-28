//! What the menu bar panel shows, decided here so it's tested with the rest of ozen: the tag menu for a line
//! (`ozen tag-menu`), the lines Review asks about (`ozen unsure`) and the control buttons (`ozen controls`).
//! menubar.swift only draws these.
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
            r.get("id")
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

/// The panel's buttons for recorder `state`, with split on (record now, transcribe later) or off, `queued`
/// chunks not yet transcribed and whether `ozen process` is running.
fn controls(state: &str, split: bool, queued: usize, processing: bool) -> Value {
    json!({
        "start": {
            "title": if state == "paused" { "Resume" } else if split { "Record" } else { "Start" },
            "enabled": matches!(state, "stopped" | "paused" | "processing"),
        },
        // a recording without a transcriber has nothing to keep loaded: Stop is the same
        "pause": {"enabled": state == "recording", "hidden": split},
        "stop": {"enabled": matches!(state, "recording" | "paused")},
        "process": {
            "title": if processing { "Stop processing".into() } else if queued > 0 { format!("Process {queued}") } else { "Process".into() },
            "enabled": processing || queued > 0,
            "hidden": !split,
        },
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
        &read(TAGS),
        &read(LABELS),
        &registry()
    ))
}

pub fn unsure_json() -> Value {
    let threshold = read(STATS).get("threshold").and_then(Value::as_f64);
    json!(unsure(&rows(), &read(TAGS), &read(LABELS), threshold))
}

pub fn controls_json(state: &str, split: bool) -> Value {
    let queued = fs::read_dir("chunks").map_or(0, |d| {
        d.flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".wav"))
            .count()
    });
    controls(
        state,
        split,
        queued,
        fs::exists(".processing").unwrap_or(false),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(v: Value) -> Row {
        v.as_object().unwrap().clone()
    }

    fn row(id: &str, t: f64, spk: &str, extra: Value) -> Row {
        let mut r = obj(json!({"id": id, "t": t, "spk": spk, "text": "hi"}));
        r.extend(obj(extra));
        r
    }

    fn titles(menu: &[Value]) -> Vec<&str> {
        menu.iter().map(|m| m["title"].as_str().unwrap()).collect()
    }

    #[test]
    fn tag_menu_offers_people_new_and_each_ignored_voice() {
        let rows = [
            row("a", 1.0, "S2", json!({"run": 7})),
            row("b", 2.0, "S2", json!({"run": 7})),
            row("c", 3.0, "S2", json!({"run": 6})), // S2 of another run: a different voice
            row("d", 4.0, "S1", json!({"run": 7})),
            row("e", 5.0, "S2", json!({"run": 7})),
        ];
        let tags = obj(json!({"d": "Dana", "x": "Ignored", "y": "Ignored 2"}));
        let menu = tag_menu(
            "a",
            &rows,
            &tags,
            &Row::new(),
            &["Avi".into(), "Dana".into()],
        );
        insta::assert_json_snapshot!(menu);
        let named = tag_menu("d", &rows, &tags, &Row::new(), &[]);
        assert!(
            !titles(&named).iter().any(|t| t.starts_with("Ignore all")),
            "a named person is never one click from ignored"
        );
    }

    #[test]
    fn tag_menu_groups_runless_lines_within_an_hour() {
        let rows = [
            row("a", 0.0, "S1", json!({})),
            row("b", 3599.0, "S1", json!({})),
            row("c", 3601.0, "S1", json!({})),
        ];
        let menu = tag_menu("a", &rows, &Row::new(), &Row::new(), &[]);
        assert!(
            titles(&menu).contains(&"Ignore all 2 lines by S1"),
            "{:?}",
            titles(&menu)
        );
    }

    #[test]
    fn unsure_lines_queue_most_uncertain_first() {
        let rows = [
            row("new", 100.0, "S1", json!({"doubt": 0.02})), // unsure, not retrained yet
            row("sure", 110.0, "S1", json!({"doubt": 0.3})), // the transcriber is sure
            row("labeled", 120.0, "S1", json!({})), // retrain's verdict: unsure (0.4 - 0.37 = 0.03)
            row("relabeled", 125.0, "S1", json!({"doubt": 0.01})), // a retrain since found it sure: its verdict wins
            row("tagged", 130.0, "S1", json!({"doubt": 0.001})),   // you said who it was
            row("edge", 90.0, "S1", json!({"doubt": 0.05})),
        ];
        let labels = obj(json!({
            "labeled": {"spk": "Dana", "sim": 0.37, "margin": 0.2, "unsure": true},
            "relabeled": {"spk": "Dana", "sim": 0.9, "margin": 0.5, "unsure": false},
        }));
        let tags = obj(json!({"tagged": "Dana"}));
        let got = unsure(&rows, &tags, &labels, Some(0.4));
        assert_eq!(
            got,
            [
                json!({"id": "new", "by": 0.02, "until": 700.0}),
                json!({"id": "labeled", "by": 0.03, "until": 720.0}),
                json!({"id": "edge", "by": 0.05, "until": 690.0}),
            ]
        );
    }

    #[test]
    fn split_swaps_start_and_pause_for_record_and_process() {
        let off = controls("stopped", false, 2, false);
        assert_eq!(
            (
                off["start"]["title"].as_str(),
                off["pause"]["hidden"].as_bool()
            ),
            (Some("Start"), Some(false))
        );
        assert_eq!(off["process"]["hidden"], true);
        insta::assert_json_snapshot!(controls("stopped", true, 2, false));
        let busy = controls("processing", true, 2, true);
        assert_eq!(
            (
                busy["process"]["title"].as_str(),
                busy["start"]["enabled"].as_bool()
            ),
            (Some("Stop processing"), Some(true))
        );
        assert_eq!(
            controls("paused", true, 0, false)["start"]["title"],
            "Resume"
        );
        assert_eq!(
            controls("stopped", true, 0, false)["process"]["enabled"],
            false
        );
    }
}
