//! `merge::apply` must not care how received records arrive (OFE-53): every Mac's files are the only
//! copies, so if applying depended on the order records come in, on how they're split into batches, or
//! on a record arriving twice, Macs would drift apart for good.
use super::*;
use crate::crdt::merge_maps;
use proptest::prelude::*;
use serde_json::json;
use std::path::Path;

/// (kind, key, version, value: None is a delete)
type Rec = (usize, usize, u64, Option<u8>);
const KINDS: [&str; 5] = ["lines", "tags", "fixes", "places", "vocab"];

fn record((kind, key, v, x): &Rec) -> (String, Value) {
    let k = format!("k{key}");
    let r = match (KINDS[*kind], x) {
        ("lines", Some(x)) => json!({"id": k, "v": v, "t": 1.0, "text": format!("said {x}")}),
        ("places", Some(x)) => {
            json!({"id": k, "v": v, "label": format!("place {x}"), "action": "record"})
        }
        ("lines" | "places", None) => json!({"id": k, "v": v, "del": true}),
        ("vocab", Some(_)) => json!({"v": v, "val": true}),
        (_, Some(x)) => json!({"v": v, "val": format!("value {x}")}),
        (_, None) => json!({"v": v}),
    };
    (k, r)
}

/// One received batch, as sync builds it. Two versions of one map key in a batch are combined by the
/// CRDT rule, as a sender's own files would hold them.
fn batch(recs: &[Rec]) -> Synced {
    let mut s = Synced::default();
    for rec in recs {
        let (k, r) = record(rec);
        let one = |m: &Row| merge_maps(m, &[(k.clone(), r.clone())].into_iter().collect());
        match KINDS[rec.0] {
            "lines" => s.lines.push(r.as_object().unwrap().clone()),
            "places" => s.places.push(r.as_object().unwrap().clone()),
            "tags" => s.tags = one(&s.tags),
            "fixes" => s.fixes = one(&s.fixes),
            _ => s.vocab = one(&s.vocab),
        }
    }
    s
}

/// A folder this Mac already has: `recs` written straight to its files, plus legacy forms (a bare tag
/// value, a line without a version) as older ozen wrote them.
fn base(recs: &[Rec]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let s = batch(recs);
    let mut tags = s.tags.clone();
    tags.insert("k9".into(), json!("Legacy"));
    let mut lines: Vec<Value> = s.lines.into_iter().map(Value::Object).collect();
    lines.push(json!({"id": "k9", "t": 2.0, "text": "from before versions"}));
    let rows: String = lines.iter().map(|r| r.to_string() + "\n").collect();
    fs::write(d.path().join(LINES), rows).unwrap();
    fs::write(d.path().join(TAGS), Value::Object(tags).to_string()).unwrap();
    fs::write(d.path().join(FIXES), Value::Object(s.fixes).to_string()).unwrap();
    fs::write(
        d.path().join(crate::mcp::VOCAB),
        Value::Object(s.vocab).to_string(),
    )
    .unwrap();
    let places: Vec<Value> = s.places.into_iter().map(Value::Object).collect();
    fs::write(
        d.path().join(places::FILE),
        Value::Array(places).to_string(),
    )
    .unwrap();
    d
}

/// Applies `batches` in turn to a copy of the folder `mine`; returns its synced files, byte for byte.
fn applied(mine: &[Rec], batches: &[Vec<Rec>]) -> Vec<(String, Option<Vec<u8>>)> {
    let d = base(mine);
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    for b in batches {
        apply(&batch(b)).unwrap();
    }
    std::env::set_current_dir(back).unwrap();
    files(d.path())
}

fn files(d: &Path) -> Vec<(String, Option<Vec<u8>>)> {
    [LINES, TAGS, FIXES, crate::mcp::VOCAB, places::FILE]
        .iter()
        .map(|f| (f.to_string(), fs::read(d.join(f)).ok()))
        .collect()
}

/// `recs` reordered, some repeated, and cut into batches at `cuts`.
fn rearranged(recs: &[Rec], order: &[usize], again: &[usize], cuts: &[usize]) -> Vec<Vec<Rec>> {
    let mut all: Vec<Rec> = order.iter().map(|&i| recs[i % recs.len()]).collect();
    all.extend(again.iter().map(|&i| recs[i % recs.len()]));
    let mut out = vec![vec![]];
    for (i, r) in all.into_iter().enumerate() {
        if cuts.contains(&i) {
            out.push(vec![]);
        }
        out.last_mut().unwrap().push(r);
    }
    out
}

/// Records with few kinds, keys and versions, so equal-version conflicts and deletes collide often;
/// with the same records in a random order.
fn recs_and_shuffled() -> impl Strategy<Value = (Vec<Rec>, Vec<Rec>)> {
    prop::collection::vec(
        (0usize..5, 0usize..4, 1u64..4, prop::option::of(0u8..3)),
        1..24,
    )
    .prop_flat_map(|r| (Just(r.clone()), Just(r).prop_shuffle()))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn any_order_batching_or_repeat_of_records_leaves_the_same_files(
        (recs, shuffled) in recs_and_shuffled(),
        mine in prop::collection::vec((0usize..5, 0usize..4, 1u64..4, prop::option::of(0u8..3)), 0..12),
        again in prop::collection::vec(0usize..24, 0..8),
        cuts in prop::collection::vec(0usize..32, 0..6),
    ) {
        let once = applied(&mine, std::slice::from_ref(&recs));
        let order: Vec<usize> = (0..shuffled.len()).collect();
        prop_assert_eq!(&applied(&mine, &rearranged(&shuffled, &order, &again, &cuts)), &once);
        // and applying everything again changes nothing
        prop_assert_eq!(applied(&mine, &[recs.clone(), recs]), once);
    }
}
