//! Lines go without the fields only their Mac means (OFE-56, `crdt::LOCAL_LINE_FIELDS`), from v2.
use super::*;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A transcribed line as the transcriber writes it, with this Mac's guess `spk`, `doubt` and `run`.
fn transcribed(spk: &str, doubt: f64, run: i64) -> Value {
    json!({"id": "1@a", "v": 1, "t": 1.0, "d": 2.0, "src": "mic", "run": run, "spk": spk,
           "doubt": doubt, "heard": "hy", "text": "hi", "e": vec![0.1; 192]})
}

fn keys(r: &Value) -> BTreeSet<&str> {
    r.as_object().unwrap().keys().map(String::as_str).collect()
}

const SYNCED: [&str; 8] = ["d", "e", "heard", "id", "src", "t", "text", "v"];

#[test]
fn a_line_is_synced_without_its_local_fields_but_a_note_keeps_its_speaker() {
    let r = crate::crdt::synced_line(transcribed("Dana", 0.3, 7).as_object().unwrap().clone());
    assert_eq!(keys(&Value::Object(r)), SYNCED.into());
    let note = json!({"id": "2@a", "v": 1, "t": 2.0, "src": "note", "spk": "Dana", "text": "todo"});
    let kept = crate::crdt::synced_line(note.as_object().unwrap().clone());
    assert_eq!(
        Value::Object(kept),
        note,
        "a note's speaker is what the user typed"
    );
}

#[test]
fn a_received_line_carries_only_the_synced_fields_in_v2_and_everything_in_v1() {
    for (own, want) in [
        (2, SYNCED.to_vec()),
        (1, keys(&transcribed("Dana", 0.3, 7)).into_iter().collect()),
    ] {
        let da = folder(json!([transcribed("Dana", 0.3, 7)]), json!({}));
        let db = folder(json!([]), json!({}));
        let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
        (a.s.versions, b.s.versions) = (Versions::new(own), Versions::new(own));
        exchange(&mut a, &mut b);
        assert_eq!(b.s.dropped, Dropped::default(), "v{own}");
        let stored = std::fs::read_to_string(db.path().join("lines.jsonl")).unwrap();
        let line: Value = serde_json::from_str(stored.lines().next().unwrap()).unwrap();
        assert_eq!(keys(&line), want.into_iter().collect(), "v{own}");
    }
}

#[test]
fn macs_whose_lines_differ_only_in_local_fields_are_in_sync() {
    let da = folder(json!([transcribed("Dana", 0.3, 1)]), json!({}));
    let db = folder(json!([transcribed("Noa", 0.9, 5)]), json!({}));
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    assert_eq!(
        exchange(&mut a, &mut b),
        0,
        "bucket hashes only: nothing to send"
    );
    // each Mac keeps its own guess
    assert!(
        std::fs::read_to_string(db.path().join("lines.jsonl"))
            .unwrap()
            .contains("Noa")
    );
}

#[test]
fn a_received_line_without_a_speaker_guess_retrains_here() {
    let da = folder(json!([transcribed("Dana", 0.3, 1)]), json!({}));
    let db = folder(json!([]), json!({}));
    let retrains = Arc::new(AtomicUsize::new(0));
    let n = retrains.clone();
    let mut a = Mac::new(da.path());
    let mut b = Mac::new(db.path());
    b.s = Session::with(
        [7; 32],
        "vault",
        Coalesced::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
        }),
    );
    exchange(&mut a, &mut b);
    let started = std::time::Instant::now();
    while retrains.load(Ordering::SeqCst) == 0 {
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "no retrain"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
