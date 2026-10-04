//! Records grouped into 256 buckets by a hash of their id, and one 128-bit hash per bucket over its
//! records' ids, versions and content hashes. Two Macs compare the 256 hashes first (one frame) and
//! summarize only the buckets that differ, so Macs already in sync exchange almost nothing however
//! many records they hold.
use sha2::{Digest, Sha256};

pub const N: usize = 256;
/// Bytes per bucket hash.
const H: usize = 16;

/// The bucket of record (kind, key).
pub fn of(kind: &str, key: &str) -> u8 {
    Sha256::digest(format!("{kind}\0{key}").as_bytes())[0]
}

/// The 256 bucket hashes (16 bytes each, in bucket order) over records given as (kind, key, v, hash),
/// in a fixed order (sorted by kind, then key), so every Mac with the same records gets the same hashes.
pub fn hashes<'a>(marks: impl IntoIterator<Item = (&'a str, &'a str, u64, &'a str)>) -> Vec<u8> {
    let mut h: Vec<Sha256> = (0..N).map(|_| Sha256::new()).collect();
    for (kind, key, v, hash) in marks {
        h[of(kind, key) as usize].update(format!("{kind}\0{key}\0{v}\0{hash}\n").as_bytes());
    }
    h.into_iter()
        .flat_map(|x| x.finalize()[..H].to_vec())
        .collect()
}

/// The buckets whose hashes differ; every bucket when `theirs` isn't a full set of hashes.
pub fn differing(ours: &[u8], theirs: &[u8]) -> Vec<u8> {
    (0..N)
        .filter(|&i| {
            theirs.len() != N * H || ours[i * H..(i + 1) * H] != theirs[i * H..(i + 1) * H]
        })
        .map(|i| i as u8)
        .collect()
}

#[cfg(test)]
#[path = "buckets_tests.rs"]
mod tests;
