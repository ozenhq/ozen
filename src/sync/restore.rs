//! Restore points before big received batches (OFE-69). The relay keeps no copy, so if another Mac's
//! records damage the data here (a bug in its ozen, a bad merge), nothing outside this Mac could put it
//! back. Before merging a batch of more than `BIG` records, the synced files are copied to
//! `.sync-restore/<unix seconds>/` (local, never synced); the newest `KEEP` from the last `MAX_AGE`
//! are kept. `ozen sync undo` puts the newest back and pauses sync until `ozen sync init`.
use crate::merge::Synced;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DIR: &str = ".sync-restore";
/// Records in a received batch past which a restore point is taken first.
pub const BIG: usize = 100;
const KEEP: usize = 5;
/// A sync that brings a big history comes in many frames: one restore point, before the first, covers
/// them all. A big batch within this long of the newest point takes none.
const BURST: Duration = Duration::from_secs(10 * 60);
const MAX_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// While this exists, sync stays off here (run.rs); `ozen sync init` removes it.
pub const PAUSED: &str = ".sync-paused";

/// The files a restore point holds: every synced file, and the vocabulary list from before vocab.json.
fn files() -> impl Iterator<Item = &'static str> {
    crate::crdt::SYNCED
        .iter()
        .map(|(_, f)| *f)
        .chain([crate::mcp::VOCAB_TXT])
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The restore points, oldest first, by their time.
fn points() -> Vec<(u64, PathBuf)> {
    let mut p: Vec<(u64, PathBuf)> = fs::read_dir(DIR)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((e.file_name().to_str()?.parse().ok()?, e.path())))
        .collect();
    p.sort();
    p
}

/// How many records `theirs` holds.
pub fn size(theirs: &Synced) -> usize {
    theirs.tags.len()
        + theirs.fixes.len()
        + theirs.vocab.len()
        + theirs.places.len()
        + theirs.lines.len()
}

/// Before merging `theirs`: a restore point if it's big and none was taken in this burst. Refuses
/// while `ozen sync undo` has paused sync, so a frame that arrives during an undo can't merge again
/// what was just put back.
pub fn before(theirs: &Synced) -> Result<(), String> {
    if Path::new(PAUSED).exists() {
        return Err("sync is paused after `ozen sync undo`: not merging".into());
    }
    let recent = points()
        .last()
        .is_some_and(|(t, _)| now().saturating_sub(*t) < BURST.as_secs());
    if size(theirs) > BIG && !recent {
        take()?;
    }
    Ok(())
}

/// Copies the synced files here into a new restore point (a file that doesn't exist is left out, and
/// an undo removes it again), then prunes old ones. Under the lines lock, so a line being written
/// isn't caught half way.
pub fn take() -> Result<PathBuf, String> {
    let _lock = crate::mcp::locked()?;
    let mut at = now();
    while Path::new(DIR).join(at.to_string()).exists() {
        at += 1; // two in one second
    }
    let dir = Path::new(DIR).join(at.to_string());
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for f in files().filter(|f| Path::new(f).exists()) {
        fs::copy(f, dir.join(f)).map_err(|e| format!("{f}: {e}"))?;
    }
    prune(now());
    Ok(dir)
}

/// Removes restore points older than `MAX_AGE` at `at`, and all but the newest `KEEP`.
fn prune(at: u64) {
    let all = points();
    let n = all.len();
    for (i, (t, p)) in all.into_iter().enumerate() {
        if i + KEEP < n || at.saturating_sub(t) > MAX_AGE.as_secs() {
            let _ = fs::remove_dir_all(p);
        }
    }
}

/// `ozen sync undo`: saves the files as they are now (so this undo can be undone in turn), puts the
/// newest earlier restore point back, and pauses sync.
pub fn undo() -> Result<String, String> {
    let Some((t, from)) = points().pop() else {
        return Err(
            "no restore point here: one is taken before each big batch from another Mac".into(),
        );
    };
    fs::write(PAUSED, "").map_err(|e| format!("{PAUSED}: {e}"))?;
    let saved = take()?;
    // Waits for a merge in progress (it holds this lock); `PAUSED` stops the ones after.
    let _lock = crate::mcp::locked()?;
    for f in files() {
        let src = from.join(f);
        let bytes = fs::read(&src).ok();
        if f == crate::merge::LINES {
            // in place: the lock holds this file open, and lines are only ever appended
            fs::write(f, bytes.unwrap_or_default()).map_err(|e| format!("{f}: {e}"))?;
        } else if let Some(b) = bytes {
            crate::crdt::write_atomic(f, &b)?;
        } else {
            let _ = fs::remove_file(f);
        }
    }
    Ok(format!(
        "restored the synced files as they were at {t} (from {}). What was here, including lines \
transcribed since then, is saved in {}: running `ozen sync undo` again puts it back.\nsync is \
paused: run `ozen sync init` to turn it back on",
        from.display(),
        saved.display()
    ))
}

#[cfg(test)]
#[path = "restore_tests.rs"]
mod tests;
