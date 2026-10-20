//! Frames a Mac can't use: dropped and counted, never merged (OFE-22).
use super::tests::{Mac, exchange, folder};
use super::*;
use serde_json::json;

/// A frame sealed for the test vault with `plain` as its plaintext, whatever it says.
fn raw_frame(plain: &[u8]) -> Vec<u8> {
    seal::seal(&crate::sync::key::key([7; 32]), "vault", plain)
        .unwrap()
        .into_bytes()
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
    // a real records frame from a Mac still on another key (after a rotate)
    let dc = folder(json!([]), json!({"z": {"v": 1, "val": "Gal"}}));
    let mut c = Mac {
        dir: dc.path().into(),
        s: Session::with(
            crate::sync::key::key([9; 32]),
            "vault",
            Coalesced::new(|| {}),
        ),
    };
    let old_key = c.changes();
    assert_eq!(old_key.len(), 1);
    let mut flipped = good[0].clone();
    flipped[40] ^= 1;
    let not_a_message = raw_frame(&[VERSION, 1, 2, 3]);
    let version_0 = raw_frame(&[0]);
    let empty = raw_frame(&[]);
    for f in [&old_key[0], &flipped, &not_a_message, &version_0, &empty] {
        b.receive(f);
    }
    assert_eq!(b.s.dropped.bad, 5);
    assert!(b.synced().is_empty(), "nothing merged");
}

#[test]
fn a_frame_inflating_past_a_frame_of_json_is_dropped() {
    use flate2::{Compression, write::DeflateEncoder};
    use std::io::Write;
    let (_da, _db, mut b, _) = two_tag_frames();
    let mut z = DeflateEncoder::new(vec![VERSION], Compression::best());
    z.write_all(&vec![b' '; 4 << 20]).unwrap(); // 4 MiB of spaces deflates to a few KiB
    b.receive(&raw_frame(&z.finish().unwrap()));
    assert_eq!(b.s.dropped.bad, 1);
}

#[test]
fn a_frame_from_a_newer_protocol_is_held_back_with_update_ozen() {
    let (_da, _db, mut b, good) = two_tag_frames();
    // a v2 Mac's records frame, otherwise valid for v1: this build must still not merge it
    let mut v2 = seal::open(&crate::sync::key::key([7; 32]), "vault", &good[0]).unwrap();
    v2[0] = VERSION + 1;
    b.receive(&raw_frame(&v2));
    assert!(b.synced().is_empty());
    assert_eq!((b.s.dropped.newer, b.s.dropped.bad), (1, 0));
    assert!(b.s.dropped.advice().unwrap().contains("update ozen"));
    assert!(
        b.s.dropped
            .last_error
            .as_deref()
            .unwrap()
            .contains(&format!("protocol {}", VERSION + 1))
    );
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
            crate::sync::key::key([7; 32]),
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

/// A records frame for the test vault carrying `r`, as another Mac would send it.
fn records_frame(r: Value) -> Vec<u8> {
    let mut z = DeflateEncoder::new(vec![VERSION], Compression::default());
    serde_json::to_writer(&mut z, &json!({"t": "records", "r": r})).unwrap();
    seal::seal(
        &crate::sync::key::key([7; 32]),
        "vault",
        &z.finish().unwrap(),
    )
    .unwrap()
    .into_bytes()
}

#[test]
fn malformed_records_are_dropped_and_counted_and_the_good_ones_still_merge() {
    let d = folder(
        json!([{"id": "1@a", "v": 1, "t": 1.0, "text": "hi"}]),
        json!({}),
    );
    let mut m = Mac::new(d.path());
    let f = records_frame(json!([
        ["lines", "1@a", {"id": "1@a", "v": 5, "t": "noon", "text": "overwritten?"}],
        ["lines", "2@b", {"id": "2@b", "v": 1, "t": 2.0, "text": "x", "e": [0.1, 0.2, 0.3]}],
        ["tags", "1@a", {"v": 1, "val": 7}],
        ["tags", "2@b", {"v": 1, "val": "Noa"}],
        ["lines", "3@b", {"id": "3@b", "v": 1, "t": 3.0, "text": "fine"}],
    ]));
    assert!(m.receive(&f).is_empty());
    assert_eq!(m.s.dropped.records, 3);
    assert_eq!(
        (m.s.dropped.bad, m.s.dropped.newer),
        (0, 0),
        "the frame itself was fine"
    );
    assert!(
        m.s.dropped
            .last_error
            .as_deref()
            .unwrap_or("")
            .contains("1@a")
    );
    let got = m.synced();
    assert_eq!(
        got[&("lines".into(), "1@a".into())]["text"],
        "hi",
        "the bad version didn't win"
    );
    assert!(!got.contains_key(&("lines".into(), "2@b".into())));
    assert!(!got.contains_key(&("tags".into(), "1@a".into())));
    assert_eq!(got[&("tags".into(), "2@b".into())]["val"], "Noa");
    assert_eq!(got[&("lines".into(), "3@b".into())]["text"], "fine");
    let after = std::fs::read_to_string(d.path().join("lines.jsonl")).unwrap();
    assert!(!after.contains("noon"));
}

#[test]
fn two_versions_of_one_key_in_a_frame_keep_the_winner_whatever_their_order() {
    let newer = json!(["tags", "x", {"v": 5, "val": "Noa"}]);
    let older = json!(["tags", "x", {"v": 2, "val": "Dana"}]);
    for r in [json!([newer, older]), json!([older, newer])] {
        let d = folder(json!([]), json!({}));
        let mut m = Mac::new(d.path());
        m.receive(&records_frame(r));
        assert_eq!(m.synced()[&("tags".into(), "x".into())]["val"], "Noa");
    }
}

#[test]
fn a_flood_of_summary_parts_leaves_the_session_working() {
    // OFE-65: a peer streaming parts of summaries that never complete can't grow memory past
    // summaries.rs's caps, and a normal exchange afterwards still syncs
    let da = folder(json!([]), json!({"x": {"v": 1, "val": "Dana"}}));
    let db = folder(json!([]), json!({}));
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    for i in 0..20_000u32 {
        let junk = Msg::Summary {
            id: u64::from(i / 3000),
            part: i % 3000,
            parts: super::super::summaries::PARTS as u32,
            s: vec![("tags".into(), format!("k{i}"), 1, "0123456789abcdef".into())],
            b: vec![0],
        };
        assert!(b.receive(&b.s.frame(&junk).unwrap()).is_empty());
    }
    assert!(
        b.s.dropped.refused > 0,
        "parts over the cap were dropped and counted"
    );
    exchange(&mut a, &mut b);
    assert!(b.synced().contains_key(&("tags".into(), "x".into())));
}

/// What `ozen health` prints in `dir` (OFE-41).
fn health_in(dir: &std::path::Path) -> Vec<String> {
    super::tests::at(dir, crate::sync::dropped::health)
}

#[test]
fn health_says_wrong_key_after_a_garbage_frame() {
    let (_da, db, mut b, good) = two_tag_frames();
    assert!(health_in(db.path()).is_empty(), "no drops, nothing said");
    b.receive(&good[0]);
    assert!(health_in(db.path()).is_empty(), "a good frame says nothing");
    let garbage: Vec<u8> = (0..1024).map(|i| (i * 31 % 251) as u8).collect();
    b.receive(&garbage);
    b.receive(&garbage);
    assert_eq!(b.s.dropped.bad, 2);
    let said = health_in(db.path());
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].starts_with("2 frames from another Mac couldn't be read (wrong key?"));
}

#[test]
fn health_says_update_ozen_after_a_newer_frame_and_counts_add_up_across_sessions() {
    let (_da, db, mut b, good) = two_tag_frames();
    let mut v2 = seal::open(&crate::sync::key::key([7; 32]), "vault", &good[0]).unwrap();
    v2[0] = VERSION + 1;
    b.receive(&raw_frame(&v2));
    assert_eq!(
        health_in(db.path()),
        ["another Mac runs a newer ozen: update ozen to sync with it"]
    );
    // a later sync process starts its counts at zero; the day's file keeps adding
    let mut again = Mac::new(db.path());
    again.receive(&[1; 1024]);
    let said = health_in(db.path());
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(said[1].starts_with("1 frame from"));
}

#[test]
fn health_forgets_drops_a_day_old() {
    let (_da, db, mut b, _) = two_tag_frames();
    b.receive(&[1; 1024]);
    let file = db.path().join(crate::sync::dropped::FILE);
    let mut seen: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    seen["at"] = json!(seen["at"].as_u64().unwrap() - 24 * 60 * 60);
    std::fs::write(&file, seen.to_string()).unwrap();
    assert!(health_in(db.path()).is_empty());
}

#[test]
fn health_names_the_records_error_even_after_another_kind_of_drop() {
    let d = folder(json!([]), json!({}));
    let mut m = Mac::new(d.path());
    m.receive(&records_frame(json!([["tags", "x", {"v": "one"}]])));
    m.receive(&[1; 1024]); // a later unreadable frame overwrites last_error
    let said = health_in(d.path());
    assert_eq!(said.len(), 2, "{said:?}");
    assert!(
        said[1].starts_with("1 record from another Mac failed checks"),
        "{said:?}"
    );
    assert!(
        !said[1].contains("(last: unknown)") && !said[1].contains("open"),
        "{said:?}"
    );
}
