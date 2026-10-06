//! What two Macs of one vault say to each other. The relay keeps nothing, so Macs online together work
//! out between them what each lacks: each sends its 256 bucket hashes (buckets.rs); the other answers
//! with a summary (each record's version and hash) of just the buckets that differ; that is answered
//! with each record the answering Mac has newer, different at the same version, or missing from the
//! summary, and both merge what they get through `ozen merge`'s path (apply.rs, which also retrains). A local edit goes
//! out as soon as `changes` is called. Messages are JSON, deflated, then sealed (seal.rs) into frames;
//! carrying the frames is the connection's job. The protocol is specified, for other clients and for
//! agreeing on changes, in https://github.com/ozenhq/sync/blob/main/docs/protocol.md.
use super::apply::{self, Coalesced};
use super::buckets;
pub use super::dropped::Dropped;
use super::key::Key;
use super::marks::Marks;
use super::parts;
pub(super) use super::records::mark;
use super::records::records;
#[cfg(test)]
use super::seal;
use super::seal::Sealed;
use crate::merge::{self, Synced};
use base64::Engine;
use serde_json::Value;
use std::collections::BTreeMap;

/// A synced record: (kind, key). The kinds are the synced files: tags, fixes, vocab, places, lines.
pub(super) type Id = (String, String);
/// What a summary says about a record: its version, and the first 8 bytes of SHA-256 of its canonical
/// JSON (`crdt::canonical`, hex).
pub(super) type Mark = (u64, String);
use super::summaries::{Entry, Partial};
// what the tests in this module's files build frames with
use super::version::Versions;
use super::wire::{self, BUDGET, Msg, frame_version};
#[cfg(test)]
use {
    super::version::VERSION,
    crate::crdt::v,
    crate::merge::Records,
    flate2::{Compression, write::DeflateEncoder},
    std::io::Read,
};

/// Received records as a folder's worth of synced data, for `merge::apply`. A kind this build doesn't
/// know (from a newer ozen) is left out.
fn synced(recs: Vec<(String, String, Value)>) -> Synced {
    let mut s = Synced::default();
    for (kind, k, r) in recs {
        s.insert(&kind, k, r);
    }
    s
}

/// One Mac's side of the conversation with the vault's other Macs, over one connection. Records only
/// flow in answer to a summary or as local changes, so every Mac says `hello` when it connects and when
/// the online count rises (relay presence frames, OFE-31).
pub struct Session {
    seal_key: Key,
    vault: String,
    /// What the other Macs last heard about each record here, so `changes` sends only what's new; marks
    /// in protocol version `told_in`.
    told: BTreeMap<Id, Mark>,
    told_in: u8,
    /// Summaries still arriving, bounded (summaries.rs).
    partial: Partial,
    /// Records too big for one frame, still arriving in parts.
    parts: parts::Pending,
    /// Relearn and retrain after received records change something.
    after: Coalesced,
    /// Marks and bucket hashes of the records here, kept while the files don't change.
    marks: Marks,
    /// The protocol versions this Mac and the others speak (version.rs).
    versions: Versions,
    pub dropped: Dropped,
    /// The synced files' stamps at the last `tick`, so a tick with no edit reads nothing.
    ticked: Option<super::marks::Stamp>,
}

impl Session {
    /// A session that runs `after` (instead of relearn + `ozen retrain`) when received records change something.
    pub fn with(seal_key: Key, vault: &str, after: Coalesced) -> Self {
        Session {
            seal_key,
            vault: vault.into(),
            told: BTreeMap::new(),
            told_in: super::version::VERSION,
            partial: Partial::default(),
            parts: parts::Pending::default(),
            after,
            dropped: Dropped::default(),
            versions: Versions::default(),
            marks: Marks::default(),
            ticked: None,
        }
    }

    fn frame(&self, m: &Msg) -> Result<Sealed, String> {
        self.frame_in(self.versions.speak(), m)
    }

    fn frame_in(&self, version: u8, m: &Msg) -> Result<Sealed, String> {
        wire::encode(&self.seal_key, &self.vault, version, m)
    }

    /// Frames carrying `recs`, and the ids that went out. A record too big for one frame goes in parts;
    /// one over `parts::CAP` stays here (reported on stderr): the rest still go.
    fn records_frames(&self, recs: Vec<(Id, Value)>) -> Result<(Vec<Sealed>, Vec<Id>), String> {
        let flat = recs.into_iter().map(|((k, key), r)| (k, key, r)).collect();
        let (groups, big) = parts::groups(flat, BUDGET);
        let (mut sent, mut frames) = (vec![], vec![]);
        for (k, key, r) in big {
            match parts::split(&k, &key, &r, BUDGET) {
                Ok(ps) => {
                    for p in ps {
                        frames.push(self.frame(&Msg::Part(p))?);
                    }
                    sent.push((k, key));
                }
                Err(e) => eprintln!("sync: {e}; not sent"),
            }
        }
        for r in groups.into_iter().filter(|g| !g.is_empty()) {
            sent.extend(r.iter().map(|(k, key, _)| (k.clone(), key.clone())));
            frames.push(self.frame(&Msg::Records { r })?);
        }
        Ok((frames, sent))
    }

    /// The bucket hashes of everything here, one frame: sent on connecting and when another Mac comes
    /// online. The other Mac answers with summaries of the buckets that differ.
    pub fn hello(&mut self) -> Result<Vec<Sealed>, String> {
        let (ours, hashes) = self.ours();
        let h = base64::engine::general_purpose::STANDARD.encode(hashes);
        self.told = ours.clone();
        self.told_in = self.versions.speak();
        let max = self.versions.own();
        Ok(vec![self.frame_in(
            self.versions.oldest(),
            &Msg::Buckets { h, max },
        )?])
    }

    /// The marks and bucket hashes of the records here, read from the files only when they changed.
    fn ours(&mut self) -> (&BTreeMap<Id, Mark>, &[u8]) {
        self.marks.current(self.versions.speak(), || {
            let all = records(&merge::read_synced(""), self.versions.speak());
            all.iter().map(|(id, r)| (id.clone(), mark(r))).collect()
        })
    }

    /// Summary frames of the records here in buckets `b` (at least one frame, even with none).
    fn summary(&self, ours: &BTreeMap<Id, Mark>, b: Vec<u8>) -> Result<Vec<Sealed>, String> {
        let wanted = buckets::set(&b);
        let s: Vec<Entry> = ours
            .iter()
            .filter(|((k, key), _)| wanted[buckets::of(k, key) as usize])
            .map(|((k, key), (v, h))| (k.clone(), key.clone(), *v, h.clone()))
            .collect();
        let parts = parts::groups(s, BUDGET).0;
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
                    b: b.clone(),
                })
            })
            .collect()
    }

    /// Frames with every record that changed here since the other Macs last heard about it.
    pub fn changes(&mut self) -> Result<Vec<Sealed>, String> {
        self.retell();
        let ours = records(&merge::read_synced(""), self.versions.speak());
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

    /// `changes`, but only once a synced file changed since the last tick (the connection ticks every
    /// few seconds). Who wrote it doesn't matter: a record merged from another Mac that ours beat goes out too.
    pub fn tick(&mut self) -> Result<Vec<Sealed>, String> {
        let now = super::marks::stamp();
        if self.ticked.as_ref() == Some(&now) {
            return Ok(vec![]);
        }
        self.ticked = Some(now);
        self.changes()
    }

    /// The message in `frame`: Err for one that doesn't open or parse, Ok(None) for a newer version's.
    fn decode(&self, frame: &[u8]) -> Result<Option<Msg>, String> {
        wire::decode(&self.seal_key, &self.vault, &self.versions, frame)
    }

    /// Handles one frame from another Mac; returns the frames to send back. A summary is answered (once
    /// all its parts are in) with the records the other Mac lacks; records are merged into the files here.
    /// A frame this Mac can't use is dropped and counted in `dropped`, never merged, and the session
    /// goes on: the sender still has its records and resends them at the next summary exchange. Only
    /// failing to write here is an error. What was dropped is noted for `ozen health` (dropped.rs).
    pub fn receive(&mut self, frame: &[u8]) -> Result<Vec<Sealed>, String> {
        let before = self.dropped.clone();
        let out = self.take(frame);
        self.dropped.note_since(&before);
        out
    }

    fn take(&mut self, frame: &[u8]) -> Result<Vec<Sealed>, String> {
        let msg = match self.decode(frame) {
            Ok(Some(m)) => m,
            Ok(None) => {
                self.dropped.newer += 1;
                self.dropped.last_error = Some(format!(
                    "frame from a newer ozen (protocol {}, this one speaks {})",
                    frame_version(&self.seal_key, &self.vault, frame),
                    self.versions.own()
                ));
                return Ok(vec![]);
            }
            Err(e) => {
                self.dropped.bad += 1;
                self.dropped.last_error = Some(e);
                return Ok(vec![]);
            }
        };
        match msg {
            Msg::Buckets { h, max } => {
                self.versions.heard(max);
                let theirs = base64::engine::general_purpose::STANDARD
                    .decode(h)
                    .unwrap_or_default();
                let (ours, hashes) = self.ours();
                let diff = buckets::differing(hashes, &theirs);
                if diff.is_empty() {
                    return Ok(vec![]); // in sync
                }
                let ours = ours.clone();
                self.summary(&ours, diff)
            }
            Msg::Summary {
                id,
                part,
                parts,
                s,
                b,
            } => {
                let theirs: BTreeMap<Id, Mark> = match self.partial.add(id, part, parts, s) {
                    Ok(Some(all)) => all
                        .into_iter()
                        .map(|(k, key, v, h)| ((k, key), (v, h)))
                        .collect(),
                    Ok(None) => return Ok(vec![]),
                    Err(e) => {
                        self.dropped.refused += 1;
                        self.dropped.last_error = Some(e);
                        return Ok(vec![]);
                    }
                };
                let wanted = buckets::set(&b);
                let lack = records(&merge::read_synced(""), self.versions.speak())
                    .into_iter()
                    .filter(|((k, key), _)| wanted[buckets::of(k, key) as usize])
                    .filter(|(id, r)| {
                        let (v, h) = mark(r);
                        theirs
                            .get(id)
                            .is_none_or(|(tv, th)| v > *tv || (v == *tv && h != *th))
                    })
                    .collect();
                Ok(self.records_frames(lack)?.0)
            }
            Msg::Part(p) => {
                let now = std::time::Instant::now();
                let expired = self.parts.expire(now);
                if expired > 0 {
                    self.dropped.refused += expired as u64;
                    self.dropped.last_error =
                        Some(format!("{expired} record(s) in parts timed out"));
                }
                match self.parts.add(p, BUDGET, now) {
                    Ok(Some(rec)) => self.merge(vec![rec]),
                    Ok(None) => Ok(vec![]),
                    Err(e) => {
                        self.dropped.refused += 1;
                        self.dropped.last_error = Some(e);
                        Ok(vec![])
                    }
                }
            }
            Msg::Records { r } => self.merge(r),
        }
    }

    /// After this Mac starts speaking another version (it heard an older Mac), what it told the others
    /// still holds: a record whose mark in the old version is what they heard gets its mark in the new
    /// one, so `changes` doesn't resend every line in its other form.
    fn retell(&mut self) {
        let now = self.versions.speak();
        if self.told_in == now {
            return;
        }
        let s = merge::read_synced("");
        let (old, new) = (records(&s, self.told_in), records(&s, now));
        let told = std::mem::take(&mut self.told);
        self.told = told
            .into_iter()
            .filter(|(id, m)| old.get(id).map(mark).as_ref() == Some(m))
            .filter_map(|(id, _)| Some((id.clone(), mark(new.get(&id)?))))
            .collect();
        self.told_in = now;
    }

    /// Merges received records (kind, key, record) into the files here.
    fn merge(&mut self, r: Vec<(String, String, Value)>) -> Result<Vec<Sealed>, String> {
        let (r, errors) = super::valid::keep(r); // the rest of the frame still merges
        if let Some(e) = errors.last() {
            self.dropped.records += errors.len() as u64;
            self.dropped.last_error = Some(e.clone());
        }
        let got: Vec<(Id, Mark)> = r
            .iter()
            .map(|(k, key, rec)| ((k.clone(), key.clone()), mark(rec)))
            .collect();
        self.retell();
        apply::received(&synced(r), &self.after)?;
        // A record that merged to exactly what the sender has is known to them: not a change to
        // send back. One where ours won stays unmarked, so `changes` sends it.
        let ours = records(&merge::read_synced(""), self.versions.speak());
        for (id, m) in got {
            if ours.get(&id).map(mark).as_ref() == Some(&m) {
                self.told.insert(id, m);
            }
        }
        Ok(vec![])
    }
}

#[cfg(test)]
#[path = "protocol_chain_tests.rs"]
mod chain_tests;
#[cfg(test)]
#[path = "protocol_drop_tests.rs"]
mod drop_tests;
#[cfg(test)]
#[path = "protocol_fuzz_tests.rs"]
mod fuzz_tests;
#[cfg(test)]
#[path = "protocol_golden_tests.rs"]
mod golden_tests;
#[cfg(test)]
#[path = "protocol_marks_tests.rs"]
mod marks_tests;
#[cfg(test)]
#[path = "protocol_parts_tests.rs"]
mod parts_tests;
#[cfg(test)]
#[path = "protocol_prop_tests.rs"]
mod prop_tests;
#[cfg(test)]
#[path = "protocol_scale_tests.rs"]
mod scale_tests;
#[cfg(test)]
#[path = "protocol_synced_tests.rs"]
mod synced_tests;
#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "protocol_version_tests.rs"]
mod version_tests;
