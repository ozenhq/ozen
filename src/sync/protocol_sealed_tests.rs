//! Everything a Mac sends, as the relay would forward it, carries no plaintext (OFE-28): a full exchange
//! between two Macs and a later edit, including a record big enough to go in parts, scanned frame by frame.
use super::seal::BUCKETS;
use super::tests::{Mac, at, folder};
use serde_json::json;

/// Words from the Macs' data that must never show up in a frame.
const SECRETS: [&str; 7] = [
    "merger-plan-zephyr",
    "Dana Levinson",
    "Noa Abramovich",
    "line-id-7f3a@macbook",
    "line-id-91c2@imac",
    "Quarterly Glorbnax",
    "fix-replacement-quokka",
];

/// Carries frames between `a` and `b` until both are quiet, keeping a copy of every frame.
fn pipe(
    a: &mut Mac,
    b: &mut Mac,
    mut to_b: Vec<Vec<u8>>,
    mut to_a: Vec<Vec<u8>>,
    log: &mut Vec<Vec<u8>>,
) {
    while !to_a.is_empty() || !to_b.is_empty() {
        for f in std::mem::take(&mut to_b) {
            log.push(f.clone());
            to_a.extend(b.receive(&f));
        }
        for f in std::mem::take(&mut to_a) {
            log.push(f.clone());
            to_b.extend(a.receive(&f));
        }
    }
}

#[test]
fn no_frame_a_mac_sends_contains_plaintext() {
    // a record over one frame's budget: it goes in parts
    let long = format!("{} merger-plan-zephyr", "word ".repeat(30_000));
    let da = folder(
        json!([{"id": "line-id-7f3a@macbook", "v": 1, "t": 1.0, "text": long, "spk": "S1"}]),
        json!({"line-id-7f3a@macbook": {"v": 1, "val": "Dana Levinson"}}),
    );
    std::fs::write(
        da.path().join("fixes.json"),
        json!({"fix-1": {"v": 1, "val": "fix-replacement-quokka"}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        da.path().join("vocab.json"),
        json!({"Quarterly Glorbnax": {"v": 1, "val": true}}).to_string(),
    )
    .unwrap();
    let db = folder(
        json!([{"id": "line-id-91c2@imac", "v": 1, "t": 2.0, "text": "the merger-plan-zephyr call"}]),
        json!({"line-id-91c2@imac": {"v": 1, "val": "Noa Abramovich"}}),
    );
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    let mut log = vec![];
    let (ha, hb) = (a.hello(), b.hello());
    pipe(&mut a, &mut b, ha, hb, &mut log);
    assert_eq!(a.synced(), b.synced(), "the exchange went through");
    // a later local edit, sent as it happens
    at(&a.dir, || {
        let tags = json!({"line-id-7f3a@macbook": {"v": 2, "val": "Dana Levinson"},
                          "line-id-91c2@imac": {"v": 2, "val": "Noa Abramovich"}});
        std::fs::write("tags.json", tags.to_string()).unwrap();
    });
    let edits = a.changes();
    assert!(!edits.is_empty());
    pipe(&mut a, &mut b, edits, vec![], &mut log);

    assert!(
        log.len() >= 6,
        "hellos, summaries, records, parts and the edit: {}",
        log.len()
    );
    for (i, f) in log.iter().enumerate() {
        assert!(
            BUCKETS.contains(&f.len()),
            "frame {i} is {} bytes, not a bucket size",
            f.len()
        );
        for s in SECRETS {
            let found = f.windows(s.len()).any(|w| w == s.as_bytes());
            assert!(!found, "frame {i} carries {s:?} in plaintext");
        }
    }
}
