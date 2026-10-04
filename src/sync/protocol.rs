//! What two Macs of one vault say to each other. The relay keeps nothing, so Macs online together work
//! out between them what each lacks: each sends a summary (every synced record's version and hash), the
//! other answers with each record it has newer, different at the same version, or missing from the
//! summary, and both merge what they get through `ozen merge`'s path (apply.rs, which also retrains). A local edit goes
//! out as soon as `changes` is called. Messages are JSON, deflated, then sealed (seal.rs) into frames;
//! carrying the frames is the connection's job.
#![allow(dead_code)] // ponytail: driven by the connection (OFE-7)
use super::apply::{self, Coalesced};
use super::key::Key;
use super::seal;
use crate::crdt::{live_ids, v};
use crate::merge::{self, Synced};
use flate2::{Compression, read::DeflateDecoder, write::DeflateEncoder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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

/// `items` in groups whose JSON fits one frame (always at least one group, maybe empty). An item that
/// can't fit even alone is left out and returned separately.
fn groups<T: Serialize>(items: Vec<T>) -> (Vec<Vec<T>>, Vec<T>) {
    let (mut out, mut big): (Vec<Vec<T>>, Vec<T>) = (vec![vec![]], vec![]);
    let mut size = 0;
    for it in items {
        let n = serde_json::to_vec(&it).map_or(usize::MAX, |j| j.len() + 1);
        if n > BUDGET {
            big.push(it);
            continue;
        }
        if size + n > BUDGET {
            out.push(vec![]);
            size = 0;
        }
        size += n;
        out.last_mut().expect("one group").push(it);
    }
    (out, big)
}

/// One Mac's side of the conversation with the vault's other Macs, over one connection. Records only
/// flow in answer to a summary or as local changes, so every Mac says `hello` when it connects and when
/// the online count rises (relay presence frames, OFE-31).
pub struct Session {
    seal_key: Key,
    vault: String,
    /// What the other Macs last heard about each record here, so `changes` sends only what's new.
    told: BTreeMap<Id, Mark>,
    /// Summaries still arriving: id -> (parts, part -> entries).
    partial: BTreeMap<u64, (u32, BTreeMap<u32, Vec<Entry>>)>,
    /// Relearn and retrain after received records change something.
    after: Coalesced,
}

impl Session {
    pub fn new(seal_key: Key, vault: &str) -> Self {
        Session::with(seal_key, vault, Coalesced::retrain())
    }

    /// A session that runs `after` (instead of relearn + `ozen retrain`) when received records change something.
    pub fn with(seal_key: Key, vault: &str, after: Coalesced) -> Self {
        Session {
            seal_key,
            vault: vault.into(),
            told: BTreeMap::new(),
            partial: BTreeMap::new(),
            after,
        }
    }

    fn frame(&self, m: &Msg) -> Result<Vec<u8>, String> {
        let mut z = DeflateEncoder::new(vec![], Compression::default());
        serde_json::to_writer(&mut z, m).map_err(|e| e.to_string())?;
        let plain = z.finish().map_err(|e| e.to_string())?;
        seal::seal(&self.seal_key, &self.vault, &plain)
    }

    /// Frames carrying `recs`, and the ids that went out. A record too big for one frame stays here
    /// (reported on stderr): the rest still go.
    fn records_frames(&self, recs: Vec<(Id, Value)>) -> Result<(Vec<Vec<u8>>, Vec<Id>), String> {
        let flat = recs.into_iter().map(|((k, key), r)| (k, key, r)).collect();
        let (groups, big) = groups(flat);
        for (k, key, _) in big {
            eprintln!("sync: {k} {key} is too big for one frame; not sent");
        }
        let mut sent = vec![];
        let mut frames = vec![];
        for r in groups.into_iter().filter(|g| !g.is_empty()) {
            sent.extend(r.iter().map(|(k, key, _)| (k.clone(), key.clone())));
            frames.push(self.frame(&Msg::Records { r })?);
        }
        Ok((frames, sent))
    }

    /// The summary of everything here, as frames (at least one, so a Mac with nothing yet still asks).
    pub fn hello(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let ours = records(&merge::read_synced(""));
        self.told = ours.iter().map(|(id, r)| (id.clone(), mark(r))).collect();
        let s: Vec<_> = self
            .told
            .iter()
            .map(|((k, key), (v, h))| (k.clone(), key.clone(), *v, h.clone()))
            .collect();
        let parts = groups(s).0;
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
        let ours = records(&merge::read_synced(""));
        let changed: Vec<(Id, Value)> = ours
            .iter()
            .filter(|(id, r)| self.told.get(*id) != Some(&mark(r)))
            .map(|(id, r)| (id.clone(), r.clone()))
            .collect();
        let (frames, sent) = self.records_frames(changed)?;
        for id in sent {
            self.told.insert(id.clone(), mark(&ours[&id]));
        }
        Ok(frames)
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
            Msg::Summary { id, part, parts, s } => {
                if self.partial.len() >= PARTIAL && !self.partial.contains_key(&id) {
                    self.partial.pop_first(); // the oldest: ids are send times
                }
                let p = self.partial.entry(id).or_insert((parts, BTreeMap::new()));
                p.1.insert(part, s); // a repeated part replaces itself
                if (p.1.len() as u32) < p.0 {
                    return Ok(vec![]);
                }
                let theirs: BTreeMap<Id, Mark> = self
                    .partial
                    .remove(&id)
                    .expect("entry")
                    .1
                    .into_values()
                    .flatten()
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
                Ok(self.records_frames(lack)?.0)
            }
            Msg::Records { r } => {
                let got: Vec<(Id, Mark)> = r
                    .iter()
                    .map(|(k, key, rec)| ((k.clone(), key.clone()), mark(rec)))
                    .collect();
                apply::received(&synced(r), &self.after)?;
                // A record that merged to exactly what the sender has is known to them: not a change to
                // send back. One where ours won stays unmarked, so `changes` sends it.
                let ours = records(&merge::read_synced(""));
                for (id, m) in got {
                    if ours.get(&id).map(mark).as_ref() == Some(&m) {
                        self.told.insert(id, m);
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
