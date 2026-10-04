use super::*;
use proptest::prelude::*;

fn row(v: Value) -> Row {
    v.as_object().unwrap().clone()
}

#[test]
fn old_files_load_as_they_are() {
    let tags = row(json!({"a": "Dana", "b": {"v": 5, "val": "Noa"}, "c": {"v": 6}}));
    assert_eq!(live_map(&tags), row(json!({"a": "Dana", "b": "Noa"})));
    let places = [
        row(json!({"label": "Home", "action": "off"})),
        row(json!({"label": "Work", "action": "record"})),
    ];
    let live = live_rows(&places);
    assert_eq!(live[0]["id"], "0000-Home");
    assert_eq!(live[1]["id"], "0001-Work");
    // saving unchanged rows changes nothing but adding the ids
    assert_eq!(live_rows(&edit_rows(&places, &live)), live);
    // older builds read places.json as [{action, label, ...}]: a deleted place must still parse
    let gone = edit_rows(&places, &live[..1]);
    assert!(
        gone.iter()
            .all(|p| p["label"].is_string() && p["action"].is_string())
    );
}

#[test]
fn editing_a_map_versions_only_what_changed() {
    let raw = row(json!({"a": "Dana", "b": "Noa"}));
    let out = edit_map(raw, &row(json!({"a": "Dana", "c": "Tal"})));
    assert_eq!(out["a"], "Dana");
    assert!(out["b"]["v"].as_u64().unwrap() > 0 && out["b"].get("val").is_none());
    assert_eq!(out["c"]["val"], "Tal");
    assert_eq!(live_map(&out), row(json!({"a": "Dana", "c": "Tal"})));
}

#[test]
fn a_later_edit_wins_and_a_delete_stays_deleted() {
    let base = row(json!({"a": "Dana"}));
    let mine = edit_map(base.clone(), &row(json!({"a": "Noa"})));
    let theirs = edit_map(base.clone(), &Row::new());
    // both newer than the old value, either order gives the same result
    assert_eq!(merge_maps(&mine, &base), mine);
    assert_eq!(merge_maps(&base, &theirs), theirs);
    assert_eq!(merge_maps(&mine, &theirs), merge_maps(&theirs, &mine));

    let lines = [row(json!({"id": "1-mic-0", "t": 1.0, "text": "hi"}))];
    let deleted = edit_rows(&lines, &[]);
    assert_eq!(merge_rows(&lines, &deleted), merge_rows(&deleted, &lines));
    assert!(live_rows(&merge_rows(&lines, &deleted)).is_empty());
}

#[test]
fn ids_minted_here_name_this_mac() {
    assert_eq!(device().len(), 8);
    assert!(mint("1-mic-0").starts_with("1-mic-0@"));
}

#[test]
fn a_version_is_past_the_last_one_even_if_the_clock_went_back() {
    let future = json!({"v": u64::MAX / 2});
    assert_eq!(stamp(Some(&future)), u64::MAX / 2 + 1);
}

fn entry() -> impl Strategy<Value = Value> {
    prop_oneof![
        "[ab]".prop_map(Value::from),
        (0u64..4, "[ab]").prop_map(|(v, s)| json!({"v": v, "val": s})),
        (0u64..4).prop_map(|v| json!({"v": v})),
    ]
}

fn map() -> impl Strategy<Value = Row> {
    prop::collection::btree_map("[xyz]", entry(), 0..4).prop_map(|m| m.into_iter().collect())
}

fn rows() -> impl Strategy<Value = Vec<Row>> {
    prop::collection::vec(
        ("[xyz]", 0u64..4, any::<bool>(), "[ab]").prop_map(|(id, v, del, text)| {
            row(if del {
                json!({"id": id, "v": v, "del": true})
            } else {
                json!({"id": id, "v": v, "text": text})
            })
        }),
        0..4,
    )
    .prop_map(|rs| merge_rows(&rs, &[])) // one row per id, as a file has
}

proptest! {
    #[test]
    fn maps_merge_as_a_crdt(a in map(), b in map(), c in map()) {
        prop_assert_eq!(merge_maps(&a, &b), merge_maps(&b, &a));
        prop_assert_eq!(merge_maps(&merge_maps(&a, &b), &c), merge_maps(&a, &merge_maps(&b, &c)));
        prop_assert_eq!(merge_maps(&a, &a), a);
    }

    #[test]
    fn rows_merge_as_a_crdt(a in rows(), b in rows(), c in rows()) {
        prop_assert_eq!(merge_rows(&a, &b), merge_rows(&b, &a));
        prop_assert_eq!(merge_rows(&merge_rows(&a, &b), &c), merge_rows(&a, &merge_rows(&b, &c)));
        prop_assert_eq!(merge_rows(&a, &a), a);
    }
}

/// Set in the child process `a_writer_killed_before_the_rename_leaves_the_old_file` starts.
const HANG: &str = "OZEN_TEST_HANG_BEFORE_RENAME";

#[test]
fn a_writer_killed_before_the_rename_leaves_the_old_file() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let name = module_path!().split_once("::").unwrap().1.to_string()
        + "::a_writer_killed_before_the_rename_leaves_the_old_file";
    if let Ok(path) = std::env::var(HANG) {
        // the child: write the new file, then hang before the rename until the parent kills it
        let _ = write_atomic_then(&path, b"new", || {
            println!("ready");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(600));
        });
        return;
    }
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("tags.json");
    fs::write(&path, "old").unwrap();
    /// Killed however the test ends, so a failed assertion never leaves the child sleeping.
    struct Kill(std::process::Child);
    impl Drop for Kill {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--nocapture", "--test-threads=1"])
        .env(HANG, &path)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let child = Kill(child);
    assert!(
        out.lines()
            .map_while(Result::ok)
            .any(|l| l.contains("ready")),
        "child never reached the rename (test name filter {name}?)"
    );
    drop(child); // SIGKILL: no destructor runs in the child, like a crash or a force quit
    assert_eq!(fs::read_to_string(&path).unwrap(), "old");
    // the new bytes were fully on disk in the temp file; only the rename was missing
    let tmp: Vec<_> = fs::read_dir(d.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p != &path)
        .collect();
    assert_eq!(tmp.len(), 1);
    assert_eq!(fs::read_to_string(&tmp[0]).unwrap(), "new");
    write_atomic(path.to_str().unwrap(), b"new").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
}

#[test]
fn a_rewrite_keeps_the_file_mode() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("lines.jsonl");
    let p = path.to_str().unwrap();
    write_atomic(p, b"a").unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    write_atomic(p, b"b").unwrap();
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o640
    );
}

#[test]
fn the_next_write_removes_a_crashed_writers_old_temp_file() {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("tags.json");
    let old = d.path().join(".tags.json.abc123.tmp");
    let young = d.path().join(".tags.json.def456.tmp"); // maybe another writer, mid-write
    let other = d.path().join(".fixes.json.abc123.tmp"); // another file's
    for f in [&old, &young, &other] {
        fs::write(f, "partial").unwrap();
    }
    let hour_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
    for f in [&old, &other] {
        fs::File::options()
            .write(true)
            .open(f)
            .unwrap()
            .set_modified(hour_ago)
            .unwrap();
    }
    write_atomic(path.to_str().unwrap(), b"new").unwrap();
    assert!(!old.exists());
    assert!(young.exists() && other.exists());
    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
}
