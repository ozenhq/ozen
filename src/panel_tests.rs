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
    assert!(v["lines"][0]["day"].as_str().is_some_and(|d| d.len() == 10)); // "Thu 01 Jan"
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
        "  accuracy 88% on 8 checks (started at 100%) · 3 tagged · 2 ignored · orange ? = unsure, tag it to teach ozen · click text to fix it"
    );
}
