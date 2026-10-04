//! What two Macs of one vault say to each other. The relay keeps nothing, so Macs online together work
//! out between them what each lacks: each sends a summary (every synced record's version and hash), the
//! other answers with each record it has newer, different at the same version, or missing from the
//! summary, and both merge what they get through `ozen merge`'s path (`merge::apply`). A local edit goes
//! out as soon as `changes` is called. Messages are JSON, deflated, then sealed (seal.rs) into frames;
//! carrying the frames is the connection's job.
#![allow(dead_code)] // ponytail: driven by the connection (OFE-7)
use super::key::Key;
use super::seal;
use crate::crdt::{live_ids, v};
use crate::merge::{self, Synced};
use flate2::{Compression, read::DeflateDecoder, write::DeflateEncoder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::io::Read;

/// A synced record: (kind, key). The kinds are the synced files: tags, fixes, vocab, places, lines.
type Id = (String, String);
/// What a summary says about a record: its version, and the first 8 bytes of SHA-256 of its JSON (hex).
type Mark = (u64, String);
/// One summary line: (kind, key, v, hash).
type Entry = (String, String, u64, String);
/// Raw JSON per frame: deflated, it stays under `seal::MAX` even when nothing compresses.
const BUDGET: usize = seal::MAX - 256;
/// Unfinished summaries kept; a summary whose last part never came (a lagging Mac) is forgotten.
const PARTIAL: usize = 8;

#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
enum Msg {
    /// Part `part` (of `parts`) of summary `id`: (kind, key, v, hash) for records the sender has.
    Summary {
        id: u64,
        part: u32,
        parts: u32,
        s: Vec<Entry>,
    },
    /// Records for the receiver to merge: (kind, key, record).
    Records { r: Vec<(String, String, Value)> },
}

/// Every synced record in `s`. Rows written before ids get theirs from `live_ids`, as `merge_rows` does.
fn records(s: &Synced) -> BTreeMap<Id, Value> {
    let mut out = BTreeMap::new();
    for (kind, m) in [("tags", &s.tags), ("fixes", &s.fixes), ("vocab", &s.vocab)] {
        for (k, e) in m {
            out.insert((kind.into(), k.clone()), e.clone());
        }
    }
    for (kind, rows) in [("places", &s.places), ("lines", &s.lines)] {
        for r in live_ids(rows) {
            let k = r
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            out.insert((kind.into(), k), Value::Object(r));
        }
    }
    out
}

fn mark(r: &Value) -> Mark {
    let h = Sha256::digest(r.to_string().as_bytes());
    (v(r), h[..8].iter().map(|b| format!("{b:02x}")).collect())
}

/// Received records as a folder's worth of synced data, for `merge::apply`. A kind this build doesn't
/// know (from a newer ozen) is left out.
fn synced(recs: Vec<(String, String, Value)>) -> Synced {
    let mut s = Synced::default();
    for (kind, k, r) in recs {
        match (kind.as_str(), r) {
            ("tags", r) => drop(s.tags.insert(k, r)),
            ("fixes", r) => drop(s.fixes.insert(k, r)),
            ("vocab", r) => drop(s.vocab.insert(k, r)),
            ("places", Value::Object(r)) => s.places.push(r),
            ("lines", Value::Object(r)) => s.lines.push(r),
            _ => {}
        }
    }
    s
}

/// `items` in groups whose JSON fits one frame. One item over the budget is an error.
fn groups<T: Serialize>(items: Vec<T>) -> Result<Vec<Vec<T>>, String> {
    let mut out: Vec<Vec<T>> = vec![];
    let mut size = 0;
    for it in items {
        let n = serde_json::to_vec(&it).map_err(|e| e.to_string())?.len() + 1;
        if n > BUDGET {
            return Err(format!("a record of {n} bytes is too big to sync"));
        }
        if out.is_empty() || size + n > BUDGET {
            out.push(vec![]);
            size = 0;
        }
        size += n;
        out.last_mut().expect("pushed").push(it);
    }
    Ok(out)
}

/// One Mac's side of the conversation with the vault's other Macs, over one connection.
pub struct Session {
    seal_key: Key,
    vault: String,
    /// What the other Macs last heard about each record here, so `changes` sends only what's new.
    told: BTreeMap<Id, Mark>,
    /// Summaries still arriving: id -> (parts so far, entries).
    partial: HashMap<u64, (u32, Vec<Entry>)>,
}

impl Session {
    pub fn new(seal_key: Key, vault: &str) -> Self {
        Session {
            seal_key,
            vault: vault.into(),
            told: BTreeMap::new(),
            partial: HashMap::new(),
        }
    }

    fn frame(&self, m: &Msg) -> Result<Vec<u8>, String> {
        let mut z = DeflateEncoder::new(vec![], Compression::default());
        serde_json::to_writer(&mut z, m).map_err(|e| e.to_string())?;
        let plain = z.finish().map_err(|e| e.to_string())?;
        seal::seal(&self.seal_key, &self.vault, &plain)
    }

    fn records_frames(&self, recs: Vec<(Id, Value)>) -> Result<Vec<Vec<u8>>, String> {
        let flat = recs.into_iter().map(|((k, key), r)| (k, key, r)).collect();
        groups(flat)?
            .into_iter()
            .map(|r| self.frame(&Msg::Records { r }))
            .collect()
    }

    /// The summary of everything here, as frames: sent on connecting and when another Mac comes online.
    pub fn hello(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let ours = records(&merge::read_synced(""));
        self.told = ours.iter().map(|(id, r)| (id.clone(), mark(r))).collect();
        let s: Vec<_> = self
            .told
            .iter()
            .map(|((k, key), (v, h))| (k.clone(), key.clone(), *v, h.clone()))
            .collect();
        let parts = groups(s)?;
        let id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as u64);
        let n = parts.len() as u32;
        parts
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                self.frame(&Msg::Summary {
                    id,
                    part: i as u32,
                    parts: n,
                    s,
                })
            })
            .collect()
    }

    /// Frames with every record that changed here since the other Macs last heard about it.
    pub fn changes(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let changed: Vec<(Id, Value)> = records(&merge::read_synced(""))
            .into_iter()
            .filter(|(id, r)| self.told.get(id) != Some(&mark(r)))
            .collect();
        for (id, r) in &changed {
            self.told.insert(id.clone(), mark(r));
        }
        self.records_frames(changed)
    }

    /// Handles one frame from another Mac; returns the frames to send back. A summary is answered (once
    /// all its parts are in) with the records the other Mac lacks; records are merged into the files here.
    pub fn receive(&mut self, frame: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        let plain = seal::open(&self.seal_key, &self.vault, frame)?;
        let mut json = vec![];
        DeflateDecoder::new(&plain[..])
            .read_to_end(&mut json)
            .map_err(|e| format!("frame does not inflate: {e}"))?;
        match serde_json::from_slice(&json).map_err(|e| format!("frame is not a message: {e}"))? {
            Msg::Summary { id, parts, s, .. } => {
                if self.partial.len() >= PARTIAL && !self.partial.contains_key(&id) {
                    self.partial.clear(); // ponytail: only stale partials pile up this far
                }
                let p = self.partial.entry(id).or_default();
                p.0 += 1;
                p.1.extend(s);
                if p.0 < parts {
                    return Ok(vec![]);
                }
                let theirs: BTreeMap<Id, Mark> = self
                    .partial
                    .remove(&id)
                    .expect("entry")
                    .1
                    .into_iter()
                    .map(|(k, key, v, h)| ((k, key), (v, h)))
                    .collect();
                let lack = records(&merge::read_synced(""))
                    .into_iter()
                    .filter(|(id, r)| {
                        let (v, h) = mark(r);
                        theirs
                            .get(id)
                            .is_none_or(|(tv, th)| v > *tv || (v == *tv && h != *th))
                    })
                    .collect();
                self.records_frames(lack)
            }
            Msg::Records { r } => {
                let got: Vec<Id> = r
                    .iter()
                    .map(|(k, key, _)| (k.clone(), key.clone()))
                    .collect();
                merge::apply(&synced(r))?;
                // What's here now for those records is what the sender has too, or wins over it on merge:
                // not a local change to send back.
                let ours = records(&merge::read_synced(""));
                for id in got {
                    if let Some(r) = ours.get(&id) {
                        self.told.insert(id, mark(r));
                    }
                }
                Ok(vec![])
            }
        }
    }
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
