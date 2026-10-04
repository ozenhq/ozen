//! Frames a Mac can't use: dropped and counted, never merged (OFE-22).
use super::tests::{Mac, exchange, folder};
use super::*;
use serde_json::json;

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
