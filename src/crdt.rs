//! What ozen keeps that is yours, in a shape two Macs can merge later: state-based CRDTs, so merging is
//! commutative, associative and idempotent, and every Mac ends up with the same data whatever the order.
//!
//! Synced data (`SYNCED`), merged by `merge_rows` / `merge_maps`:
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
//! restore points (.sync-restore/), the record mode and switches (app defaults, dot files). After a merge, relearn and retrain.
//!
//! Merging is the `crdts` crate's last-writer-wins register. No `crate::` imports: the menu bar app includes
//! this file too (src/bin/bar/main.rs).
/// What syncs: each kind as sync messages name it, and its file. The one list (OFE-59): merge.rs reads and
/// writes these files (`merge::Synced`), sync/protocol.rs sends and merges these kinds, sync/valid.rs
/// checks them, and a test (sync/protocol_synced_tests.rs) fails if any of them misses one. A kind
/// missing anywhere would leave Macs different for good, with no server copy to repair them.
pub const SYNCED: [(&str, &str); 5] = [
    ("lines", "lines.jsonl"),
    ("places", "places.json"),
    ("tags", "tags.json"),
    ("fixes", "fixes.json"),
    ("vocab", "vocab.json"),
];

use crdts::{CvRDT, LWWReg};
use serde_json::{Map, Value, json};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::OnceLock;

pub type Row = Map<String, Value>;

/// This Mac: 8 hex digits of its hardware UUID (stable, and not copied along with the ozen folder).
pub fn device() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        machine_uid::get().map_or("local".into(), |id| {
            id.chars()
                .filter(char::is_ascii_hexdigit)
                .take(8)
                .collect::<String>()
                .to_lowercase()
        })
    })
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A record's version: its "v", 0 for one written before versions.
pub fn v(r: &Value) -> u64 {
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

/// `r` with every object's keys in sorted order, whatever map type serde_json was built with: with its
/// `preserve_order` feature (which any dependency could switch on through Cargo feature unification)
/// maps keep insertion order, so two Macs would write and compare the same record as different text
/// (OFE-70). A no-op with the default, sorted map.
pub fn sorted(r: &Value) -> Value {
    match r {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|k| (k.clone(), sorted(&m[k])))
                    .collect(),
            )
        }
        Value::Array(a) => Value::Array(a.iter().map(sorted).collect()),
        x => x.clone(),
    }
}

/// Line fields the transcribing Mac derives for itself (OFE-56): its first guess at the speaker from
/// its own voiceprints (`spk`), how unsure that guess is (`doubt`) and its transcriber run (`run`).
/// Another Mac guesses for itself when it retrains (labels.json) and numbers its own runs, so sync
/// (protocol v2 on) neither sends nor hashes them. A line with no voiceprint keeps `spk`: a note's
/// speaker is what the user typed, and no retrain could recompute it.
pub const LOCAL_LINE_FIELDS: [&str; 3] = ["spk", "doubt", "run"];

/// Line `r` as sync carries it: without `LOCAL_LINE_FIELDS`.
pub fn synced_line(mut r: Row) -> Row {
    let voiced = r.get("e").is_some_and(|e| !e.is_null());
    for f in LOCAL_LINE_FIELDS {
        if f != "spk" || voiced {
            r.remove(f);
        }
    }
    r
}

/// `r` as compact JSON with keys in sorted order: tie-breaks and sync hashes are taken over this
/// text, so it is the same on every Mac. Equal to `r.to_string()` with the default map, so existing
/// markers and hashes don't change.
pub fn canonical(r: &Value) -> String {
    sorted(r).to_string()
}

/// The winner of two versions of one record. The register's marker is ("v", the record's JSON): the JSON
/// breaks ties between equal versions (old records are all 0) the same way on every Mac, and makes a marker
/// name exactly one value, as the register requires.
fn merged(a: &Value, b: &Value) -> Value {
    let reg = |r: &Value| LWWReg {
        val: r.clone(),
        marker: (v(r), canonical(r)),
    };
    let mut x = reg(a);
    x.merge(reg(b));
    x.val
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
    write_atomic(
        path,
        (serde_json::to_string_pretty(&sorted(&Value::from(raw))).expect("json") + "\n").as_bytes(),
    )
}

/// Replace the file at `path` so that a crash at any moment leaves the old file or the new one, never a
/// truncated one: these files are the only copy of the user's data (the sync relay keeps none). Writes a
/// temp file beside it, fsyncs it (F_FULLFSYNC on macOS), then renames it over. A killed writer can leave
/// its temp file (`.<name>.<random>.tmp`) behind, never a half-written `path`; the next write of the same
/// file removes such leftovers once they're a minute old (a younger one may be another writer's, mid-write).
pub fn write_atomic(path: &str, bytes: &[u8]) -> Result<(), String> {
    write_atomic_then(path, bytes, |_| Ok(Some(()))).map(|_| ())
}

/// `write_atomic` in two steps, for a big file others append to under a lock: `bytes` are written and
/// synced first, then `last` gets the temp file (to take the lock and add what was appended meanwhile,
/// synced here again). The temp file replaces `path` only when `last` returns Some, and that value is
/// returned after the rename, so a lock it holds covers the swap; None leaves `path` as it was.
pub fn write_atomic_then<T>(
    path: &str,
    bytes: &[u8],
    last: impl FnOnce(&mut fs::File) -> Result<Option<T>, String>,
) -> Result<Option<T>, String> {
    let err = |e: std::io::Error| format!("write {path}: {e}");
    let p = Path::new(path);
    let dir = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // keep the file's mode (tempfile makes 0600); a new file gets the usual 0644
    let mode =
        fs::metadata(p).map_or_else(|_| fs::Permissions::from_mode(0o644), |m| m.permissions());
    let name = p
        .file_name()
        .map_or(String::new(), |n| n.to_string_lossy().into());
    let prefix = format!(".{name}.");
    remove_stale_temps(dir, &prefix);
    let mut f = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".tmp")
        .permissions(mode)
        .tempfile_in(dir)
        .map_err(err)?;
    f.write_all(bytes).map_err(err)?;
    f.as_file().sync_all().map_err(err)?;
    let len = f.as_file().metadata().map_err(err)?.len();
    let Some(held) = last(f.as_file_mut())? else {
        return Ok(None); // dropping `f` removes the temp file
    };
    if f.as_file().metadata().map_err(err)?.len() != len {
        f.as_file().sync_all().map_err(err)?; // what `last` added
    }
    f.persist(p).map_err(|e| err(e.error))?;
    fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(err)?; // the rename itself, durable too
    Ok(Some(held))
}

/// Removes `write_atomic` temp files (`<prefix><random>.tmp`) a killed writer left in `dir` over a minute ago.
fn remove_stale_temps(dir: &Path, prefix: &str) {
    let stale = |e: &fs::DirEntry| {
        e.metadata()
            .and_then(|m| m.modified())
            .is_ok_and(|t| t.elapsed().is_ok_and(|a| a.as_secs() >= 60))
    };
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with(prefix) && n.ends_with(".tmp") && stale(&e) {
            let _ = fs::remove_file(e.path());
        }
    }
}

pub fn merge_maps(a: &Row, b: &Row) -> Row {
    let mut out = a.clone();
    for (k, y) in b {
        let m = out.get(k).map_or(y.clone(), |x| merged(x, y));
        out.insert(k.clone(), m);
    }
    out
}

// ---- rows: lines.jsonl, places.json ----

pub fn is_live(r: &Row) -> bool {
    r.get("del") != Some(&Value::Bool(true))
}

/// What's left of a deleted row: enough to win the merge, nothing a reader would show. A place keeps its
/// label and action: builds before this parse places.json only when every row has them (a deleted place
/// shows up again there, but the file isn't lost). A line keeps no text, voiceprint or time.
pub fn tombstone(r: &Row) -> Row {
    let v = stamp(Some(&Value::Object(r.clone())));
    let mut t: Row = ["label", "action"]
        .iter()
        .filter_map(|k| Some((k.to_string(), r.get(*k)?.clone())))
        .collect();
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
    let mut by: std::collections::BTreeMap<String, Value> = Default::default();
    for r in live_ids(a).into_iter().chain(live_ids(b)) {
        let (k, r) = (id(&r).to_string(), Value::Object(r));
        let m = by.get(&k).map_or(r.clone(), |x| merged(x, &r));
        by.insert(k, m);
    }
    by.into_values()
        .filter_map(|r| r.as_object().cloned())
        .collect()
}

/// Every row with its id (live_rows' for the ones written before ids), tombstones included.
pub fn live_ids(raw: &[Row]) -> Vec<Row> {
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
#[path = "crdt_tests.rs"]
mod tests;
