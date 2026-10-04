//! `ozen merge DIR`: what a sync will do, by hand. src/crdt.rs has the merge rules and the list of what's synced.
use crate::crdt::{Row, merge_maps, merge_rows, parse_jsonl, write_atomic};
use crate::mcp::{VOCAB, VOCAB_TXT, locked};
use crate::places;
use serde_json::{Value, json};
use std::fs;
use std::io::Read;

const TAGS: &str = "tags.json";
const FIXES: &str = "fixes.json";
const LINES: &str = "lines.jsonl";

/// `ozen merge DIR`: merge another ozen folder's synced data into this one (src/crdt.rs lists what's
/// synced), then relearn and retrain from the result like after any fix or tag.
pub fn merge_from(dir: &str) -> Result<String, String> {
    let out = merge_files(dir)?;
    crate::fixes::relearn()?;
    crate::retrain();
    Ok(out)
}

fn save(path: &str, v: &impl serde::Serialize) -> Result<(), String> {
    let json = serde_json::to_string_pretty(v).map_err(|e| e.to_string())? + "\n";
    write_atomic(path, json.as_bytes())
}

/// The file half of `merge_from`: merges `dir`'s synced files into the ones here. Every file is replaced
/// crash-safely (`write_atomic`), all under the transcriber's lines.jsonl lock, so none of its lines is
/// lost and two merges never interleave.
pub fn merge_files(dir: &str) -> Result<String, String> {
    if !std::path::Path::new(dir).is_dir() {
        return Err(format!("no folder {dir}"));
    }
    let mut lock = locked()?;
    let raw = |f: &str| -> Row {
        serde_json::from_slice(&fs::read(f).unwrap_or_default()).unwrap_or_default()
    };
    let vocab_raw = |d: &str| -> Row {
        if fs::metadata(format!("{d}{VOCAB}")).is_ok() {
            return raw(&format!("{d}{VOCAB}"));
        }
        let words = fs::read_to_string(format!("{d}{VOCAB_TXT}")).unwrap_or_default();
        words
            .split([',', '\n'])
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(|w| (w.into(), json!(true)))
            .collect()
    };
    let theirs = format!("{}/", dir.trim_end_matches('/'));
    for f in [TAGS, FIXES] {
        save(f, &merge_maps(&raw(f), &raw(&format!("{theirs}{f}"))))?;
    }
    save(VOCAB, &merge_maps(&vocab_raw(""), &vocab_raw(&theirs)))?;
    let _ = fs::remove_file(VOCAB_TXT);
    let rows = |f: &str| -> Vec<Row> {
        serde_json::from_slice(&fs::read(f).unwrap_or_default()).unwrap_or_default()
    };
    let p = merge_rows(
        &rows(places::FILE),
        &rows(&format!("{theirs}{}", places::FILE)),
    );
    save(places::FILE, &p)?;

    let other = parse_jsonl(&fs::read_to_string(format!("{theirs}{LINES}")).unwrap_or_default());
    let mut text = String::new();
    lock.read_to_string(&mut text).map_err(|e| e.to_string())?;
    let lines = merge_rows(&parse_jsonl(&text), &other);
    let out: String = lines
        .iter()
        .map(|r| Value::Object(r.clone()).to_string() + "\n")
        .collect();
    write_atomic(LINES, out.as_bytes())?;
    drop(lock);
    Ok(format!("merged: {} lines, {} places", lines.len(), p.len()))
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
