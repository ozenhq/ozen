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
        acc += &format!(" (was {})", pct(first));
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
        let mut m = read(file);
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
        .collect();
    let junk: BTreeSet<String> = fs::read(JUNK)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let threshold = read(STATS).get("threshold").and_then(Value::as_f64);
    // Review leaves out lines the panel just tagged (unsure() only knows the written tags)
    let unsure: Vec<Value> = unsure(&rows(), &read(TAGS), &read(LABELS), threshold)
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
    fn transcription_off_hides_pause() {
        let on = controls("stopped", false, 2);
        assert_eq!(
            (
                on["start"]["title"].as_str(),
                on["pause"]["hidden"].as_bool()
            ),
            (Some("Start"), Some(false))
        );
        insta::assert_json_snapshot!(controls("stopped", true, 2));
        assert_eq!(controls("processing", true, 2)["start"]["enabled"], true);
        assert_eq!(controls("paused", true, 0)["start"]["title"], "Resume");
    }

    #[test]
    fn timeline_shows_the_latest_meeting_with_fixed_text() {
        let raw = [
            row("old", 1.0, "S1", json!({"d": 2.0})),
            row("a", 1000.0, "S1", json!({"d": 2.0})), // over GAP later: a new meeting
            row("b", 1300.0, "S2", json!({"d": 2.0, "text": "heard"})), // within GAP: same meeting
        ];
        let fixes = obj(json!({"b": "fixed"}));
        let v = transcript(
            &raw,
            &BTreeSet::new(),
            &Row::new(),
            &Row::new(),
            &fixes,
            &[],
            &Row::new(),
        );
        let segs: Vec<(&str, &str)> = v["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| (s["id"].as_str().unwrap(), s["text"].as_str().unwrap()))
            .collect();
        assert_eq!(segs[0].0, "a");
        assert_eq!(segs[1], ("b", "fixed"));
        assert_eq!(segs.len(), 2);
        assert_eq!(v["lines"].as_array().unwrap().len(), 3); // the transcript still shows every line
    }

    #[test]
    fn transcript_corrects_speakers_and_text_and_marks_unsure() {
        let raw = [
            row("b", 2.0, "S1", json!({"src": "room", "d": 3.0})),
            row("a", 1.0, "S1", json!({"src": "call", "text": "שלום"})), // file order isn't time order
            row("j", 3.0, "S1", json!({})),                              // junk: hidden everywhere
            row("i", 4.0, "S2", json!({"d": 1.0})),
            row("u", 5.0, "S3", json!({"d": 2.0})),
        ];
        let junk: BTreeSet<String> = ["j".to_string()].into();
        let tags = obj(json!({"b": "Dana", "i": "Ignored 2", "a": ""}));
        let labels = obj(json!({"u": {"spk": "Omer"}}));
        let fixes = obj(json!({"b": "fixed", "a": ""}));
        let unsure = [
            json!({"id": "u", "until": 605.0}),
            json!({"id": "b", "until": 602.0}),
        ];
        let stats = obj(
            json!({"accuracy": 0.875, "accuracy_first": 1.0, "evaluated": 8, "tagged": 3, "ignored": 2}),
        );
        let v = transcript(&raw, &junk, &tags, &labels, &fixes, &unsure, &stats);
        let lines = v["lines"].as_array().unwrap();
        let ids: Vec<&str> = lines.iter().map(|l| l["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["a", "b", "i", "u"]);
        assert_eq!(
            (
                lines[0]["speaker"].clone(),
                lines[0]["rtl"].clone(),
                lines[0]["text"].clone()
            ),
            (json!("S1"), json!(true), json!("שלום"))
        );
        assert_eq!(
            (
                lines[1]["speaker"].clone(),
                lines[1]["mark"].clone(),
                lines[1]["text"].clone(),
                lines[1]["heard"].clone()
            ),
            (json!("Dana"), json!("✓"), json!("fixed"), json!("hi"))
        );
        assert_eq!(
            (
                lines[2]["ignored"].clone(),
                lines[3]["speaker"].clone(),
                lines[3]["mark"].clone()
            ),
            (json!(true), json!("Omer"), json!("?"))
        );
        let segs: Vec<&str> = v["segments"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(segs, ["a", "b", "u"]); // the ignored voice leaves the timeline
        assert_eq!(v["segments"][0]["d"], json!(1.0)); // no duration: at least a second
        assert_eq!(
            v["review"],
            json!([{"id": "u", "until": 605.0}, {"id": "b", "until": 602.0}])
        );
        assert_eq!(
            v["footer"],
            "  accuracy 88% on 8 checks (was 100%) · 3 tagged · 2 ignored · orange ? = unsure, tag it to teach ozen · click text to fix it"
        );
    }
}
