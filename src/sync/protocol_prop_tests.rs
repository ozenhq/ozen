//! Whatever order Macs meet in, they end with the same data (lines, tags, fixes, places, vocab), and
//! it's what crdt.rs's rule picks:
//! for each record the highest `v`, ties broken by the larger JSON text (OFE-43).
use super::tests::{Mac, exchange, folder};
use super::*;
use proptest::prelude::*;
use serde_json::json;

const LINES: [&str; 5] = ["l0", "l1", "l2@a", "l3@b", "l4"];
const TAGS: [&str; 4] = ["l0", "l1", "l2@a", "x"];
const FIXES: [&str; 3] = ["l0", "l3@b", "y"];
const PLACES: [&str; 3] = ["p0", "p1@a", "p2@b"];
const WORDS: [&str; 3] = ["Kev", "PR", "Claude"];

/// One Mac's version of a record: (v, which value; None is a delete). `v` is drawn from a small range
/// so different Macs often hold the same version with different values.
type Edit = Option<(u64, Option<u8>)>;

fn edits(n: usize) -> impl Strategy<Value = Vec<Edit>> {
    prop::collection::vec(prop::option::of((1u64..4, prop::option::of(0u8..3))), n)
}

/// A Mac's edits to every line, tag, fix, place and vocab key (None: it never had that record).
type Edits = (Vec<Edit>, Vec<Edit>, Vec<Edit>, Vec<Edit>, Vec<Edit>);

fn mac() -> impl Strategy<Value = Edits> {
    (
        edits(LINES.len()),
        edits(TAGS.len()),
        edits(FIXES.len()),
        edits(PLACES.len()),
        edits(WORDS.len()),
    )
}

fn place(id: &str, v: u64, x: Option<u8>) -> Value {
    match x {
        Some(x) => json!({"id": id, "v": v, "label": format!("place {x}"), "action": "record"}),
        None => json!({"id": id, "v": v, "del": true}),
    }
}

fn line(id: &str, v: u64, x: Option<u8>) -> Value {
    match x {
        Some(x) => json!({"id": id, "v": v, "t": 1.0, "text": format!("said {x}")}),
        None => json!({"id": id, "v": v, "del": true}),
    }
}

fn entry(v: u64, x: Option<u8>) -> Value {
    match x {
        Some(x) => json!({"v": v, "val": format!("value {x}")}),
        None => json!({"v": v}),
    }
}

/// A vocab entry: a word is there (true) or deleted.
fn word(v: u64, live: bool) -> Value {
    if live {
        json!({"v": v, "val": true})
    } else {
        json!({"v": v})
    }
}

/// A Mac's folder, and every record it starts with as (kind, key) -> record.
fn sandbox((l, t, f, p, w): &Edits) -> (tempfile::TempDir, BTreeMap<Id, Value>) {
    let mut has = BTreeMap::new();
    let lines: Vec<Value> = LINES
        .iter()
        .zip(l)
        .filter_map(|(id, e)| e.map(|(v, x)| line(id, v, x)))
        .collect();
    for r in &lines {
        has.insert(
            ("lines".into(), r["id"].as_str().unwrap().into()),
            r.clone(),
        );
    }
    let map = |keys: &[&str], es: &[Edit], kind: &str, has: &mut BTreeMap<Id, Value>| {
        let mut m = serde_json::Map::new();
        for (k, e) in keys.iter().zip(es) {
            if let Some((v, x)) = e {
                // a vocab entry's value is always true (valid.rs rejects anything else)
                let r = if kind == "vocab" {
                    word(*v, x.is_some())
                } else {
                    entry(*v, *x)
                };
                m.insert(k.to_string(), r.clone());
                has.insert((kind.into(), k.to_string()), r);
            }
        }
        Value::Object(m)
    };
    let tags = map(&TAGS, t, "tags", &mut has);
    let fixes = map(&FIXES, f, "fixes", &mut has);
    let vocab = map(&WORDS, w, "vocab", &mut has);
    let places: Vec<Value> = PLACES
        .iter()
        .zip(p)
        .filter_map(|(id, e)| e.map(|(v, x)| place(id, v, x)))
        .collect();
    for r in &places {
        has.insert(
            ("places".into(), r["id"].as_str().unwrap().into()),
            r.clone(),
        );
    }
    let d = folder(Value::Array(lines), tags);
    std::fs::write(d.path().join(merge::FIXES), fixes.to_string()).unwrap();
    std::fs::write(d.path().join(crate::mcp::VOCAB), vocab.to_string()).unwrap();
    std::fs::write(
        d.path().join(crate::places::FILE),
        Value::Array(places).to_string(),
    )
    .unwrap();
    (d, has)
}

/// crdt.rs's rule, computed here independently: per record, the highest `v`, then the larger JSON.
fn expected(all: &[BTreeMap<Id, Value>]) -> BTreeMap<Id, Value> {
    let mut out: BTreeMap<Id, Value> = BTreeMap::new();
    for (id, r) in all.iter().flatten() {
        let key = |r: &Value| (v(r), crate::crdt::canonical(r));
        if out.get(id).is_none_or(|have| key(r) > key(have)) {
            out.insert(id.clone(), r.clone());
        }
    }
    out
}

proptest! {
    // 64 cases a run keeps CI fast; PROPTEST_CASES=256 for a deeper local run.
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn any_order_of_meetings_converges_to_the_crdt_winner(
        macs in prop::collection::vec(mac(), 2..=4),
        schedule in prop::collection::vec((0usize..4, 0usize..4), 0..8),
    ) {
        let boxes: Vec<_> = macs.iter().map(sandbox).collect();
        let want = expected(&boxes.iter().map(|(_, has)| has.clone()).collect::<Vec<_>>());
        let mut ms: Vec<Mac> = boxes.iter().map(|(d, _)| Mac::new(d.path())).collect();
        let n = ms.len();
        let meet = |ms: &mut Vec<Mac>, a: usize, b: usize| {
            let (lo, hi) = (a.min(b), a.max(b));
            let (left, right) = ms.split_at_mut(hi);
            exchange(&mut left[lo], &mut right[0]);
        };
        // random meetings first, in whatever order they come
        for (a, b) in schedule {
            let (a, b) = (a % n, b % n);
            if a != b {
                meet(&mut ms, a, b);
            }
        }
        // then every pair meets, in rounds; one round already spreads everything (Mac 0 ends with the
        // union, and its later meetings pass it on), the extra rounds only make that obvious
        for _ in 1..n {
            for a in 0..n {
                for b in a + 1..n {
                    meet(&mut ms, a, b);
                }
            }
        }
        let first = ms[0].synced();
        for m in &ms[1..] {
            prop_assert_eq!(&m.synced(), &first);
        }
        prop_assert_eq!(first, want);
    }
}

/// The synced files in `dir`, byte for byte.
fn synced_files(dir: &std::path::Path) -> Vec<(String, Option<Vec<u8>>)> {
    [
        merge::LINES,
        merge::TAGS,
        merge::FIXES,
        crate::mcp::VOCAB,
        crate::places::FILE,
    ]
    .iter()
    .map(|f| (f.to_string(), std::fs::read(dir.join(f)).ok()))
    .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    /// `ozen merge DIR` and sync both end in `merge::apply`, but through different readers (a folder vs.
    /// received records): from the same two folders they must leave identical files, or a user who merged
    /// a backup by hand and then synced would see Macs diverge (OFE-87).
    #[test]
    fn ozen_merge_and_sync_leave_the_same_files(a in mac(), b in mac()) {
        let (by_merge, _) = sandbox(&a);
        let (by_sync, _) = sandbox(&a);
        let (theirs, _) = sandbox(&b);
        let (theirs_too, _) = sandbox(&b);
        super::tests::at(by_merge.path(), || merge::merge_files(&theirs.path().to_string_lossy()))
            .unwrap();
        let (mut ma, mut mb) = (Mac::new(by_sync.path()), Mac::new(theirs_too.path()));
        exchange(&mut ma, &mut mb);
        prop_assert_eq!(synced_files(by_merge.path()), synced_files(by_sync.path()));
    }
}
