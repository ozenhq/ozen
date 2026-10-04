use super::*;
use std::path::Path;
use tempfile::TempDir;

fn folder(files: &[(&str, Value)]) -> TempDir {
    let d = tempfile::tempdir().unwrap();
    for (name, v) in files {
        let body = match v {
            Value::String(s) => s.clone(),
            Value::Array(rows) if *name == LINES => {
                rows.iter().map(|r| r.to_string() + "\n").collect()
            }
            v => v.to_string(),
        };
        fs::write(d.path().join(name), body).unwrap();
    }
    d
}

/// Every file in `d`, name -> contents.
fn snapshot(d: &Path) -> Vec<(String, String)> {
    let mut v: Vec<_> = fs::read_dir(d)
        .unwrap()
        .map(|e| e.unwrap().path())
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into(),
                fs::read_to_string(&p).unwrap(),
            )
        })
        .collect();
    v.sort();
    v
}

/// `merge_files(from)` run inside `into`, as `ozen` runs it from the ozen folder.
fn merge(into: &Path, from: &Path) -> Result<String, String> {
    let _cwd = crate::CWD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(into).unwrap();
    let r = merge_files(&from.to_string_lossy());
    std::env::set_current_dir(back).unwrap();
    r
}

fn a() -> TempDir {
    folder(&[
        (
            LINES,
            json!([{"id": "1", "t": 1.0, "text": "hi"}, {"id": "2", "t": 2.0, "text": "yo"}]),
        ),
        (TAGS, json!({"1": "Dana", "2": {"v": 5, "val": "Noa"}})),
        (FIXES, json!({"1": "hi there"})),
        (VOCAB_TXT, json!("Kev, PR\n")),
        (places::FILE, json!([{"label": "Home", "action": "off"}])),
    ])
}

fn b() -> TempDir {
    folder(&[
        (
            LINES,
            json!([{"id": "2", "v": 6, "del": true}, {"id": "3@b", "t": 3.0, "text": "new"}]),
        ),
        (TAGS, json!({"2": {"v": 6}, "3@b": {"v": 7, "val": "Omer"}})),
        (VOCAB, json!({"Claude": {"v": 5, "val": true}})),
        (
            places::FILE,
            json!([{"id": "9@b", "v": 5, "label": "Work", "action": "record"}]),
        ),
    ])
}

#[test]
fn either_order_gives_the_same_files_and_repeating_changes_nothing() {
    let (x, y) = (a(), b());
    let (x2, y2) = (a(), b());
    merge(x.path(), y.path()).unwrap(); // a <- b
    merge(y2.path(), x2.path()).unwrap(); // b <- a
    let x_files = snapshot(x.path());
    assert_eq!(x_files, snapshot(y2.path()));
    merge(x.path(), y.path()).unwrap();
    merge(x.path(), y2.path()).unwrap();
    assert_eq!(snapshot(x.path()), x_files);
}

#[test]
fn a_tombstone_beats_an_older_edit() {
    let (x, y) = (a(), b());
    merge(x.path(), y.path()).unwrap();
    let tags: Value =
        serde_json::from_str(&fs::read_to_string(x.path().join(TAGS)).unwrap()).unwrap();
    assert_eq!(tags["2"], json!({"v": 6})); // untagged on b after a's tag at v5
    assert_eq!(tags["1"], "Dana");
    let lines = fs::read_to_string(x.path().join(LINES)).unwrap();
    assert!(lines.contains(r#"{"del":true,"id":"2","v":6}"#), "{lines}");
    assert!(!lines.contains("yo"));
}

#[test]
fn a_legacy_vocab_txt_imports_and_goes_away() {
    let (x, y) = (a(), b());
    merge(x.path(), y.path()).unwrap();
    let vocab: Row =
        serde_json::from_str(&fs::read_to_string(x.path().join(VOCAB)).unwrap()).unwrap();
    assert_eq!(vocab.keys().collect::<Vec<_>>(), ["Claude", "Kev", "PR"]);
    assert!(!x.path().join(VOCAB_TXT).exists());
    // and from the other side: b takes in a's vocab.txt words
    let (x, y) = (a(), b());
    merge(y.path(), x.path()).unwrap();
    let vocab: Row =
        serde_json::from_str(&fs::read_to_string(y.path().join(VOCAB)).unwrap()).unwrap();
    assert_eq!(vocab.keys().collect::<Vec<_>>(), ["Claude", "Kev", "PR"]);
}

#[test]
fn a_missing_folder_errors_and_writes_nothing() {
    let x = a();
    let before = snapshot(x.path());
    let r = merge(x.path(), &x.path().join("nope"));
    assert!(r.unwrap_err().starts_with("no folder"));
    assert_eq!(snapshot(x.path()), before);
}

#[test]
fn no_temp_file_survives_a_merge() {
    let (x, y) = (a(), b());
    merge(x.path(), y.path()).unwrap();
    let names: Vec<String> = snapshot(x.path()).into_iter().map(|(n, _)| n).collect();
    assert_eq!(names, [FIXES, LINES, places::FILE, TAGS, VOCAB]);
}
