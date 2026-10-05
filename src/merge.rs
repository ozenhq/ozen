//! `ozen merge DIR`: what a sync will do, by hand. src/crdt.rs has the merge rules and the list of what's synced.
use crate::crdt::{Row, merge_maps, merge_rows, parse_jsonl, write_atomic};
use crate::mcp::{VOCAB, VOCAB_TXT, locked};
use crate::places;
use serde_json::{Value, json};
use std::fs;
use std::io::Read;

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
fn save(path: &str, v: &impl serde::Serialize) -> Result<(), String> {
    let v = crate::crdt::sorted(&serde_json::to_value(v).map_err(|e| e.to_string())?);
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

/// The synced data in folder `d` ("" for this one, otherwise ending in '/'). A folder from before
/// vocab.json has its words in vocab.txt.
pub fn read_synced(d: &str) -> Synced {
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
        lines: parse_jsonl(&fs::read_to_string(format!("{d}{LINES}")).unwrap_or_default()),
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
/// replaced crash-safely (`write_atomic`), all under the transcriber's lines.jsonl lock, so none of its
/// lines is lost and two merges never interleave. A file that would come out the same isn't rewritten.
pub fn apply(theirs: &Synced) -> Result<Applied, String> {
    let mut lock = locked()?;
    let ours = read_synced("");
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

    let mut text = String::new(); // under the lock: every line the transcriber has written
    lock.read_to_string(&mut text).map_err(|e| e.to_string())?;
    let mine = parse_jsonl(&text);
    let lines = merge_rows(&mine, &theirs.lines);
    changed |= lines != merge_rows(&mine, &[]);
    let out: String = lines
        .iter()
        .map(|r| crate::crdt::canonical(&Value::Object(r.clone())) + "\n")
        .collect();
    write_new(LINES, out.as_bytes())?;
    drop(lock);
    Ok(Applied {
        changed,
        lines: lines.len(),
        places: p.len(),
    })
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
#[path = "merge_tests.rs"]
mod tests;
