//! Sealing sync frames: XChaCha20-Poly1305 under the seal key, the vault id as associated data, and
//! the plaintext padded to a fixed bucket, so the relay sees only a frame's bucket and timing.
//! Frame: 24-byte random nonce, then ciphertext of (u32 LE length, plaintext, zero padding), then
//! the 16-byte tag; the whole frame is exactly one bucket long.
use super::key::Key;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use security_framework::random::SecRandom;

const NONCE: usize = 24;
const TAG: usize = 16;
const LEN: usize = 4;
/// Sealed frame sizes; the largest stays under the relay's 64 KiB frame cap.
pub const BUCKETS: [usize; 4] = [1 << 10, 4 << 10, 16 << 10, 60 << 10];
/// The most plaintext one frame carries.
pub const MAX: usize = BUCKETS[3] - NONCE - TAG - LEN;

/// `plaintext` encrypted and padded into the smallest bucket it fits.
#[allow(dead_code)] // ponytail: used by the sync protocol (OFE-10)
pub fn seal(seal_key: &Key, vault_id: &str, plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let bucket = BUCKETS
        .into_iter()
        .find(|b| plaintext.len() <= b - NONCE - TAG - LEN)
        .ok_or_else(|| format!("frame too big: {} bytes, max {MAX}", plaintext.len()))?;
    let mut padded = Vec::with_capacity(bucket - NONCE - TAG);
    padded.extend_from_slice(&(plaintext.len() as u32).to_le_bytes());
    padded.extend_from_slice(plaintext);
    padded.resize(bucket - NONCE - TAG, 0);
    let mut nonce = [0; NONCE];
    SecRandom::default()
        .copy_bytes(&mut nonce)
        .map_err(|e| format!("random nonce: {e}"))?;
    let ct = XChaCha20Poly1305::new(seal_key.into())
        .encrypt(
            &XNonce::from(nonce),
            Payload {
                msg: &padded,
                aad: vault_id.as_bytes(),
            },
        )
        .map_err(|_| "seal failed")?;
    Ok([&nonce[..], &ct].concat())
}

/// The plaintext of a frame sealed by `seal` with the same key and vault id; any other frame errors.
#[allow(dead_code)] // ponytail: used by the sync protocol (OFE-10)
pub fn open(seal_key: &Key, vault_id: &str, frame: &[u8]) -> Result<Vec<u8>, String> {
    if !BUCKETS.contains(&frame.len()) {
        return Err(format!("frame is {} bytes, not a bucket size", frame.len()));
    }
    let (nonce, ct) = frame.split_at(NONCE);
    let nonce: [u8; NONCE] = nonce.try_into().expect("split at NONCE");
    let padded = XChaCha20Poly1305::new(seal_key.into())
        .decrypt(
            &XNonce::from(nonce),
            Payload {
                msg: ct,
                aad: vault_id.as_bytes(),
            },
        )
        .map_err(|_| "frame does not open: wrong key or vault, or altered")?;
    let n = u32::from_le_bytes(padded[..LEN].try_into().expect("4 bytes")) as usize;
    padded
        .get(LEN..LEN + n)
        .map(<[u8]>::to_vec)
        .ok_or_else(|| "frame length prefix past its end".into())
}

#[cfg(test)]
#[path = "seal_tests.rs"]
mod tests;
