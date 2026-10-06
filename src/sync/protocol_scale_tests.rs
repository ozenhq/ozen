//! Bucket hashes at scale (OFE-33): Macs already in sync exchange one small frame however many
//! records they hold, and one edit costs one bucket's summary plus the record.
use super::tests::{Mac, exchange};
use super::*;
use serde_json::json;

/// The JSON message inside one of the test vault's frames.
fn message(frame: &[u8]) -> Value {
    let plain = seal::open(&crate::sync::key::key([7; 32]), "vault", frame).unwrap();
    let mut json = vec![];
    flate2::read::DeflateDecoder::new(&plain[1..])
        .read_to_end(&mut json)
        .unwrap();
    serde_json::from_slice(&json).unwrap()
}

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
    let plain = seal::open(&crate::sync::key::key([7; 32]), "vault", &summary[0]).unwrap();
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
    assert_eq!(
        message(&records[0])["r"].as_array().unwrap().len(),
        1,
        "one record"
    );
    assert!(b.receive(&records[0]).is_empty());
    assert_eq!(
        b.synced()[&("tags".into(), "x".into())]["val"],
        json!("Gal")
    );
    assert_eq!(exchange(&mut a, &mut b), 0, "in sync again");
}

#[test]
fn a_summary_scoped_to_one_bucket_is_answered_only_from_that_bucket() {
    let (da, db) = twins(2_000);
    let (mut a, b) = (Mac::new(da.path()), Mac::new(db.path()));
    // B never had x or y: an empty summary of x's bucket asks A for x's bucket only
    std::fs::write(db.path().join("tags.json"), "{}").unwrap();
    let bx = buckets::of("tags", "x");
    let summary =
        b.s.frame(&Msg::Summary {
            id: 1,
            part: 0,
            parts: 1,
            s: vec![],
            b: vec![bx],
        })
        .unwrap();
    let records = a.receive(&summary);
    let sent: Vec<Value> = records
        .iter()
        .flat_map(|f| message(f)["r"].as_array().unwrap().clone())
        .collect();
    assert!(!sent.is_empty());
    for r in &sent {
        let (k, key) = (r[0].as_str().unwrap(), r[1].as_str().unwrap());
        assert_eq!(
            buckets::of(k, key),
            bx,
            "{k} {key} is outside the asked bucket"
        );
    }
}

#[test]
fn garbled_bucket_hashes_get_a_full_summary_not_a_crash() {
    let (da, _db) = twins(100);
    let mut a = Mac::new(da.path());
    let bogus = Mac::new(da.path())
        .s
        .frame(&Msg::Buckets {
            h: "not base64!".into(),
            max: 1,
            from: None,
        })
        .unwrap();
    let summary = a.receive(&bogus);
    let covered: usize = summary
        .iter()
        .map(|f| message(f)["s"].as_array().unwrap().len())
        .sum();
    assert_eq!(covered, 102, "every record: 100 lines and 2 tags");
}
