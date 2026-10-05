//! The wire format, frozen (OFE-42). Macs update ozen at different times and no server sits between
//! them, so a renamed or reordered field, or a change to deflate-then-seal framing, would silently stop
//! two of a user's Macs from syncing. The JSON snapshots fail on any such change, and the sealed v1
//! frames checked in under tests/fixtures/sync-golden/ must still open, parse and merge. Changing the
//! wire format on purpose means bumping VERSION, reviewing the snapshots (`cargo insta review`) and
//! writing new frames: `OZEN_GOLDEN=write cargo nextest run golden`.
use super::tests::{Mac, folder};
use super::*;
use serde_json::json;
use std::path::PathBuf;

/// One message of each kind, as protocol v1 sends it.
fn messages() -> [(&'static str, Msg); 4] {
    [
        (
            "buckets",
            Msg::Buckets {
                h: base64::engine::general_purpose::STANDARD.encode([0xab; 16 * 2]),
                max: 1,
            },
        ),
        (
            "summary",
            Msg::Summary {
                id: 1790520395366000000,
                part: 0,
                parts: 2,
                s: vec![("tags".into(), "x".into(), 3, "0123456789abcdef".into())],
                b: vec![4, 200],
            },
        ),
        (
            "records",
            Msg::Records {
                r: vec![
                    ("tags".into(), "x".into(), json!({"v": 1, "val": "Dana"})),
                    (
                        "lines".into(),
                        "1@a".into(),
                        json!({"id": "1@a", "v": 2, "t": 1.0, "text": "hi"}),
                    ),
                ],
            },
        ),
        (
            "part",
            Msg::Part(parts::Part {
                k: "lines".into(),
                key: "1@a".into(),
                v: 2,
                h: "0123456789abcdef".into(),
                i: 0,
                n: 2,
                d: base64::engine::general_purpose::STANDARD.encode(br#"{"id":"1@a","#),
            }),
        ),
    ]
}

fn golden(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/fixtures/sync-golden/{name}.bin"))
}

/// The checked-in v1 frame `name`, written from `msg` first when OZEN_GOLDEN=write.
fn frame(s: &Session, name: &str, msg: &Msg) -> Vec<u8> {
    let p = golden(name);
    if std::env::var("OZEN_GOLDEN").as_deref() == Ok("write") {
        // Rewriting in CI would make these tests compare today's code with itself.
        assert!(std::env::var_os("CI").is_none(), "OZEN_GOLDEN=write in CI");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, s.frame(msg).unwrap()).unwrap();
    }
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

#[test]
fn golden_these_fixtures_are_protocol_v1_which_this_build_still_reads() {
    // v2 (OFE-56) changed which line fields a records message carries, not the messages: these v1
    // frames are what a Mac on the previous ozen sends, and this build must keep opening them.
    // A VERSION past 2 drops v1 (a build reads its own and the one before): write new frames then.
    assert_eq!(VERSION, 2);
    assert_eq!(
        Versions::default().read(1),
        super::super::version::Read::Yes
    );
}

#[test]
fn golden_message_json_is_unchanged() {
    for (name, msg) in messages() {
        insta::assert_snapshot!(name, serde_json::to_string(&msg).unwrap());
    }
}

#[test]
fn golden_v1_frames_still_open_and_parse_to_the_same_json() {
    let s = Session::with([7; 32], "vault", Coalesced::new(|| {}));
    for (name, msg) in messages() {
        let f = frame(&s, name, &msg);
        let got = s
            .decode(&f)
            .unwrap_or_else(|e| panic!("{name}: {e}"))
            .unwrap_or_else(|| panic!("{name}: read as a newer version"));
        assert_eq!(
            serde_json::to_string(&got).unwrap(),
            serde_json::to_string(&msg).unwrap(),
            "{name}"
        );
    }
}

#[test]
fn golden_v1_records_frame_still_merges() {
    let d = folder(json!([]), json!({}));
    let mut b = Mac::new(d.path());
    let [_, _, (name, msg), _] = messages();
    let f = frame(&b.s, name, &msg);
    assert!(b.receive(&f).is_empty());
    assert_eq!(b.s.dropped, Dropped::default());
    let got = b.synced();
    assert_eq!(
        got[&("tags".into(), "x".into())],
        json!({"v": 1, "val": "Dana"})
    );
    assert_eq!(got[&("lines".into(), "1@a".into())]["text"], "hi");
}
