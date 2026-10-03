//! What ozen keeps that is yours, in a shape two Macs can merge later: state-based CRDTs, so merging is
//! commutative, associative and idempotent, and every Mac ends up with the same data whatever the order.
//!
//! Synced data, merged by `merge_rows` / `merge_maps`:
//! - lines.jsonl (transcript lines and notes) and places.json: rows keyed by "id".
//! - tags.json, fixes.json, vocab.json: maps, key -> entry.
//!
//! Each record is last-writer-wins. A record may carry "v", a hybrid clock: ms since 1970, bumped past the
//! record's own last "v", so a later edit wins even if this Mac's clock went back. Equal "v"s fall back to
//! the larger JSON text, so every Mac picks the same winner. Deletes leave a tombstone ({"id", "v", "del":
//! true} for a row, {"v"} without "val" for a map entry) so a merge can't bring the record back.
//!
//! Backward compatible: a record without "v" is from before versions and counts as 0, a map value that is
//! not an entry object is a plain value (tags.json and fixes.json were id -> string), and a place without an
//! "id" gets one from its position and label. Files written before this load unchanged; writes rewrite
//! only the records they change. New line, note and place ids end in "@<device>", so Macs never mint the
//! same id.
//!
//! Local, never merged (each Mac rebuilds them): labels.json, stats.json, ignore.json and voices/ (retrain,
//! from tags; voices/ has its own git sync), learned.json (`ozen fix` relearns from fixes), junk.json,
//! transcript.txt, context/, pace.jsonl, here.json*, chunk audio (chunks/, recent/, fixes/), start.log,
//! the record mode and switches (app defaults, dot files). After a merge, relearn and retrain.
//!
//! No crate imports: the menu bar app includes this file too (src/bin/bar/main.rs).
use serde_json::{Map, Value, json};
use std::fs;
use std::sync::OnceLock;

pub type Row = Map<String, Value>;

/// This Mac: 8 hex digits of its hardware UUID (stable, and not copied along with the ozen folder).
pub fn device() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        unsafe extern "C" {
            fn gethostuuid(id: *mut u8, wait: *const [i64; 2]) -> i32;
        }
        let mut id = [0u8; 16];
        // SAFETY: a 16-byte uuid_t and a zero timespec (don't wait), as the man page asks.
        if unsafe { gethostuuid(id.as_mut_ptr(), &[0, 0]) } != 0 {
            return "local".into();
        }
        id[..4].iter().map(|b| format!("{b:02x}")).collect()
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn v(r: &Value) -> u64 {
    r.get("v").and_then(Value::as_u64).unwrap_or(0)
}

/// The "v" for a new version of `old` (None: a new record).
pub fn stamp(old: Option<&Value>) -> u64 {
    now_ms().max(old.map_or(0, v) + 1)
}

/// A fresh id for a record this Mac creates: `<base>@<device>`.
pub fn mint(base: &str) -> String {
    format!("{base}@{}", device())
}

/// Whether record `a` wins over `b` (two versions of one key).
fn wins(a: &Value, b: &Value) -> bool {
    (v(a), a.to_string()) > (v(b), b.to_string())
}

// ---- maps: tags.json, fixes.json, vocab.json ----

/// An entry's value: None for a tombstone. A non-object is a value written before entries existed.
fn value(e: &Value) -> Option<&Value> {
    match e {
        Value::Object(o) => o.get("val"),
        x => Some(x),
    }
}

/// The map's values, without tombstones: what readers want.
pub fn live_map(raw: &Row) -> Row {
    raw.iter()
        .filter_map(|(k, e)| Some((k.clone(), value(e)?.clone())))
        .collect()
}

fn read_raw(path: &str) -> Row {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The live values of the map file at `path` (missing or unreadable: empty).
pub fn read_map(path: &str) -> Row {
    live_map(&read_raw(path))
}

/// The raw map after setting its live values to `live`: changed keys get a new version, dropped ones a tombstone.
pub fn edit_map(mut raw: Row, live: &Row) -> Row {
    let old = live_map(&raw);
    for (k, x) in live.iter().filter(|(k, x)| old.get(*k) != Some(*x)) {
        let v = stamp(raw.get(k));
        raw.insert(k.clone(), json!({"v": v, "val": x}));
    }
    for k in old.keys().filter(|k| !live.contains_key(*k)) {
        let v = stamp(raw.get(k));
        raw.insert(k.clone(), json!({"v": v}));
    }
    raw
}

/// Save `live` as the map file at `path` (see `edit_map`).
pub fn write_map(path: &str, live: &Row) -> Result<(), String> {
    let raw = edit_map(read_raw(path), live);
    let tmp = format!("{path}.tmp");
    fs::write(
        &tmp,
        serde_json::to_string_pretty(&raw).expect("json") + "\n",
    )
    .and_then(|_| fs::rename(&tmp, path))
    .map_err(|e| format!("write {path}: {e}"))
}

pub fn merge_maps(a: &Row, b: &Row) -> Row {
    let mut out = a.clone();
    for (k, y) in b {
        if out.get(k).is_none_or(|x| wins(y, x)) {
            out.insert(k.clone(), y.clone());
        }
    }
    out
}

// ---- rows: lines.jsonl, places.json ----

pub fn is_live(r: &Row) -> bool {
    r.get("del") != Some(&Value::Bool(true))
}

/// What's left of a deleted row: enough to win the merge, nothing a reader would show.
pub fn tombstone(r: &Row) -> Row {
    let v = stamp(Some(&Value::Object(r.clone())));
    let mut t = Row::new();
    t.insert("id".into(), r.get("id").cloned().unwrap_or(Value::Null));
    t.insert("v".into(), json!(v));
    t.insert("del".into(), json!(true));
    t
}

/// `new`, a changed version of row `old`, with its version set.
pub fn bump(old: &Row, mut new: Row) -> Row {
    new.insert("v".into(), json!(stamp(Some(&Value::Object(old.clone())))));
    new
}

fn id(r: &Row) -> &str {
    r.get("id").and_then(Value::as_str).unwrap_or("")
}

/// Live rows, each with an id: one written before ids gets `0<position>-<label>` (zero-padded so that
/// file order sorts first, ahead of minted ids; the label so two Macs' copies of one old place match).
pub fn live_rows(raw: &[Row]) -> Vec<Row> {
    raw.iter()
        .enumerate()
        .filter(|(_, r)| is_live(r))
        .map(|(i, r)| {
            let mut r = r.clone();
            if !r.contains_key("id") {
                let label = r.get("label").and_then(Value::as_str).unwrap_or("");
                r.insert("id".into(), json!(format!("0{i:03}-{label}")));
            }
            r
        })
        .collect()
}

fn without_v(r: &Row) -> Row {
    let mut r = r.clone();
    r.remove("v");
    r
}

/// The raw rows after setting the live ones to `live` (in that order, which is kept): rows without an id
/// get a minted one, changed rows a new version, dropped rows a tombstone (kept at the end).
pub fn edit_rows(raw: &[Row], live: &[Row]) -> Vec<Row> {
    let old = live_rows(raw);
    let mut out: Vec<Row> = live
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let mut r = r.clone();
            if !r.contains_key("id") {
                r.insert("id".into(), json!(mint(&format!("{}-{i}", now_ms()))));
            }
            match old.iter().find(|o| id(o) == id(&r)) {
                Some(o) if without_v(o) == without_v(&r) => o.clone(),
                Some(o) => bump(o, r),
                None => {
                    r.insert("v".into(), json!(stamp(None)));
                    r
                }
            }
        })
        .collect();
    let gone: Vec<Row> = old
        .iter()
        .filter(|o| !out.iter().any(|r| id(r) == id(o)))
        .map(tombstone)
        .collect();
    let kept: Vec<Row> = raw
        .iter()
        .filter(|t| !is_live(t) && !out.iter().any(|r| id(r) == id(t)))
        .cloned()
        .collect();
    out.extend(gone);
    out.extend(kept);
    out
}

/// Union by id, the winning version of each, sorted by id (ids start with their creation time).
pub fn merge_rows(a: &[Row], b: &[Row]) -> Vec<Row> {
    let mut by: std::collections::BTreeMap<String, Row> = Default::default();
    for r in live_ids(a).into_iter().chain(live_ids(b)) {
        let k = id(&r).to_string();
        if by
            .get(&k)
            .is_none_or(|x| wins(&Value::Object(r.clone()), &Value::Object(x.clone())))
        {
            by.insert(k, r);
        }
    }
    by.into_values().collect()
}

/// Every row with its id (live_rows' for the ones written before ids), tombstones included.
fn live_ids(raw: &[Row]) -> Vec<Row> {
    let named = live_rows(raw);
    let mut named = named.into_iter();
    raw.iter()
        .map(|r| {
            if is_live(r) {
                named.next().unwrap()
            } else {
                r.clone()
            }
        })
        .collect()
}

/// JSONL text -> rows (lines that don't parse are skipped).
pub fn parse_jsonl(text: &str) -> Vec<Row> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn row(v: Value) -> Row {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn old_files_load_as_they_are() {
        let tags = row(json!({"a": "Dana", "b": {"v": 5, "val": "Noa"}, "c": {"v": 6}}));
        assert_eq!(live_map(&tags), row(json!({"a": "Dana", "b": "Noa"})));
        let places = [
            row(json!({"label": "Home", "action": "off"})),
            row(json!({"label": "Work", "action": "record"})),
        ];
        let live = live_rows(&places);
        assert_eq!(live[0]["id"], "0000-Home");
        assert_eq!(live[1]["id"], "0001-Work");
        // saving unchanged rows changes nothing but adding the ids
        assert_eq!(live_rows(&edit_rows(&places, &live)), live);
    }

    #[test]
    fn editing_a_map_versions_only_what_changed() {
        let raw = row(json!({"a": "Dana", "b": "Noa"}));
        let out = edit_map(raw, &row(json!({"a": "Dana", "c": "Tal"})));
        assert_eq!(out["a"], "Dana");
        assert!(out["b"]["v"].as_u64().unwrap() > 0 && out["b"].get("val").is_none());
        assert_eq!(out["c"]["val"], "Tal");
        assert_eq!(live_map(&out), row(json!({"a": "Dana", "c": "Tal"})));
    }

    #[test]
    fn a_later_edit_wins_and_a_delete_stays_deleted() {
        let base = row(json!({"a": "Dana"}));
        let mine = edit_map(base.clone(), &row(json!({"a": "Noa"})));
        let theirs = edit_map(base.clone(), &Row::new());
        // both newer than the old value, either order gives the same result
        assert_eq!(merge_maps(&mine, &base), mine);
        assert_eq!(merge_maps(&base, &theirs), theirs);
        assert_eq!(merge_maps(&mine, &theirs), merge_maps(&theirs, &mine));

        let lines = [row(json!({"id": "1-mic-0", "t": 1.0, "text": "hi"}))];
        let deleted = edit_rows(&lines, &[]);
        assert_eq!(merge_rows(&lines, &deleted), merge_rows(&deleted, &lines));
        assert!(live_rows(&merge_rows(&lines, &deleted)).is_empty());
    }

    #[test]
    fn ids_minted_here_name_this_mac() {
        assert_eq!(device().len(), 8);
        assert!(mint("1-mic-0").starts_with("1-mic-0@"));
    }

    #[test]
    fn a_version_is_past_the_last_one_even_if_the_clock_went_back() {
        let future = json!({"v": u64::MAX / 2});
        assert_eq!(stamp(Some(&future)), u64::MAX / 2 + 1);
    }

    fn entry() -> impl Strategy<Value = Value> {
        prop_oneof![
            "[ab]".prop_map(Value::from),
            (0u64..4, "[ab]").prop_map(|(v, s)| json!({"v": v, "val": s})),
            (0u64..4).prop_map(|v| json!({"v": v})),
        ]
    }

    fn map() -> impl Strategy<Value = Row> {
        prop::collection::btree_map("[xyz]", entry(), 0..4).prop_map(|m| m.into_iter().collect())
    }

    fn rows() -> impl Strategy<Value = Vec<Row>> {
        prop::collection::vec(
            ("[xyz]", 0u64..4, any::<bool>(), "[ab]").prop_map(|(id, v, del, text)| {
                row(if del {
                    json!({"id": id, "v": v, "del": true})
                } else {
                    json!({"id": id, "v": v, "text": text})
                })
            }),
            0..4,
        )
        .prop_map(|rs| merge_rows(&rs, &[])) // one row per id, as a file has
    }

    proptest! {
        #[test]
        fn maps_merge_as_a_crdt(a in map(), b in map(), c in map()) {
            prop_assert_eq!(merge_maps(&a, &b), merge_maps(&b, &a));
            prop_assert_eq!(merge_maps(&merge_maps(&a, &b), &c), merge_maps(&a, &merge_maps(&b, &c)));
            prop_assert_eq!(merge_maps(&a, &a), a);
        }

        #[test]
        fn rows_merge_as_a_crdt(a in rows(), b in rows(), c in rows()) {
            prop_assert_eq!(merge_rows(&a, &b), merge_rows(&b, &a));
            prop_assert_eq!(merge_rows(&merge_rows(&a, &b), &c), merge_rows(&a, &merge_rows(&b, &c)));
            prop_assert_eq!(merge_rows(&a, &a), a);
        }
    }
}
