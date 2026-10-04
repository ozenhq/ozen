//! Bucket hashes at scale (OFE-33): Macs already in sync exchange one small frame however many
//! records they hold, and one edit costs one bucket's summary plus the record.
use super::tests::{Mac, exchange};
use super::*;
use serde_json::json;

/// Two identical folders with `n` short transcript lines and a few tags.
fn twins(n: usize) -> (tempfile::TempDir, tempfile::TempDir) {
    let lines: String = (0..n)
        .map(|i| {
            json!({"id": format!("{}-mic-0@a", 1790520395366u64 + i as u64), "v": 1, "t": i, "text": "hi"})
                .to_string()
                + "\n"
        })
        .collect();
    let tags = json!({"x": {"v": 1, "val": "Dana"}, "y": {"v": 1, "val": "Noa"}}).to_string();
    let make = || {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("lines.jsonl"), &lines).unwrap();
        std::fs::write(d.path().join("tags.json"), &tags).unwrap();
        d
    };
    (make(), make())
}

#[test]
fn macs_in_sync_with_50000_lines_exchange_one_frame_each_under_16_kib() {
    let (da, db) = twins(50_000);
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    let (ha, hb) = (a.hello(), b.hello());
    assert_eq!((ha.len(), hb.len()), (1, 1));
    assert!(ha[0].len() <= 16 * 1024, "{} bytes", ha[0].len());
    assert!(
        b.receive(&ha[0]).is_empty() && a.receive(&hb[0]).is_empty(),
        "nothing else"
    );
}

#[test]
fn one_changed_tag_costs_one_bucket_summary_and_one_record() {
    let (da, db) = twins(50_000);
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    std::fs::write(
        da.path().join("tags.json"),
        json!({"x": {"v": 2, "val": "Gal"}, "y": {"v": 1, "val": "Noa"}}).to_string(),
    )
    .unwrap();
    // B hears A's hashes and summarizes the one bucket that differs
    let summary = b.receive(&a.hello()[0]);
    assert_eq!(summary.len(), 1);
    let plain = seal::open(&[7; 32], "vault", &summary[0]).unwrap();
    let mut json = vec![];
    flate2::read::DeflateDecoder::new(&plain[1..])
        .read_to_end(&mut json)
        .unwrap();
    let msg: Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(msg["b"], json!([buckets::of("tags", "x")]), "one bucket");
    assert!(
        msg["s"].as_array().unwrap().len() < 50_000 / 64,
        "a bucket's records, not all of them"
    );
    // A answers with the one record B lacks
    let records = a.receive(&summary[0]);
    assert_eq!(records.len(), 1);
    assert!(b.receive(&records[0]).is_empty());
    assert_eq!(
        b.synced()[&("tags".into(), "x".into())]["val"],
        json!("Gal")
    );
    assert_eq!(exchange(&mut a, &mut b), 0, "in sync again");
}
