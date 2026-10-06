//! What a sync frame carries and how: a message (`Msg`) as JSON, deflated, after a version byte
//! (version.rs), sealed (seal.rs). Carrying frames is the connection's job; protocol.rs decides what to say.
use super::key::Key;
use super::parts;
use super::seal;
use super::summaries::Entry;
use super::version::{Read, Versions};
use flate2::{Compression, read::DeflateDecoder, write::DeflateEncoder};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Read as _;

/// Raw JSON per frame: deflated, it stays under `seal::MAX` even when nothing compresses.
pub(super) const BUDGET: usize = seal::MAX - 256;
#[derive(Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub(super) enum Msg {
    /// The sender's 256 bucket hashes, 16 bytes each, base64, and the newest protocol version it speaks
    /// (version.rs; a hello from before announcing meant 1).
    Buckets {
        h: String,
        #[serde(default = "one")]
        max: u8,
    },
    /// Part `part` (of `parts`) of summary `id`: (kind, key, v, hash) for the sender's records in buckets
    /// `b`.
    Summary {
        id: u64,
        part: u32,
        parts: u32,
        s: Vec<Entry>,
        b: Vec<u8>,
    },
    /// Records for the receiver to merge: (kind, key, record).
    Records { r: Vec<(String, String, Value)> },
    /// One part of a record too big for one frame (parts.rs).
    Part(parts::Part),
}

fn one() -> u8 {
    1
}

/// Message `m` as a frame of protocol `version`.
pub(super) fn encode(key: &Key, vault: &str, version: u8, m: &Msg) -> Result<seal::Sealed, String> {
    let mut z = DeflateEncoder::new(vec![version], Compression::default());
    serde_json::to_writer(&mut z, m).map_err(|e| e.to_string())?;
    let plain = z.finish().map_err(|e| e.to_string())?;
    seal::seal(key, vault, &plain)
}

/// The message in `frame`: Err for one that doesn't open or parse (or is older than `versions` still
/// reads), Ok(None) for a newer version's.
pub(super) fn decode(
    key: &Key,
    vault: &str,
    versions: &Versions,
    frame: &[u8],
) -> Result<Option<Msg>, String> {
    let plain = seal::open(key, vault, frame)?;
    match plain.first().map(|v| (*v, versions.read(*v))) {
        Some((_, Read::Yes)) => {}
        Some((_, Read::Newer)) => return Ok(None),
        Some((v, Read::Older)) if v > 0 => {
            return Err(format!(
                "frame from an older ozen (protocol {v}, this one reads {} and up): update ozen on that Mac",
                versions.oldest()
            ));
        }
        v => return Err(format!("frame with protocol version {:?}", v.map(|v| v.0))),
    }
    // A sender's JSON is at most BUDGET per frame; anything inflating past that is no frame of ours.
    let mut json = vec![];
    DeflateDecoder::new(&plain[1..])
        .take(BUDGET as u64 + 1024)
        .read_to_end(&mut json)
        .map_err(|e| format!("frame does not inflate: {e}"))?;
    serde_json::from_slice(&json)
        .map(Some)
        .map_err(|e| format!("frame is not a message: {e}"))
}

/// The version byte of a frame that opens (for messages; 0 if it doesn't).
pub(super) fn frame_version(key: &Key, vault: &str, frame: &[u8]) -> u8 {
    seal::open(key, vault, frame)
        .ok()
        .and_then(|p| p.first().copied())
        .unwrap_or(0)
}
