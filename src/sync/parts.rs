//! Fitting records into frames. Records go out in groups that fit one frame; a record too big for any
//! frame (a long note, a very long line) is split into parts that the other Mac reassembles by
//! (kind, key, v) before merging, so every record syncs (OFE-76). Parts wait in memory only, bounded:
//! a record of at most `CAP`, at most `PENDING` records at once, each for at most `TIMEOUT`.
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The largest record that syncs, in JSON bytes; a peer can't make this Mac hold more per record.
pub const CAP: usize = 2 << 20;
/// Records arriving in parts at once; parts of a further one are dropped until one completes.
const PENDING: usize = 4;
/// How long a record's parts may take to arrive; then they are dropped (the sender resends at the next
/// summary exchange).
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// Raw JSON per group or part: base64 and the part's fields still fit one frame's budget.
fn chunk(budget: usize) -> usize {
    (budget - 1024) / 4 * 3
}

/// Part `i` (of `n`) of record (`k`, `key`) at version `v` whose content hash is `h` (as in summaries):
/// a slice of its JSON, base64. Two Macs may send different records at the same `v` (an equal-version
/// conflict); `h` keeps their parts apart.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Part {
    pub k: String,
    pub key: String,
    pub v: u64,
    pub h: String,
    pub i: u32,
    pub n: u32,
    pub d: String,
}

/// `items` in groups whose JSON fits `budget` (always at least one group, maybe empty). An item that
/// can't fit even alone is left out and returned separately, for `split`.
pub fn groups<T: Serialize>(items: Vec<T>, budget: usize) -> (Vec<Vec<T>>, Vec<T>) {
    let (mut out, mut big): (Vec<Vec<T>>, Vec<T>) = (vec![vec![]], vec![]);
    let mut size = 0;
    for it in items {
        let n = serde_json::to_vec(&it).map_or(usize::MAX, |j| j.len() + 1);
        if n > budget {
            big.push(it);
            continue;
        }
        if size + n > budget {
            out.push(vec![]);
            size = 0;
        }
        size += n;
        out.last_mut().expect("one group").push(it);
    }
    (out, big)
}

/// Record (`k`, `key`, `r`) as parts that each fit a frame of `budget`; Err for one over `CAP`.
pub fn split(k: &str, key: &str, r: &Value, budget: usize) -> Result<Vec<Part>, String> {
    let json = serde_json::to_vec(r).map_err(|e| e.to_string())?;
    if json.len() > CAP {
        return Err(format!("{k} {key} is {} bytes, over {CAP}", json.len()));
    }
    let pieces: Vec<&[u8]> = json.chunks(chunk(budget)).collect();
    let n = pieces.len() as u32;
    let (v, h) = super::protocol::mark(r);
    Ok(pieces
        .into_iter()
        .enumerate()
        .map(|(i, d)| Part {
            k: k.into(),
            key: key.into(),
            v,
            h: h.clone(),
            i: i as u32,
            n,
            d: base64::engine::general_purpose::STANDARD.encode(d),
        })
        .collect())
}

struct Assembly {
    n: u32,
    got: BTreeMap<u32, Vec<u8>>,
    bytes: usize,
    started: Instant,
}

/// Records arriving in parts, by (kind, key, v, hash).
#[derive(Default)]
pub struct Pending(BTreeMap<(String, String, u64, String), Assembly>);

impl Pending {
    /// Drops records whose parts didn't all arrive within `TIMEOUT` of the first; returns how many.
    pub fn expire(&mut self, now: Instant) -> usize {
        let before = self.0.len();
        self.0
            .retain(|_, a| now.duration_since(a.started) < TIMEOUT);
        before - self.0.len()
    }

    /// Takes part `p`: the whole record (kind, key, record) once its last part is in, None while parts
    /// are missing, Err for a part that can't belong to a record under `CAP`.
    pub fn add(
        &mut self,
        p: Part,
        budget: usize,
        now: Instant,
    ) -> Result<Option<(String, String, Value)>, String> {
        let what = format!("{} {} part {} of {}", p.k, p.key, p.i, p.n);
        if p.n < 2 || p.i >= p.n || p.n as usize > CAP.div_ceil(chunk(budget)) {
            return Err(format!("{what}: no such part"));
        }
        let d = base64::engine::general_purpose::STANDARD
            .decode(&p.d)
            .map_err(|e| format!("{what}: {e}"))?;
        if d.len() > chunk(budget) {
            return Err(format!("{what}: part too long"));
        }
        let id = (p.k, p.key, p.v, p.h);
        if !self.0.contains_key(&id) && self.0.len() >= PENDING {
            return Err(format!(
                "{what}: {PENDING} records already arriving in parts"
            ));
        }
        let a = self.0.entry(id.clone()).or_insert_with(|| Assembly {
            n: p.n,
            got: BTreeMap::new(),
            bytes: 0,
            started: now,
        });
        if a.n != p.n {
            return Err(format!("{what}: other parts said {} parts", a.n));
        }
        a.bytes += d.len();
        if let Some(old) = a.got.insert(p.i, d) {
            a.bytes -= old.len(); // a repeated part replaces itself
        }
        if a.bytes > CAP {
            self.0.remove(&id);
            return Err(format!("{what}: record over {CAP} bytes"));
        }
        if (a.got.len() as u32) < a.n {
            return Ok(None);
        }
        let a = self.0.remove(&id).expect("entry");
        let json: Vec<u8> = a.got.into_values().flatten().collect();
        let r: Value = serde_json::from_slice(&json).map_err(|e| format!("{what}: {e}"))?;
        let (k, key, v, h) = id;
        if super::protocol::mark(&r) != (v, h) {
            return Err(format!(
                "{k} {key}: the record doesn't match its parts' version and hash"
            ));
        }
        Ok(Some((k, key, r)))
    }
}

#[cfg(test)]
#[path = "parts_tests.rs"]
mod tests;
