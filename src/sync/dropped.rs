//! Counts of what another Mac sent that this one couldn't use (protocol.rs). The sync process adds
//! each rise to `.sync-dropped.json`, so `ozen health` can tell the user, who is the only one who can
//! fix it (no server keeps a copy): a Mac dropping another's frames looks fine while the two drift apart.
use serde::{Deserialize, Serialize};
use std::fs;

/// Where the counts of the last day are kept, in the ozen folder.
pub const FILE: &str = ".sync-dropped.json";
/// Counts older than this since their last rise are stale: whatever caused them stopped.
const DAY: u64 = 24 * 60 * 60;

/// What happened to frames this Mac couldn't use.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Dropped {
    /// Frames that didn't open (another key, altered) or didn't parse: dropped, not merged.
    pub bad: u64,
    /// Frames from a newer protocol version: not merged until this Mac updates ozen.
    pub newer: u64,
    /// Records in frames that opened fine but failed validation (valid.rs): dropped, not merged.
    pub records: u64,
    /// Summary or record parts that opened fine but were refused: over summaries.rs's or parts.rs's
    /// bounds, inconsistent, or timed out. The other Mac resends at the next exchange.
    pub refused: u64,
    pub last_error: Option<String>,
}

/// The counts on disk and when they last rose (unix seconds).
#[derive(Default, Serialize, Deserialize)]
struct Seen {
    #[serde(flatten)]
    d: Dropped,
    at: u64,
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The counts of the last day, or none.
fn recent(at: u64) -> Dropped {
    fs::read(FILE)
        .ok()
        .and_then(|b| serde_json::from_slice::<Seen>(&b).ok())
        .filter(|s| at.saturating_sub(s.at) < DAY)
        .map(|s| s.d)
        .unwrap_or_default()
}

impl Dropped {
    /// The line to show the user, if any.
    pub fn advice(&self) -> Option<&'static str> {
        (self.newer > 0).then_some("another Mac runs a newer ozen: update ozen to sync with it")
    }

    /// Adds what rose since `before` to FILE. Best effort: failing to note it never stops sync.
    pub fn note_since(&self, before: &Dropped) {
        if self == before {
            return;
        }
        let at = now();
        let mut d = recent(at);
        d.bad += self.bad - before.bad;
        d.newer += self.newer - before.newer;
        d.records += self.records - before.records;
        d.refused += self.refused - before.refused;
        d.last_error.clone_from(&self.last_error);
        if let Ok(b) = serde_json::to_vec(&Seen { d, at }) {
            let _ = fs::write(FILE, b);
        }
    }
}

/// What `ozen health` says about frames dropped in the last day; nothing if none were.
/// Refused parts aren't shown: the other Mac resends them, so they fix themselves.
pub fn health() -> Vec<String> {
    let d = recent(now());
    let mut out: Vec<String> = d.advice().map(Into::into).into_iter().collect();
    if d.bad > 0 {
        out.push(format!(
            "{} frames from another Mac couldn't be read (wrong key? both Macs need the same vault key)",
            d.bad
        ));
    }
    if d.records > 0 {
        out.push(format!(
            "{} records from another Mac failed checks and were skipped (last: {})",
            d.records,
            d.last_error.as_deref().unwrap_or("unknown")
        ));
    }
    out
}
