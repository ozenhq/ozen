//! Randomized fuzzing of `Session::receive` (OFE-49): received frames are untrusted (the relay can't
//! check them), so no frame may panic, every frame is either used or dropped and counted, and the
//! files stay readable. Frames are built at every depth: raw bytes; sealed bytes (version and deflate
//! layers); sealed, deflated bytes (JSON layer); and sealed, well-formed messages with hostile fields.
//! 64 cases per test run; a long run: `PROPTEST_CASES=200000 cargo nextest run any_frames`.
use super::tests::{Mac, folder};
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
        Just("tags".to_string()),
        Just("fixes".into()),
        Just("vocab".into()),
        Just("places".into()),
        Just("lines".into()),
        ".{0,8}",
    ]
}

/// A message with the right shape and anything inside it.
fn message() -> impl Strategy<Value = Value> {
    prop_oneof![
        prop_oneof![
            prop::collection::vec(any::<u8>(), 0..5000).prop_map(|b| base64::engine::general_purpose::STANDARD.encode(b)),
            ".{0,40}",
        ]
        .prop_map(|h| json!({"t": "buckets", "h": h})),
        (
            any::<u64>(),
            any::<u32>(),
            prop_oneof![Just(0u32), Just(1), 1u32..4, any::<u32>()],
            prop::collection::vec((kind(), ".{0,8}", any::<u64>(), ".{0,20}"), 0..20),
            prop::collection::vec(any::<u8>(), 0..300),
        )
            .prop_map(|(id, part, parts, s, b)| json!({"t": "summary", "id": id, "part": part, "parts": parts, "s": s, "b": b})),
        prop::collection::vec((kind(), ".{0,8}", json_value()), 0..12)
            .prop_map(|r| json!({"t": "records", "r": r})),
        json_value(),
    ]
}

/// One received frame, at any depth of the format.
fn frame() -> impl Strategy<Value = Vec<u8>> {
    prop_oneof![
        prop::collection::vec(any::<u8>(), 0..2048),
        prop::collection::vec(any::<u8>(), 0..2048).prop_map(|p| sealed(&p)),
        (any::<u8>(), prop::collection::vec(any::<u8>(), 0..2048))
            .prop_map(|(v, b)| sealed(&deflated(v, &b))),
        message().prop_map(|m| sealed(&deflated(VERSION, m.to_string().as_bytes()))),
        message().prop_map(|m| sealed(&deflated(VERSION + 1, m.to_string().as_bytes()))),
    ]
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
    fn any_frames_are_used_or_dropped_never_a_panic(frames in prop::collection::vec(frame(), 1..8)) {
        let d = folder(
            json!([{"id": "1@a", "v": 1, "t": 1.0, "text": "hi"}]),
            json!({"1@a": {"v": 1, "val": "Dana"}}),
        );
        let mut m = Mac::new(d.path());
        for f in &frames {
            let verdict = m.s.decode(f);
            let before = m.s.dropped.clone();
            m.receive(f); // a panic here fails the case, and proptest shrinks it
            let after = &m.s.dropped;
            match verdict {
                Ok(Some(_)) => prop_assert_eq!((after.bad, after.newer), (before.bad, before.newer)),
                Ok(None) => prop_assert_eq!((after.bad, after.newer), (before.bad, before.newer + 1)),
                Err(_) => prop_assert_eq!((after.bad, after.newer), (before.bad + 1, before.newer)),
            }
            prop_assert_eq!(files_ok(d.path()), Ok(()));
        }
    }
}
