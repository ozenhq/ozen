//! The marks (version, content hash) of every synced record here and their 256 bucket hashes, kept
//! between hellos (OFE-57). Macs say hello on every reconnect, wake and presence change, and lines.jsonl
//! is MBs and growing, so re-reading every file each time burns CPU for an answer that rarely changed.
//! Whoever writes the files (the transcriber, MCP, a merge, another sync session), a change shows in
//! each file's size, modified time or inode; only then are they read and hashed again. Like git's "racy
//! clean" check, a file modified less than `RACY` before the cache was built isn't trusted: two writes
//! of the same size within one timestamp tick would look unchanged.
use std::collections::BTreeMap;
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::protocol::{Id, Mark};

const RACY: Duration = Duration::from_secs(1);

/// (size, modified s, modified ns, inode) of each file the synced data is read from.
type Stamp = Vec<Option<(u64, i64, i64, u64)>>;

fn stamp() -> Stamp {
    crate::crdt::SYNCED
        .iter()
        .map(|(_, f)| *f)
        .chain([crate::mcp::VOCAB_TXT]) // read_synced's fallback for a folder from before vocab.json
        .map(|f| {
            let m = std::fs::metadata(f).ok()?;
            Some((m.len(), m.mtime(), m.mtime_nsec(), m.ino()))
        })
        .collect()
}

pub struct Marks {
    stamp: Option<Stamp>,
    /// When `stamp` was taken.
    at: SystemTime,
    marks: BTreeMap<Id, Mark>,
    hashes: Vec<u8>,
    /// How many times the files were read and hashed (for tests).
    pub reads: usize,
}

impl Default for Marks {
    fn default() -> Self {
        Marks {
            stamp: None,
            at: UNIX_EPOCH,
            marks: BTreeMap::new(),
            hashes: vec![],
            reads: 0,
        }
    }
}

/// Whether a cache built at `at` can trust `stamp`: every file last changed well before `at`.
fn settled(stamp: &Stamp, at: SystemTime) -> bool {
    let at = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    stamp.iter().flatten().all(|(_, s, ns, _)| {
        let modified = Duration::new((*s).max(0) as u64, (*ns).clamp(0, 999_999_999) as u32);
        modified + RACY < at
    })
}

impl Marks {
    /// The marks and bucket hashes of the records in this folder: `compute` runs again only
    /// when a synced file changed since the last call. The files are stat-ed before `compute` reads
    /// them, so a write in between shows as a change next time rather than being missed.
    pub fn current(
        &mut self,
        compute: impl FnOnce() -> BTreeMap<Id, Mark>,
    ) -> (&BTreeMap<Id, Mark>, &[u8]) {
        let now = stamp();
        if self.stamp.as_ref() != Some(&now) || !settled(&now, self.at) {
            self.at = SystemTime::now();
            self.marks = compute();
            self.hashes = super::buckets::hashes(
                self.marks
                    .iter()
                    .map(|((k, key), (v, h))| (k.as_str(), key.as_str(), *v, h.as_str())),
            );
            self.stamp = Some(now);
            self.reads += 1;
        }
        (&self.marks, &self.hashes)
    }
}
