use super::*;
use crate::ignore::IGNORE;

fn row(id: &str, t: f64, spk: &str, run: Option<i64>) -> Row {
    let mut r = json!({"id": id, "t": t, "spk": spk, "text": format!("said {id}")});
    if let Some(run) = run {
        r["run"] = json!(run);
    }
    r.as_object().unwrap().clone()
}

#[test]
fn groups_people_this_runs_speakers_and_ignored() {
    let rows = [
        row("a", 1.0, "S1", Some(1)), // tagged Dana
        row("b", 2.0, "S1", Some(1)), // matched to Dana
        row("c", 3.0, "S2", Some(1)), // S2 in an earlier run: a different voice, not listed
        row("d", 4.0, "S2", Some(2)), // this run's S2
        row("e", 5.0, "S2", Some(2)),
        row("f", 6.0, "S3", Some(2)), // ignored
        row("i", 6.5, "S4", Some(2)), // a second ignored voice, listed apart
        row("g", 7.0, "?", Some(2)),  // too short to say
        row("h", 8.0, "S1", Some(2)), // tagged Dana
    ];
    let tags = json!({"a": "Dana", "f": IGNORE, "h": "Dana", "i": "Ignored 2"})
        .as_object()
        .unwrap()
        .clone();
    let labels = json!({"b": {"spk": "Dana"}}).as_object().unwrap().clone();
    let fixes = json!({"h": "fixed text"}).as_object().unwrap().clone();
    let v = summarize(&rows, &tags, &labels, &fixes);
    let names: Vec<(&str, &str, u64)> = v
        .iter()
        .map(|x| {
            (
                x["name"].as_str().unwrap(),
                x["kind"].as_str().unwrap(),
                x["lines"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        names,
        [
            ("Dana", "person", 3),
            ("S2", "unnamed", 2),
            (IGNORE, "ignored", 1),
            ("Ignored 2", "ignored", 1)
        ]
    );
    assert_eq!(v[0]["recent"][0]["text"], "fixed text"); // newest first, with your fix
    assert_eq!(v[1]["ids"], json!(["e", "d"]));
    assert!(v[0].get("ids").is_none());
}

#[test]
fn anonymous_labels_are_exact() {
    assert!(anon("S1") && anon("S12"));
    assert!(!anon("S") && !anon("Sarah") && !anon("S1a"));
}
