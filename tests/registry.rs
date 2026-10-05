//! With sync set up, voiceprints never go to or come from the GitHub voices registry (OFE-44): retraining
//! runs no git at all, and Macs sharing lines and tags build byte-identical voiceprints from them alone.
use assert_cmd::Command;
use serde_json::json;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use tempfile::TempDir;

/// A folder with `n` lines from three people (192-float prints) and every 10th line tagged.
fn folder(n: usize) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut lines = String::new();
    let mut tags = serde_json::Map::new();
    for i in 0..n {
        let who = i % 3;
        let e: Vec<f64> = (0..192)
            .map(|k| ((k * (who + 1)) as f64 * 0.11).sin() + (i as f64 * 0.001))
            .collect();
        lines += &(json!({"id": format!("{i}@m"), "t": i, "d": 2.0, "text": "said", "e": e})
            .to_string()
            + "\n");
        if i % 10 == 0 {
            tags.insert(
                format!("{i}@m"),
                json!({"v": 1, "val": format!("Person {who}")}),
            );
        }
    }
    fs::write(dir.path().join("lines.jsonl"), lines).unwrap();
    fs::write(
        dir.path().join("tags.json"),
        serde_json::Value::Object(tags).to_string(),
    )
    .unwrap();
    dir
}

/// A `git` that only logs its arguments, first on PATH.
fn stub_git(log: &Path) -> TempDir {
    let bin = tempfile::tempdir().unwrap();
    let git = bin.path().join("git");
    fs::write(
        &git,
        format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 1\n", log.display()),
    )
    .unwrap();
    fs::set_permissions(&git, fs::Permissions::from_mode(0o755)).unwrap();
    bin
}

fn retrain(dir: &Path, path: Option<&Path>) {
    let mut c = Command::cargo_bin("ozen").unwrap();
    c.env("OZEN_DIR", dir).arg("retrain");
    if let Some(bin) = path {
        c.env(
            "PATH",
            format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
        );
    }
    c.assert().success();
}

#[test]
fn with_sync_a_retrain_runs_no_git_without_it_the_registry_is_used() {
    let d = folder(60);
    let log = d.path().join("git.log");
    let bin = stub_git(&log);
    retrain(d.path(), Some(bin.path()));
    let calls = fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.contains("fetch") && calls.contains("push"),
        "no sync: registry as before\n{calls}"
    );

    let d = folder(60);
    fs::write(d.path().join(".sync-on"), "").unwrap();
    let log = d.path().join("git.log");
    let bin = stub_git(&log);
    retrain(d.path(), Some(bin.path()));
    assert_eq!(
        fs::read_to_string(&log).unwrap_or_default(),
        "",
        "sync: no git at all"
    );
    assert_eq!(
        fs::read_dir(d.path().join("voices/voices"))
            .unwrap()
            .count(),
        3,
        "three voiceprints built here"
    );
}

/// voices/voices and voices/samples, file by file.
fn prints(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![];
    for sub in ["voices/voices", "voices/samples"] {
        for f in fs::read_dir(dir.join(sub)).unwrap() {
            let p = f.unwrap().path();
            out.push((
                p.strip_prefix(dir).unwrap().display().to_string(),
                fs::read(&p).unwrap(),
            ));
        }
    }
    out.sort();
    out
}

#[test]
fn synced_macs_with_different_registry_histories_build_identical_voiceprints() {
    let (a, b) = (folder(90), folder(90));
    for d in [&a, &b] {
        fs::write(d.path().join(".sync-on"), "").unwrap();
    }
    // Mac b once pulled the GitHub registry: an old voice for someone no synced tag names, and an old
    // sample of Person 0 from a line it no longer has
    let old = b.path().join("voices/voices");
    fs::create_dir_all(&old).unwrap();
    fs::create_dir_all(b.path().join("voices/samples")).unwrap();
    let e: Vec<f64> = (0..192).map(|k| (k as f64).cos()).collect();
    let rec = |n: &str| json!({"name": n, "model": "speechbrain/spkrec-ecapa-voxceleb", "count": 1, "embedding": e});
    fs::write(
        old.join("someone-else.json"),
        rec("Someone Else").to_string(),
    )
    .unwrap();
    fs::write(old.join("person-0.json"), rec("Person 0").to_string()).unwrap();
    fs::write(
        b.path().join("voices/samples/person-0.json"),
        json!({"999@old": {"w": 1, "e": e}}).to_string(),
    )
    .unwrap();
    retrain(a.path(), None);
    retrain(b.path(), None);
    let (pa, pb) = (prints(a.path()), prints(b.path()));
    assert_eq!(pa.len(), 6, "three people, a print and samples each");
    assert_eq!(pa, pb);
    for f in ["labels.json", "stats.json"] {
        assert_eq!(
            fs::read(a.path().join(f)).unwrap(),
            fs::read(b.path().join(f)).unwrap(),
            "{f}"
        );
    }
}
