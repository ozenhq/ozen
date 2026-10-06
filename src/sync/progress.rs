//! How far a big exchange with another Mac has got (OFE-78). Joining a Mac with months of meetings
//! brings thousands of records; the menu bar shows "Syncing with <Mac>: 420 / 1,300" so the user can
//! tell it works and doesn't quit mid-way (the relay keeps nothing, so a quit exchange starts over).
//! A session learns how many records are coming from the other Mac's summary (protocol.rs) and counts
//! them as they merge; `ozen sync status` reports it while it's fresh, the panel footer shows it.
use super::protocol::{Id, Mark};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;

/// `{"mac": "<name>", "done": n, "total": m, "at": <unix seconds>}` while a big exchange runs.
pub const FILE: &str = ".sync-progress.json";
/// Exchanges up to this many records finish in seconds: nothing to show.
pub const BIG: u64 = 200;
/// A note not updated for this long is from an exchange that stopped (a dropped connection).
const STALE_SECS: u64 = 60;

/// One session's count of the records coming from the other Mac.
#[derive(Default)]
pub struct Progress {
    mac: String,
    total: u64,
    done: u64,
}

impl Progress {
    /// The other Mac said hello as `name`.
    pub fn heard(&mut self, name: &str) {
        self.mac = name.into();
    }

    /// The other Mac's summary says `n` records here are older or missing: they're coming.
    pub fn expect(&mut self, n: u64) {
        (self.total, self.done) = (n, 0);
        self.note();
    }

    /// `n` of them merged.
    pub fn got(&mut self, n: u64) {
        if self.total == 0 {
            return; // a local edit from the other Mac, not part of an exchange
        }
        self.done += n;
        if self.done >= self.total {
            self.total = 0;
        }
        self.note();
    }

    /// Writes the note while the exchange is big, and removes it when it's done.
    fn note(&self) {
        // ponytail: one note for all connections, the last writer wins; per-Mac notes if two big
        // exchanges at once ever matter. A failed write only hides the progress.
        if self.total > BIG {
            let n = json!({"mac": self.mac, "done": self.done, "total": self.total,
                "at": super::status::now()});
            let _ = fs::write(FILE, n.to_string());
        } else {
            let _ = fs::remove_file(FILE);
        }
    }
}

/// How many of the records in the other Mac's summary `theirs` it will send: the ones missing here, or
/// newer there, or different at the same version (what protocol.rs's answer to a summary sends).
pub(super) fn coming(ours: &BTreeMap<Id, Mark>, theirs: &BTreeMap<Id, Mark>) -> u64 {
    theirs
        .iter()
        .filter(|(id, (tv, th))| {
            ours.get(*id)
                .is_none_or(|(v, h)| tv > v || (tv == v && th != h))
        })
        .count() as u64
}

/// The exchange in progress at `now`, for `ozen sync status`: none once it's done or stale.
pub fn read_at(now: u64) -> Option<Value> {
    let v: Value = serde_json::from_slice(&fs::read(FILE).ok()?).ok()?;
    let fresh = now.saturating_sub(v["at"].as_u64()?) < STALE_SECS;
    (fresh && v["done"].as_u64()? < v["total"].as_u64()?)
        .then(|| json!({"mac": v["mac"], "done": v["done"], "total": v["total"]}))
}

/// `n` as people read it: 1,300.
fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// What the panel footer says about `ozen sync status`'s "progress": only for a big exchange.
pub fn line(progress: &Value) -> Option<String> {
    let (done, total) = (progress["done"].as_u64()?, progress["total"].as_u64()?);
    let mac = progress["mac"]
        .as_str()
        .filter(|m| !m.is_empty())
        .unwrap_or("another Mac");
    (total > BIG && done < total)
        .then(|| format!("Syncing with {mac}: {} / {}", count(done), count(total)))
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
