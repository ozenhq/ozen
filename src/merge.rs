//! `ozen merge DIR`: what a sync will do, by hand. src/crdt.rs has the merge rules and the list of what's synced.
use crate::crdt::{Row, merge_maps, merge_rows, parse_jsonl, write_atomic, write_atomic_then};
use crate::mcp::{VOCAB, VOCAB_TXT, locked};
use crate::places;
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;

pub const TAGS: &str = "tags.json";
pub const FIXES: &str = "fixes.json";
pub const LINES: &str = "lines.jsonl";

/// `ozen merge DIR`: merge another ozen folder's synced data into this one (src/crdt.rs lists what's
/// synced), then relearn and retrain from the result like after any fix or tag.
pub fn merge_from(dir: &str) -> Result<String, String> {
    let out = merge_files(dir)?;
    crate::fixes::relearn()?;
    crate::retrain();
    Ok(out)
}

/// Writes `bytes` to `path` unless the file already holds exactly that.
fn write_new(path: &str, bytes: &[u8]) -> Result<(), String> {
    match fs::read(path) {
        Ok(old) if old == bytes => Ok(()),
        _ => write_atomic(path, bytes),
    }
}

/// Writes `v` with sorted keys (`crdt::sorted`), so every Mac writes the same data as the same text.
/// Nothing to save is no reason to create a file: each write is a full flush, under the transcriber's lock.
fn save(path: &str, v: &impl serde::Serialize) -> Result<(), String> {
    let v = crate::crdt::sorted(&serde_json::to_value(v).map_err(|e| e.to_string())?);
    let empty = v.as_object().is_some_and(serde_json::Map::is_empty)
        || v.as_array().is_some_and(Vec::is_empty);
    if empty && !std::path::Path::new(path).exists() {
        return Ok(());
    }
    let json = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())? + "\n";
    write_new(path, json.as_bytes())
}

/// One folder's synced data (src/crdt.rs lists it), raw: tombstones and versions included.
#[derive(Default)]
pub struct Synced {
    pub tags: Row,
    pub fixes: Row,
    pub vocab: Row,
    pub places: Vec<Row>,
    pub lines: Vec<Row>,
}

/// One synced kind's records in a `Synced`.
pub enum Records<'a> {
    Map(&'a Row),
    Rows(&'a [Row]),
}

impl Synced {
    /// The records of `kind` (one of `crdt::SYNCED`); None for a kind this build doesn't know.
    pub fn get(&self, kind: &str) -> Option<Records<'_>> {
        Some(match kind {
            "tags" => Records::Map(&self.tags),
            "fixes" => Records::Map(&self.fixes),
            "vocab" => Records::Map(&self.vocab),
            "places" => Records::Rows(&self.places),
            "lines" => Records::Rows(&self.lines),
            _ => return None,
        })
    }

    /// Adds received record `r` of `kind` under key `k`. Two versions of one map key keep the CRDT
    /// winner, not the last; a kind this build doesn't know (from a newer ozen) is left out.
    pub fn insert(&mut self, kind: &str, k: String, r: Value) {
        let put =
            |m: &mut Row| *m = crate::crdt::merge_maps(m, &[(k, r.clone())].into_iter().collect());
        match (kind, &r) {
            ("tags", _) => put(&mut self.tags),
            ("fixes", _) => put(&mut self.fixes),
            ("vocab", _) => put(&mut self.vocab),
            ("places", Value::Object(o)) => self.places.push(o.clone()),
            ("lines", Value::Object(o)) => self.lines.push(o.clone()),
            _ => {}
        }
    }
}

/// The synced data in folder `d` ("" for this one, otherwise ending in '/'). A folder from before
/// vocab.json has its words in vocab.txt.
pub fn read_synced(d: &str) -> Synced {
    Synced {
        lines: parse_jsonl(&fs::read_to_string(format!("{d}{LINES}")).unwrap_or_default()),
        ..read_small(d)
    }
}

/// `read_synced` without lines.jsonl: the small files, quick to read under the transcriber's lock.
fn read_small(d: &str) -> Synced {
    let map = |f: &str| -> Row {
        serde_json::from_slice(&fs::read(format!("{d}{f}")).unwrap_or_default()).unwrap_or_default()
    };
    let vocab = if fs::metadata(format!("{d}{VOCAB}")).is_ok() {
        map(VOCAB)
    } else {
        let words = fs::read_to_string(format!("{d}{VOCAB_TXT}")).unwrap_or_default();
        words
            .split([',', '\n'])
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(|w| (w.into(), json!(true)))
            .collect()
    };
    Synced {
        tags: map(TAGS),
        fixes: map(FIXES),
        vocab,
        places: serde_json::from_slice(
            &fs::read(format!("{d}{}", places::FILE)).unwrap_or_default(),
        )
        .unwrap_or_default(),
        lines: vec![],
    }
}

/// The file half of `merge_from`: merges `dir`'s synced files into the ones here (see `apply`).
pub fn merge_files(dir: &str) -> Result<String, String> {
    if !std::path::Path::new(dir).is_dir() {
        return Err(format!("no folder {dir}"));
    }
    let a = apply(&read_synced(&format!("{}/", dir.trim_end_matches('/'))))?;
    Ok(format!("merged: {} lines, {} places", a.lines, a.places))
}

/// What `apply` did.
pub struct Applied {
    /// Some record here is different now: relearn and retrain.
    pub changed: bool,
    pub lines: usize,
    pub places: usize,
}

/// Merges `theirs` into this folder's synced files: what `ozen merge` and sync both do. Every file is
/// replaced crash-safely (`write_atomic`) under the transcriber's lines.jsonl lock, so none of its lines
/// is lost and two merges never interleave. lines.jsonl is MBs, so its merge runs before the lock
/// (`merge_lines`): a whole history arriving never stalls live transcription. A file that would come out
/// the same isn't rewritten.
pub fn apply(theirs: &Synced) -> Result<Applied, String> {
    let lock = locked()?;
    let ours = read_small(""); // lines.jsonl is merged after, mostly without the lock
    let mut changed = false;
    for (f, mine, other) in [
        (TAGS, &ours.tags, &theirs.tags),
        (FIXES, &ours.fixes, &theirs.fixes),
        (VOCAB, &ours.vocab, &theirs.vocab),
    ] {
        let m = merge_maps(mine, other);
        changed |= &m != mine;
        save(f, &m)?;
    }
    let _ = fs::remove_file(VOCAB_TXT); // vocab.json holds its words now
    let p = merge_rows(&ours.places, &theirs.places);
    changed |= p != merge_rows(&ours.places, &[]);
    save(places::FILE, &p)?;
    drop(lock);
    let (lines_changed, lines) = merge_lines(&theirs.lines)?;
    Ok(Applied {
        changed: changed || lines_changed,
        lines,
        places: p.len(),
    })
}

/// lines.jsonl's rows in file order, one canonical JSON per line.
fn jsonl(rows: &[Row]) -> String {
    rows.iter()
        .map(|r| crate::crdt::canonical(&Value::Object(r.clone())) + "\n")
        .collect()
}

/// How many times `merge_lines` redoes its work when the file changed under it before taking the lock.
const TRIES: usize = 3;

/// Merges `theirs` into lines.jsonl; (some line changed, lines now). The slow part (reading, merging and
/// writing MBs) runs without the lock; the lock is taken only to swap the result in. Meanwhile the file
/// can only have grown: the transcriber and `ozen mcp` notes append, and every rewrite swaps in a new
/// file (`write_atomic`, mcp `rewrite`). So under the lock, the same file means just appended lines,
/// copied onto the merged file before the swap. A swapped file, or an appended line that edits a merged
/// one, means doing it again; after `TRIES`, all of it runs under the lock as before.
fn merge_lines(theirs: &[Row]) -> Result<(bool, usize), String> {
    let err = |e: std::io::Error| format!("{LINES}: {e}");
    for _ in 0..TRIES {
        let (ino, text) = match fs::File::open(LINES) {
            Ok(mut f) => {
                let ino = f.metadata().map_err(err)?.ino();
                let mut t = String::new();
                f.read_to_string(&mut t).map_err(err)?;
                (Some(ino), t)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (None, String::new()),
            Err(e) => return Err(err(e)),
        };
        // an append caught mid-write comes back whole in the tail below
        let cut = text.rfind('\n').map_or(0, |i| i + 1);
        let mine = parse_jsonl(&text[..cut]);
        let lines = merge_rows(&mine, theirs);
        let changed = lines != merge_rows(&mine, &[]);
        let out = jsonl(&lines);
        if out == text[..cut] {
            return Ok((changed, lines.len()));
        }
        let ids: std::collections::HashSet<&str> = lines
            .iter()
            .map(|r| r.get("id").and_then(Value::as_str).unwrap_or(""))
            .collect();
        let swapped = write_atomic_then(LINES, out.as_bytes(), |tmp| {
            let mut lock = locked()?;
            let now = lock.metadata().map_err(err)?;
            let same = ino.map_or(now.len() == 0, |i| i == now.ino());
            if !same || now.len() < cut as u64 {
                return Ok(None);
            }
            let mut tail = String::new();
            lock.seek(SeekFrom::Start(cut as u64)).map_err(err)?;
            lock.read_to_string(&mut tail).map_err(err)?;
            let appended = parse_jsonl(&tail);
            let edits_merged = appended
                .iter()
                .any(|r| ids.contains(r.get("id").and_then(Value::as_str).unwrap_or("")));
            if edits_merged {
                return Ok(None);
            }
            tmp.write_all(tail.as_bytes()).map_err(err)?;
            Ok(Some((lock, appended.len())))
        })?;
        if let Some((_lock, appended)) = swapped {
            return Ok((changed, lines.len() + appended)); // the lock drops after the swap
        }
    }
    let mut lock = locked()?;
    let mut text = String::new(); // under the lock: every line the transcriber has written
    lock.read_to_string(&mut text).map_err(err)?;
    let mine = parse_jsonl(&text);
    let lines = merge_rows(&mine, theirs);
    let changed = lines != merge_rows(&mine, &[]);
    write_new(LINES, jsonl(&lines).as_bytes())?;
    Ok((changed, lines.len()))
}

/// `ozen merge DIR` (DIR relative to where it was called from): prints the result, or exits 1.
pub fn cli(cwd: &std::path::Path, usage: &str) {
    let out = std::env::args().nth(2).ok_or(usage.to_string());
    match out.and_then(|d| merge_from(&cwd.join(d).to_string_lossy())) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
#[path = "merge_prop_tests.rs"]
mod prop_tests;
#[cfg(test)]
#[path = "merge_stall_tests.rs"]
mod stall_tests;
#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
