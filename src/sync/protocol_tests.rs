use super::*;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Runs `f` with `dir` as the working directory, as ozen runs from its folder.
fn at<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();
    let r = f();
    std::env::set_current_dir(back).unwrap();
    r
}

fn folder(lines: Value, tags: Value) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let rows: String = lines
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.to_string() + "\n")
        .collect();
    std::fs::write(d.path().join("lines.jsonl"), rows).unwrap();
    std::fs::write(d.path().join("tags.json"), tags.to_string()).unwrap();
    d
}

/// A Mac: its folder and its session.
struct Mac {
    dir: PathBuf,
    s: Session,
}

impl Mac {
    fn new(dir: &Path) -> Self {
        Mac {
            dir: dir.into(),
            s: Session::with([7; 32], "vault", Coalesced::new(|| {})),
        }
    }
    fn hello(&mut self) -> Vec<Vec<u8>> {
        at(&self.dir, || self.s.hello()).unwrap()
    }
    fn changes(&mut self) -> Vec<Vec<u8>> {
        at(&self.dir, || self.s.changes()).unwrap()
    }
    fn receive(&mut self, f: &[u8]) -> Vec<Vec<u8>> {
        at(&self.dir, || self.s.receive(f)).unwrap()
    }
    fn synced(&self) -> BTreeMap<Id, Value> {
        at(&self.dir, || records(&merge::read_synced("")))
    }
}

/// Carries frames between two Macs the way the relay does (each frame to the other Mac) until both
/// are quiet. Returns how many record frames each way.
fn pipe(a: &mut Mac, b: &mut Mac, mut to_b: Vec<Vec<u8>>, mut to_a: Vec<Vec<u8>>) -> usize {
    let mut replies = 0;
    while !to_a.is_empty() || !to_b.is_empty() {
        for f in std::mem::take(&mut to_b) {
            let r = b.receive(&f);
            replies += r.len();
            to_a.extend(r);
        }
        for f in std::mem::take(&mut to_a) {
            let r = a.receive(&f);
            replies += r.len();
            to_b.extend(r);
        }
    }
    replies
}

/// Both Macs come online together: each sends its summary, each answers.
fn exchange(a: &mut Mac, b: &mut Mac) -> usize {
    let (ha, hb) = (a.hello(), b.hello());
    pipe(a, b, ha, hb)
}

#[test]
fn disjoint_edits_converge_in_one_exchange_and_a_second_sends_only_summaries() {
    let da = folder(
        json!([{"id": "1@a", "v": 1, "t": 1.0, "text": "hi"}]),
        json!({"1@a": {"v": 1, "val": "Dana"}}),
    );
    let db = folder(
        json!([{"id": "2@b", "v": 2, "t": 2.0, "text": "yo"}]),
        json!({"2@b": {"v": 2, "val": "Noa"}}),
    );
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    assert_eq!(exchange(&mut a, &mut b), 2, "one records frame each way");
    let synced = a.synced();
    assert_eq!(synced, b.synced());
    assert_eq!(synced.len(), 4);
    assert_eq!(exchange(&mut a, &mut b), 0, "in sync: summaries only");
}

#[test]
fn an_equal_version_conflict_resolves_the_same_on_both() {
    let da = folder(json!([]), json!({"x": {"v": 5, "val": "Dana"}}));
    let db = folder(json!([]), json!({"x": {"v": 5, "val": "Noa"}}));
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    exchange(&mut a, &mut b);
    let tag = &a.synced()[&("tags".into(), "x".into())];
    assert_eq!(a.synced(), b.synced());
    assert_eq!(
        tag,
        &json!({"v": 5, "val": "Noa"}),
        "the larger JSON wins, as in crdt.rs"
    );
}

#[test]
fn a_deleted_line_stays_deleted() {
    let da = folder(json!([{"id": "2", "v": 9, "del": true}]), json!({}));
    let db = folder(
        json!([{"id": "2", "v": 3, "t": 2.0, "text": "yo"}]),
        json!({}),
    );
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    exchange(&mut a, &mut b);
    for m in [&a, &b] {
        assert_eq!(
            m.synced()[&("lines".into(), "2".into())]["del"],
            json!(true)
        );
    }
}

#[test]
fn a_local_edit_goes_out_alone_and_lands() {
    let da = folder(json!([]), json!({"x": {"v": 1, "val": "Dana"}}));
    let db = folder(json!([]), json!({}));
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    exchange(&mut a, &mut b);
    assert_eq!(
        b.synced(),
        a.synced(),
        "the Mac with nothing got everything"
    );
    assert!(
        a.changes().is_empty() && b.changes().is_empty(),
        "nothing new after the exchange"
    );
    std::fs::write(
        da.path().join("tags.json"),
        json!({"x": {"v": 2, "val": "Noa"}}).to_string(),
    )
    .unwrap();
    let out = a.changes();
    assert_eq!(out.len(), 1);
    assert_eq!(pipe(&mut a, &mut b, out, vec![]), 0);
    assert_eq!(
        b.synced()[&("tags".into(), "x".into())]["val"],
        json!("Noa")
    );
    assert!(
        b.changes().is_empty(),
        "a received record is not echoed back"
    );
}

/// A frame sealed for the test vault with `plain` as its plaintext, whatever it says.
fn raw_frame(plain: &[u8]) -> Vec<u8> {
    seal::seal(&[7; 32], "vault", plain).unwrap()
}

/// Two Macs where A has two tags B lacks, and the records frames A sends B.
fn two_tag_frames() -> (tempfile::TempDir, tempfile::TempDir, Mac, Vec<Vec<u8>>) {
    let da = folder(json!([]), json!({}));
    let db = folder(json!([]), json!({}));
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    exchange(&mut a, &mut b);
    let mut frames = vec![];
    for t in ["x", "y"] {
        std::fs::write(
            da.path().join("tags.json"),
            json!({t: {"v": 1, "val": "Dana"}}).to_string(),
        )
        .unwrap();
        frames.extend(a.changes());
    }
    assert_eq!(frames.len(), 2);
    (da, db, b, frames)
}

#[test]
fn a_garbage_frame_between_two_good_ones_is_dropped_and_counted() {
    let (_da, _db, mut b, good) = two_tag_frames();
    let garbage: Vec<u8> = (0..1024).map(|i| (i * 31 % 251) as u8).collect();
    for f in [&good[0], &garbage, &good[1]] {
        assert!(b.receive(f).is_empty());
    }
    let got = b.synced();
    assert!(got.contains_key(&("tags".into(), "x".into())));
    assert!(
        got.contains_key(&("tags".into(), "y".into())),
        "the session went on"
    );
    assert_eq!(b.s.dropped.bad, 1);
    assert!(b.s.dropped.last_error.is_some());
}

#[test]
fn a_frame_under_an_old_key_or_with_a_flipped_byte_is_dropped_not_merged() {
    let (_da, _db, mut b, good) = two_tag_frames();
    let old_key = seal::seal(&[9; 32], "vault", &good[0][..10]).unwrap();
    let mut flipped = good[0].clone();
    flipped[40] ^= 1;
    let not_a_message = raw_frame(&[VERSION, 1, 2, 3]);
    for f in [&old_key, &flipped, &not_a_message] {
        b.receive(f);
    }
    assert_eq!(b.s.dropped.bad, 3);
    assert!(b.synced().is_empty(), "nothing merged");
}

#[test]
fn a_frame_from_a_newer_protocol_is_held_back_with_update_ozen() {
    let (_da, _db, mut b, good) = two_tag_frames();
    // a v2 Mac's records: whatever follows the version byte, this build must not merge it
    let mut v2 = vec![VERSION + 1];
    v2.extend(&good[0]); // any bytes
    b.receive(&raw_frame(&v2));
    assert!(b.synced().is_empty());
    assert_eq!((b.s.dropped.newer, b.s.dropped.bad), (1, 0));
    assert!(b.s.dropped.advice().unwrap().contains("update ozen"));
    assert_eq!(Dropped::default().advice(), None);
}

#[test]
fn a_dropped_frame_never_reaches_apply() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let runs = std::sync::Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let db = folder(json!([]), json!({}));
    let mut b = Mac {
        dir: db.path().into(),
        s: Session::with(
            [7; 32],
            "vault",
            Coalesced::new(move || {
                r.fetch_add(1, Ordering::SeqCst);
            }),
        ),
    };
    let before = std::fs::read(db.path().join("tags.json")).unwrap();
    for f in [
        raw_frame(b"junk"),
        raw_frame(&[VERSION + 1]),
        b"short".to_vec(),
    ] {
        b.receive(&f);
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(runs.load(Ordering::SeqCst), 0, "no retrain");
    assert_eq!(
        std::fs::read(db.path().join("tags.json")).unwrap(),
        before,
        "files untouched"
    );
    assert!(!db.path().join("vocab.json").exists(), "apply never ran");
}

/// A folder shaped like a real one: `n` lines with 192-float voiceprints and Hebrew text, a tag on most.
fn realistic(n: usize) -> tempfile::TempDir {
    let lines: Vec<Value> = (0..n)
        .map(|i| {
            let e: Vec<f64> = (0..192)
                .map(|j| ((i * 192 + j) as f64).sin() / 10.0)
                .collect();
            json!({"id": format!("{}-mic-{}", 1790520395366u64 + i as u64 * 7919, i % 3),
                   "v": 1790520399270u64 + i as u64 * 7919, "t": 1790520399.27 + i as f64 * 7.9,
                   "d": 2.94, "src": "room", "spk": format!("S{}", i % 4),
                   "text": "יואי, זה מזהה את הקול שלי ונשמע טוב מאוד היום.", "e": e})
        })
        .collect();
    let tags: serde_json::Map<String, Value> = (0..n / 10)
        .map(|i| {
            (
                format!("{}-mic-0", 1790520395366u64 + i as u64 * 7919),
                json!({"v": i, "val": "Dana"}),
            )
        })
        .collect();
    folder(Value::Array(lines), Value::Object(tags))
}

fn sizes(dir: &Path) -> (usize, usize) {
    let mut m = Mac::new(dir);
    let summary: usize = m.hello().iter().map(Vec::len).sum();
    let tags = dir.join("tags.json");
    let mut t: serde_json::Map<String, Value> =
        serde_json::from_slice(&std::fs::read(&tags).unwrap_or_default()).unwrap_or_default();
    t.insert("new-tag".into(), json!({"v": 1, "val": "Noa"}));
    std::fs::write(&tags, Value::Object(t).to_string()).unwrap();
    let edit = m.changes();
    assert_eq!(edit.len(), 1, "a one-tag edit is one frame");
    (summary, edit[0].len())
}

#[test]
fn a_481_line_summary_is_under_30_kb_and_a_tag_edit_one_small_frame() {
    let d = realistic(481);
    let (summary, edit) = sizes(d.path());
    assert!(summary < 30_000, "summary {summary} bytes");
    assert!(edit <= 4096, "edit frame {edit} bytes");
}

/// On a copy of a real ozen folder: `OZEN_REAL_COPY=/path/to/copy cargo test -- --ignored real_folder`.
#[test]
#[ignore]
fn real_folder_sizes() {
    let dir = PathBuf::from(std::env::var("OZEN_REAL_COPY").expect("OZEN_REAL_COPY"));
    let lines = std::fs::read_to_string(dir.join("lines.jsonl"))
        .unwrap()
        .lines()
        .count();
    let (summary, edit) = sizes(&dir);
    eprintln!("{lines} lines: summary {summary} bytes, one-tag edit {edit} bytes");
    assert!(summary < 30_000 && edit <= 4096);
}

#[test]
fn a_mac_with_no_files_at_all_gets_everything_of_every_kind() {
    let da = folder(
        json!([{"id": "1@a", "v": 1, "text": "hi"}]),
        json!({"1@a": "Dana"}),
    );
    std::fs::write(
        da.path().join("fixes.json"),
        json!({"1@a": "hi there"}).to_string(),
    )
    .unwrap();
    std::fs::write(da.path().join("vocab.txt"), "Kev, PR\n").unwrap();
    std::fs::write(
        da.path().join("places.json"),
        json!([{"label": "Home", "action": "off"}]).to_string(),
    )
    .unwrap();
    let db = tempfile::tempdir().unwrap();
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    exchange(&mut a, &mut b);
    let got = b.synced();
    assert_eq!(got, a.synced());
    for kind in ["lines", "tags", "fixes", "vocab", "places"] {
        assert!(got.keys().any(|(k, _)| k == kind), "no {kind}");
    }
}

#[test]
fn an_edit_that_wins_over_a_received_one_still_goes_out() {
    let da = folder(json!([]), json!({"x": {"v": 1, "val": "Dana"}}));
    let db = folder(json!([]), json!({}));
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    exchange(&mut a, &mut b);
    // both edit x before hearing from the other; B's is newer
    let tags = |d: &Path, v: u64, val: &str| {
        std::fs::write(
            d.join("tags.json"),
            json!({"x": {"v": v, "val": val}}).to_string(),
        )
        .unwrap()
    };
    tags(db.path(), 3, "Noa");
    tags(da.path(), 2, "Gal");
    let from_a = a.changes();
    pipe(&mut a, &mut b, from_a, vec![]);
    let from_b = b.changes();
    assert_eq!(from_b.len(), 1, "B's winning edit is still sent");
    pipe(&mut a, &mut b, vec![], from_b);
    for m in [&a, &b] {
        assert_eq!(
            m.synced()[&("tags".into(), "x".into())]["val"],
            json!("Noa")
        );
    }
}

#[test]
fn a_record_too_big_for_a_frame_stays_and_the_rest_go() {
    let da = folder(json!([]), json!({}));
    let db = folder(json!([]), json!({}));
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    exchange(&mut a, &mut b);
    let huge = "x".repeat(seal::MAX);
    std::fs::write(
        da.path().join("tags.json"),
        json!({"big": {"v": 1, "val": huge}, "small": {"v": 1, "val": "Dana"}}).to_string(),
    )
    .unwrap();
    let out = a.changes();
    assert_eq!(out.len(), 1);
    pipe(&mut a, &mut b, out, vec![]);
    let got = b.synced();
    assert!(got.contains_key(&("tags".into(), "small".into())));
    assert!(!got.contains_key(&("tags".into(), "big".into())));
}

#[test]
fn a_summary_in_many_parts_arriving_twice_and_out_of_order_still_answers_once() {
    let tags: serde_json::Map<String, Value> = (0..4000)
        .map(|i| (format!("tag-{i:05}"), json!({"v": i, "val": "Dana"})))
        .collect();
    let da = folder(json!([]), Value::Object(tags));
    let db = folder(json!([]), json!({"only-b": {"v": 1, "val": "Noa"}}));
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    let hb = b.hello();
    let mut ha = a.hello();
    assert!(ha.len() > 1, "{} part(s)", ha.len());
    ha.reverse();
    let dup = ha[1..].to_vec(); // every part but the last-sent arrives twice, before completion
    ha.splice(1..1, dup);
    let mut replies = vec![];
    for f in &ha {
        replies.extend(b.receive(f));
    }
    assert_eq!(
        replies.len(),
        1,
        "B answers the summary once, with the record A lacks"
    );
    replies.extend(hb);
    pipe(&mut a, &mut b, vec![], replies); // B's answer and B's own summary, to A
    assert_eq!(b.synced().len(), 4001);
    assert_eq!(a.synced(), b.synced());
}

#[test]
fn record_hashes_are_pinned() {
    // Every Mac must hash a record identically: serde_json writes keys sorted and floats round-trip.
    assert_eq!(
        mark(&json!({"val": "Dana", "v": 1})),
        (1, "d9b8eb182db8a62b".into())
    );
}

#[test]
fn received_records_that_change_something_retrain_once() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let runs = std::sync::Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let da = folder(json!([]), json!({"x": {"v": 1, "val": "Dana"}}));
    let db = folder(json!([]), json!({}));
    let mut a = Mac::new(da.path());
    let mut b = Mac {
        dir: db.path().into(),
        s: Session::with(
            [7; 32],
            "vault",
            Coalesced::new(move || {
                r.fetch_add(1, Ordering::SeqCst);
            }),
        ),
    };
    exchange(&mut a, &mut b);
    exchange(&mut a, &mut b); // in sync now: nothing lands, nothing retrains
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
}
