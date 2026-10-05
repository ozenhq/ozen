//! Every place that names the synced kinds agrees with `crdt::SYNCED` (OFE-59): adding a kind to the
//! table but not to merge.rs, the protocol or valid.rs, or a file to merge.rs but not the table, fails here.
use super::tests::at;
use super::*;
use crate::crdt::SYNCED;
use serde_json::json;

/// A valid record of `kind` keyed `k`.
fn record(kind: &str, k: &str) -> Value {
    match kind {
        "lines" => json!({"id": k, "v": 1, "t": 1.0, "text": "hi"}),
        "places" => json!({"id": k, "v": 1, "label": "Home", "action": "record"}),
        "vocab" => json!({"v": 1, "val": true}),
        _ => json!({"v": 1, "val": "Dana"}),
    }
}

#[test]
fn the_table_names_exactly_the_files_merge_reads_and_writes() {
    let mut files: Vec<&str> = SYNCED.iter().map(|(_, f)| *f).collect();
    files.sort();
    let mut known = [
        merge::LINES,
        merge::TAGS,
        merge::FIXES,
        crate::mcp::VOCAB,
        crate::places::FILE,
    ];
    known.sort();
    assert_eq!(files, known);
}

#[test]
fn every_synced_kind_is_read_from_its_file_sent_and_received() {
    for (kind, file) in SYNCED {
        let d = tempfile::tempdir().unwrap();
        let r = record(kind, "k1");
        // the file as ozen keeps it: JSON lines, an array of rows, or a map
        let text = match Synced::default()
            .get(kind)
            .expect("merge::Synced has every kind")
        {
            Records::Rows(_) if file.ends_with(".jsonl") => format!("{r}\n"),
            Records::Rows(_) => json!([r]).to_string(),
            Records::Map(_) => json!({"k1": r}).to_string(),
        };
        std::fs::write(d.path().join(file), text).unwrap();
        let read = at(d.path(), || records(&merge::read_synced("")));
        let id = (kind.to_string(), "k1".to_string());
        assert_eq!(read.get(&id), Some(&r), "{kind} read from {file}");
        let got = records(&synced(vec![(kind.into(), "k1".into(), r.clone())]));
        assert_eq!(got.get(&id), Some(&r), "{kind} received");
        assert!(
            super::super::valid::record(kind, "k1", &r).is_ok(),
            "{kind} valid"
        );
        assert!(
            super::super::valid::record(kind, "k1", &json!(42)).is_err(),
            "valid.rs checks {kind}"
        );
    }
    // a kind no build knows passes valid.rs and is left out, so the checks above aren't vacuous
    assert!(super::super::valid::record("future", "k1", &json!(42)).is_ok());
    assert!(records(&synced(vec![("future".into(), "k1".into(), json!(42))])).is_empty());
}
