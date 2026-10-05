//! Randomized fuzzing of `Session::receive` (OFE-49): received frames are untrusted (the relay can't
//! check them), so no frame may panic, every frame is either used or dropped and counted, and the
//! files stay readable. Frames are built at every depth: raw bytes; sealed bytes (version and deflate
//! layers); sealed, deflated bytes (JSON layer); and sealed, well-formed messages with hostile fields.
//! Up to 15 frames a case, so more than 8 unfinished summaries force evictions. 64 cases per test run; a long run: `PROPTEST_CASES=200000 cargo nextest run any_frames`.
use super::tests::{Mac, at, folder};
use super::*;
use proptest::prelude::*;
use serde_json::json;
use std::io::Write;

/// Sealed for the test Macs' key and vault (protocol_tests::Mac).
fn sealed(plain: &[u8]) -> Vec<u8> {
    seal::seal(&[7; 32], "vault", plain).unwrap()
}

fn deflated(version: u8, body: &[u8]) -> Vec<u8> {
    let mut z = DeflateEncoder::new(vec![version], Compression::default());
    z.write_all(body).unwrap();
    z.finish().unwrap()
}

fn json_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_map(|f| json!(f)),
        ".{0,12}".prop_map(Value::from),
        prop_oneof![Just("v"), Just("val"), Just("id"), Just("del"), Just("t")]
            .prop_map(Value::from),
    ];
    leaf.prop_recursive(3, 24, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::btree_map(
                prop_oneof![
                    Just("v".to_string()),
                    Just("val".into()),
                    Just("id".into()),
                    Just("del".into()),
                    ".{0,6}"
                ],
                inner,
                0..6
            )
            .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

fn kind() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop_oneof![
            Just("tags".to_string()),
            Just("fixes".into()),
            Just("vocab".into()),
            Just("places".into()),
            Just("lines".into()),
        ],
        1 => ".{0,8}",
    ]
}

/// Keys mostly from a small pool that includes the fixture's records, so received records collide with
/// what's there (higher, lower and equal versions), and sometimes anything.
fn key() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop_oneof![Just("1@a".to_string()), Just("2@b".into()), Just("x".into())],
        1 => ".{0,8}",
    ]
}

/// A record value: mostly the shapes real records have (row or map entry, live or deleted, any
/// version), sometimes arbitrary JSON.
fn record(key: String) -> impl Strategy<Value = Value> {
    prop_oneof![
        3 => (0u64..4, prop::option::of(".{0,6}")).prop_map(move |(v, x)| match x {
            Some(x) => json!({"id": key, "v": v, "t": 1.0, "text": x, "val": x, "label": x}),
            None => json!({"id": key, "v": v, "del": true}),
        }),
        1 => json_value(),
    ]
}

/// A message with the right shape and anything inside it.
fn message() -> impl Strategy<Value = Value> {
    prop_oneof![
        2 => prop_oneof![
            prop::collection::vec(any::<u8>(), 0..5000).prop_map(|b| base64::engine::general_purpose::STANDARD.encode(b)),
            ".{0,40}",
        ]
        .prop_map(|h| json!({"t": "buckets", "h": h})),
        // ids from a small pool, so parts of one summary meet, repeat, and more than 8 unfinished
        // summaries force evictions
        3 => (
            prop_oneof![4 => 0u64..12, 1 => any::<u64>()],
            prop_oneof![4 => 0u32..3, 1 => any::<u32>()],
            prop_oneof![4 => 0u32..4, 1 => any::<u32>()],
            prop::collection::vec((kind(), key(), 0u64..4, ".{0,20}"), 0..20),
            prop::collection::vec(any::<u8>(), 0..300),
        )
            .prop_map(|(id, part, parts, s, b)| json!({"t": "summary", "id": id, "part": part, "parts": parts, "s": s, "b": b})),
        4 => prop::collection::vec((kind(), key()).prop_flat_map(|(k, key)| (Just(k), Just(key.clone()), record(key))), 0..12)
            .prop_map(|r| json!({"t": "records", "r": r})),
        1 => json_value(),
    ]
}

/// A version byte: mostly this protocol's, sometimes an old or newer one.
fn version() -> impl Strategy<Value = u8> {
    prop_oneof![8 => Just(VERSION), 1 => Just(0u8), 1 => any::<u8>()]
}

/// One received frame, at any depth of the format, weighted toward the deep layers.
fn frame() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        1 => prop::collection::vec(any::<u8>(), 0..2048),
        1 => prop::collection::vec(any::<u8>(), 0..2048).prop_map(|p| sealed(&p)),
        // a version byte, then bytes that may not inflate
        2 => (version(), prop::collection::vec(any::<u8>(), 0..2048))
            .prop_map(|(v, b)| sealed(&[&[v], &b[..]].concat())),
        2 => (version(), prop::collection::vec(any::<u8>(), 0..2048))
            .prop_map(|(v, b)| sealed(&deflated(v, &b))),
        12 => message().prop_map(|m| sealed(&deflated(VERSION, m.to_string().as_bytes()))),
        1 => message().prop_map(|m| sealed(&deflated(VERSION + 1, m.to_string().as_bytes()))),
    ]
}

/// Every file in `dir`, name -> bytes.
fn snapshot(dir: &std::path::Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_file())
        .map(|p| {
            (
                p.file_name().unwrap().to_string_lossy().into(),
                std::fs::read(&p).unwrap(),
            )
        })
        .collect()
}

/// The synced files parse as what they must be; `read_synced` would hide a broken one behind a default.
fn files_ok(dir: &std::path::Path) -> Result<(), String> {
    let read = |f: &str| std::fs::read(dir.join(f)).ok();
    for f in [merge::TAGS, merge::FIXES, crate::mcp::VOCAB] {
        if let Some(b) = read(f) {
            serde_json::from_slice::<serde_json::Map<String, Value>>(&b)
                .map_err(|e| format!("{f}: {e}"))?;
        }
    }
    if let Some(b) = read(crate::places::FILE) {
        serde_json::from_slice::<Vec<Value>>(&b).map_err(|e| format!("places: {e}"))?;
    }
    if let Some(b) = read(merge::LINES) {
        for l in String::from_utf8(b).map_err(|e| e.to_string())?.lines() {
            serde_json::from_str::<serde_json::Map<String, Value>>(l)
                .map_err(|e| format!("lines: {e}: {l}"))?;
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn any_frames_are_used_or_dropped_never_a_panic(frames in prop::collection::vec(frame(), 1..16)) {
        let d = folder(
            json!([{"id": "1@a", "v": 1, "t": 1.0, "text": "hi"}]),
            json!({"1@a": {"v": 1, "val": "Dana"}}),
        );
        let mut m = Mac::new(d.path());
        for f in &frames {
            let verdict = m.s.decode(f);
            let before = m.s.dropped.clone();
            let files = snapshot(d.path());
            let had = m.synced();
            m.receive(f); // a panic here fails the case, and proptest shrinks it
            let after = &m.s.dropped;
            match verdict {
                Ok(Some(_)) => prop_assert_eq!((after.bad, after.newer), (before.bad, before.newer)),
                Ok(None) => prop_assert_eq!((after.bad, after.newer), (before.bad, before.newer + 1)),
                Err(_) => prop_assert_eq!((after.bad, after.newer), (before.bad + 1, before.newer)),
            }
            if !matches!(verdict, Ok(Some(_))) {
                prop_assert_eq!(snapshot(d.path()), files, "a dropped frame changed the files");
            }
            prop_assert_eq!(files_ok(d.path()), Ok(()));
            // no record is ever lost or goes back to an older version (a newer one or a tombstone may
            // replace it)
            let have = m.synced();
            for (id, r) in &had {
                let now = have.get(id);
                prop_assert!(now.is_some_and(|n| v(n) >= v(r)), "{:?}: {} became {:?}", id, r, now);
            }
        }
        // what reaches the files is safe for the readers: the panel and relearning run on it
        at(d.path(), || {
            crate::panel::transcript_json("");
            let _ = crate::fixes::relearn();
        });
    }
}
