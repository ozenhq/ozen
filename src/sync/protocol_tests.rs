use super::*;
use crate::sync::key::key;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Runs `f` with `dir` as the working directory, as ozen runs from its folder.
pub(super) fn at<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();
    let r = f();
    std::env::set_current_dir(back).unwrap();
    r
}

pub(super) fn folder(lines: Value, tags: Value) -> tempfile::TempDir {
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
pub(super) struct Mac {
    pub(super) dir: PathBuf,
    pub(super) s: Session,
}

impl Mac {
    pub(super) fn new(dir: &Path) -> Self {
        Mac {
            dir: dir.into(),
            s: Session::with(key([7; 32]), "vault", Coalesced::new(|| {})),
        }
    }
    pub(super) fn hello(&mut self) -> Vec<Vec<u8>> {
        bytes(at(&self.dir, || self.s.hello()).unwrap())
    }
    pub(super) fn changes(&mut self) -> Vec<Vec<u8>> {
        bytes(at(&self.dir, || self.s.changes()).unwrap())
    }
    pub(super) fn receive(&mut self, f: &[u8]) -> Vec<Vec<u8>> {
        bytes(at(&self.dir, || self.s.receive(f)).unwrap())
    }
    pub(super) fn synced(&self) -> BTreeMap<Id, Value> {
        at(&self.dir, || records(&merge::read_synced(""), VERSION))
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
pub(super) fn exchange(a: &mut Mac, b: &mut Mac) -> usize {
    let (ha, hb) = (a.hello(), b.hello());
    pipe(a, b, ha, hb)
}

#[test]
fn disjoint_edits_converge_in_one_exchange_and_a_second_sends_only_bucket_hashes() {
    let da = folder(
        json!([{"id": "1@a", "v": 1, "t": 1.0, "text": "hi"}]),
        json!({"1@a": {"v": 1, "val": "Dana"}}),
    );
    let db = folder(
        json!([{"id": "2@b", "v": 2, "t": 2.0, "text": "yo"}]),
        json!({"2@b": {"v": 2, "val": "Noa"}}),
    );
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    assert_eq!(
        exchange(&mut a, &mut b),
        4,
        "each way: one summary of the differing buckets, then one records frame"
    );
    let synced = a.synced();
    assert_eq!(synced, b.synced());
    assert_eq!(synced.len(), 4);
    assert_eq!(exchange(&mut a, &mut b), 0, "in sync: bucket hashes only");
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
fn a_record_too_big_for_a_frame_goes_in_parts_with_the_rest() {
    // OFE-76: it used to stay behind for good; one over parts::CAP still does
    let da = folder(json!([]), json!({}));
    let db = folder(json!([]), json!({}));
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    exchange(&mut a, &mut b);
    let (huge, too_big) = ("x".repeat(seal::MAX), "x".repeat(parts::CAP + 1));
    std::fs::write(
        da.path().join("tags.json"),
        json!({"big": {"v": 1, "val": huge}, "small": {"v": 1, "val": "Dana"},
               "too-big": {"v": 1, "val": too_big}})
        .to_string(),
    )
    .unwrap();
    let out = a.changes();
    assert_eq!(out.len(), 3, "small in one frame, big in two parts");
    pipe(&mut a, &mut b, out, vec![]);
    let got = b.synced();
    assert!(got.contains_key(&("tags".into(), "small".into())));
    assert_eq!(got[&("tags".into(), "big".into())]["val"], huge);
    assert!(!got.contains_key(&("tags".into(), "too-big".into())));
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
    assert_eq!(hb.len(), 1, "bucket hashes: one frame");
    let mut sa = a.receive(&hb[0]); // A's summary of the buckets that differ: all of its 4000 tags
    assert!(sa.len() > 1, "{} part(s)", sa.len());
    sa.reverse();
    let dup = sa[1..].to_vec(); // every part but the last-sent arrives twice, before completion
    sa.splice(1..1, dup);
    let mut replies = vec![];
    for f in &sa {
        replies.extend(b.receive(f));
    }
    assert_eq!(
        replies.len(),
        1,
        "B answers the summary once, with the record A lacks"
    );
    let ha = a.hello();
    pipe(&mut a, &mut b, ha, replies); // A's hashes to B, B's answer to A
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
            key([7; 32]),
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

#[path = "protocol_local_fields_tests.rs"]
mod local_fields;

/// Sealed frames as bytes, for tests that look inside or tamper with them.
pub(super) fn bytes(frames: Vec<crate::sync::seal::Sealed>) -> Vec<Vec<u8>> {
    frames
        .into_iter()
        .map(crate::sync::seal::Sealed::into_bytes)
        .collect()
}

#[test]
fn a_connection_that_stays_up_says_hello_again_every_hour() {
    let d = folder(json!([]), json!({}));
    let mut a = Mac::new(d.path());
    a.hello();
    assert!(
        at(d.path(), || a.s.tick()).unwrap().is_empty(),
        "nothing new"
    );
    a.s.said_hello = Some(std::time::Instant::now() - super::super::status::HELLO_EVERY);
    let again = at(d.path(), || a.s.tick()).unwrap();
    let b = Mac::new(d.path());
    assert_eq!(again.len(), 1);
    assert!(matches!(
        b.s.decode(&again[0]),
        Ok(Some(Msg::Buckets { .. }))
    ));
}
