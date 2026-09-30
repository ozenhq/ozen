//! The ozen CLI end to end on a sample folder (OZEN_DIR), as the menu bar panel calls it. Nothing records.
use assert_cmd::Command;
use serde_json::{Value, json};
use std::fs;
use tempfile::TempDir;

fn folder(files: &[(&str, String)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, body) in files {
        let path = dir.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }
    dir
}

fn ozen(dir: &TempDir, args: &[&str]) -> Value {
    let out = Command::cargo_bin("ozen")
        .unwrap()
        .env("OZEN_DIR", dir.path())
        .args(args)
        .assert()
        .success();
    serde_json::from_slice(&out.get_output().stdout).unwrap()
}

fn lines(rows: &[Value]) -> String {
    rows.iter().map(|r| r.to_string() + "\n").collect()
}

fn line(id: &str, t: f64, spk: &str, extra: Value) -> Value {
    let mut r = json!({"id": id, "t": t, "spk": spk, "src": "room", "text": format!("said {id}")});
    r.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    r
}

#[test]
fn ignored_voices_stay_apart() {
    let dir = folder(&[
        (
            "lines.jsonl",
            lines(&[
                line("dana", 1.0, "S1", json!({"run": 1, "e": [1.0, 0.0]})),
                line("tv", 2.0, "S2", json!({"run": 1, "e": [0.0, 1.0]})),
                line("radio", 3.0, "S3", json!({"run": 1, "e": [0.5, 0.5]})),
                line("new", 4.0, "S4", json!({"run": 1, "e": [0.2, 0.9]})),
            ]),
        ),
        (
            "tags.json",
            json!({"dana": "Dana", "tv": "Ignored", "radio": "Ignored 2"}).to_string(),
        ),
        ("voices/voices/avi.json", json!({"name": "Avi"}).to_string()),
    ]);
    insta::assert_json_snapshot!("voices", ozen(&dir, &["voices"]));
    insta::assert_json_snapshot!("tag_menu", ozen(&dir, &["tag-menu", "new"]));
}

#[test]
fn review_asks_about_unsure_lines_but_not_junk() {
    let dir = folder(&[
        (
            "lines.jsonl",
            lines(&[
                line("new", 100.0, "S1", json!({"doubt": 0.02})),
                line("sure", 110.0, "S1", json!({"doubt": 0.3})),
                line("labeled", 120.0, "S1", json!({})),
                line("junk", 130.0, "S1", json!({"doubt": 0.001})), // an old Whisper echo: never shown
            ]),
        ),
        (
            "labels.json",
            json!({"labeled": {"spk": "Dana", "sim": 0.37, "margin": 0.2, "unsure": true}})
                .to_string(),
        ),
        ("stats.json", json!({"threshold": 0.4}).to_string()),
        ("junk.json", json!(["junk"]).to_string()),
    ]);
    assert_eq!(
        ozen(&dir, &["unsure"]),
        json!([{"id": "new", "by": 0.02, "until": 700.0}, {"id": "labeled", "by": 0.03, "until": 720.0}])
    );
}

#[test]
fn priority_is_remembered() {
    let dir = folder(&[]);
    let priority = |args: &[&str]| {
        let out = Command::cargo_bin("ozen")
            .unwrap()
            .env("OZEN_DIR", dir.path())
            .arg("priority")
            .args(args)
            .assert()
            .success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    };
    assert_eq!(priority(&[]), "normal\n");
    priority(&["low"]);
    assert_eq!(priority(&[]), "low\n");
    priority(&["normal"]);
    assert_eq!(priority(&[]), "normal\n");
}

#[test]
fn split_counts_queued_chunks() {
    let dir = folder(&[
        ("chunks/1-call.wav", String::new()),
        ("chunks/1-mic.wav", String::new()),
    ]);
    let c = ozen(&dir, &["controls", "stopped", "split"]);
    assert_eq!(
        (c["queued"].as_i64(), c["pause"]["hidden"].as_bool()),
        (Some(2), Some(true))
    );
    assert_eq!(
        ozen(&dir, &["controls", "stopped"])["pause"]["hidden"],
        false
    );
}

#[test]
fn timebar_reports_done_skipped_and_waiting_chunks() {
    let pace = [
        json!({"ms": 1000, "tag": "mic", "sec": 15.0, "done": 1060.0, "took": 3.0, "lines": 1}),
        json!({"ms": 2000, "tag": "call", "sec": 0.0, "done": 2080.0, "took": 0.1, "lines": 0, "error": "bad wav"}),
    ];
    let dir = folder(&[
        ("pace.jsonl", lines(&pace)),
        ("chunks/3000-local.wav", String::new()),
    ]);
    let mut out = ozen(&dir, &["timebar"]);
    assert!(out["now"].as_f64().unwrap() > 0.0);
    out.as_object_mut().unwrap().remove("now");
    insta::assert_json_snapshot!("timebar", out);
}
