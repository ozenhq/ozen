//! Five Macs that are never all online at once: data only spreads pair by pair, along a chain, as
//! with a relay that keeps nothing (OFE-63). Afterwards every Mac's synced files are byte-identical.
use super::tests::{Mac, exchange, folder};
use serde_json::json;

const MACS: usize = 5;

/// Mac `i`'s folder: a line, tag, fix and word only it has; its own version of one shared tag at
/// the same `v` as everyone else's (an equal-version conflict); and, on Mac 0, a later delete of
/// the line every Mac starts with.
fn sandbox(i: usize) -> tempfile::TempDir {
    let shared = if i == 0 {
        json!({"id": "s", "v": 5, "del": true})
    } else {
        json!({"id": "s", "v": 4, "t": 0.5, "text": "everyone has this"})
    };
    let mine =
        json!({"id": format!("{i}@m{i}"), "v": 1, "t": i as f64, "text": format!("from Mac {i}")});
    let d = folder(
        json!([shared, mine]),
        json!({format!("who-{i}"): {"v": 1, "val": format!("Person {i}")},
               "conflict": {"v": 3, "val": format!("Mac {i}'s name")}}),
    );
    std::fs::write(
        d.path().join("fixes.json"),
        json!({format!("fix-{i}"): {"v": 1, "val": format!("fixed by {i}")}}).to_string(),
    )
    .unwrap();
    std::fs::write(
        d.path().join("vocab.json"),
        json!({format!("Word{i}"): {"v": 1, "val": true}}).to_string(),
    )
    .unwrap();
    d
}

/// Every synced file (`crdt::SYNCED`), byte for byte, in the table's order.
fn files(dir: &std::path::Path) -> Vec<(&'static str, Option<Vec<u8>>)> {
    crate::crdt::SYNCED
        .map(|(_, f)| (f, std::fs::read(dir.join(f)).ok()))
        .to_vec()
}

#[test]
fn five_macs_meeting_only_in_pairs_along_a_chain_end_byte_identical() {
    let dirs: Vec<_> = (0..MACS).map(sandbox).collect();
    let mut ms: Vec<Mac> = dirs.iter().map(|d| Mac::new(d.path())).collect();
    let meet = |ms: &mut Vec<Mac>, a: usize, b: usize| {
        let (lo, hi) = (a.min(b), a.max(b));
        let (left, right) = ms.split_at_mut(hi);
        exchange(&mut left[lo], &mut right[0]);
    };
    // a few chance meetings, then down the chain and back: never more than two online at once
    let chance = [(1, 3), (4, 2), (0, 4)];
    let down = (0..MACS - 1).map(|i| (i, i + 1));
    let up = (0..MACS - 1).rev().map(|i| (i + 1, i));
    for (a, b) in chance.into_iter().chain(down).chain(up) {
        meet(&mut ms, a, b);
    }
    let first = files(dirs[0].path());
    for (f, bytes) in &first {
        // places.json: no Mac has places here
        assert!(bytes.is_some() || *f == crate::places::FILE, "{f} written");
    }
    for (i, d) in dirs.iter().enumerate().skip(1) {
        assert_eq!(files(d.path()), first, "Mac {i} differs from Mac 0");
    }
    // and the data is everyone's: every Mac's own records, the delete, one winner for the conflict
    let got = ms[0].synced();
    for i in 0..MACS {
        assert!(
            got.contains_key(&("lines".into(), format!("{i}@m{i}"))),
            "Mac {i}'s line"
        );
        assert!(
            got.contains_key(&("tags".into(), format!("who-{i}"))),
            "Mac {i}'s tag"
        );
        assert!(
            got.contains_key(&("fixes".into(), format!("fix-{i}"))),
            "Mac {i}'s fix"
        );
        assert!(
            got.contains_key(&("vocab".into(), format!("Word{i}"))),
            "Mac {i}'s word"
        );
    }
    assert_eq!(
        got[&("lines".into(), "s".into())]["del"],
        true,
        "the delete won (higher v)"
    );
    // equal v: crdt.rs picks the larger canonical JSON, here Mac 4's value
    assert_eq!(
        got[&("tags".into(), "conflict".into())]["val"],
        "Mac 4's name"
    );
    // a further round changes nothing anywhere
    for i in 0..MACS - 1 {
        let (left, right) = ms.split_at_mut(i + 1);
        assert_eq!(
            exchange(&mut left[i], &mut right[0]),
            0,
            "Macs {i} and {} in sync",
            i + 1
        );
    }
}
