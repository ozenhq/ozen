//! The vault key: 32 random bytes in the login Keychain (never in the ozen folder), and what the
//! relay (ozenhq/sync src/auth) and the encryption derive from it.
use hkdf::Hkdf;
use security_framework::passwords::{get_generic_password, set_generic_password};
use security_framework::random::SecRandom;
use sha2::{Digest, Sha256};

const SERVICE: &str = "ozen-sync";
const ACCOUNT: &str = "vault-key";

/// A 32-byte key: the vault key or one derived from it. Wiped from memory when dropped, and not `Copy`,
/// so it isn't silently duplicated (OFE-60); clone it only where a second owner is needed.
pub type Key = zeroize::Zeroizing<[u8; 32]>;

/// `bytes` as a key.
pub fn key(bytes: [u8; 32]) -> Key {
    zeroize::Zeroizing::new(bytes)
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(b.len() * 2); // no reallocation leaves key hex behind
    for x in b {
        let _ = write!(s, "{x:02x}");
    }
    s
}

fn hkdf(key: &Key, info: &str) -> Key {
    let mut out = self::key([0; 32]);
    Hkdf::<Sha256>::new(None, &key[..])
        .expand(info.as_bytes(), &mut out[..])
        .expect("32 bytes is a valid HKDF-SHA256 length");
    out
}

/// The bearer token the relay checks: an HKDF output separate from the encryption key.
pub fn token(key: &Key) -> zeroize::Zeroizing<String> {
    zeroize::Zeroizing::new(hex(&hkdf(key, "ozen-sync token")[..]))
}

/// The vault's name on the relay: hex(SHA-256(token)), 64 lowercase hex chars. The relay checks a
/// Mac's token by hashing it, so it stores nothing (ozenhq/sync OFE-26).
pub fn vault_id(key: &Key) -> String {
    hex(&Sha256::digest(token(key).as_bytes()))
}

/// The vault id as shown to people: its first 8 hex chars, enough to tell vaults apart. The full id is
/// what the relay sees for one user's Macs, so it never goes to output, logs or screenshots (OFE-80).
pub fn short(vault_id: &str) -> &str {
    vault_id
        .char_indices()
        .nth(8)
        .map_or(vault_id, |(i, _)| &vault_id[..i])
}

/// The key ops are sealed with; never sent anywhere.
pub fn seal_key(key: &Key) -> Key {
    hkdf(key, "ozen-sync seal")
}

/// The key two Macs on one network prove they share before syncing directly (local.rs). The relay
/// knows the token, so the LAN proof must not be derivable from it; this is a separate HKDF output.
pub fn lan_key(key: &Key) -> Key {
    hkdf(key, "ozen-sync lan")
}

/// The stored key, or a new random one saved through `write`. Running it again keeps the key.
pub fn load_or_create(
    read: impl FnOnce() -> Result<Option<Vec<u8>>, String>,
    write: impl FnOnce(&Key) -> Result<(), String>,
) -> Result<Key, String> {
    if let Some(k) = read()? {
        return from_bytes(k);
    }
    let k = random()?;
    write(&k)?;
    Ok(k)
}

/// A new random key.
pub fn random() -> Result<Key, String> {
    let mut k = key([0; 32]);
    SecRandom::default()
        .copy_bytes(&mut k[..])
        .map_err(|e| format!("random key: {e}"))?;
    Ok(k)
}

/// Stored bytes as a key; the bytes are wiped either way.
fn from_bytes(bytes: Vec<u8>) -> Result<Key, String> {
    let bytes = zeroize::Zeroizing::new(bytes);
    if bytes.len() != 32 {
        return Err("vault key in the Keychain is not 32 bytes".into());
    }
    let mut k = key([0; 32]);
    k.copy_from_slice(&bytes);
    Ok(k)
}

/// The vault key, from the login Keychain. Debug builds only: `OZEN_SYNC_KEY_FILE` names a file with a
/// throwaway key instead (scripts/sync-dev.sh), so dev sandboxes never touch the Keychain or join the
/// vault of the user's real Macs.
fn read() -> Result<Option<Vec<u8>>, String> {
    if let Some(f) = key_file() {
        return std::fs::read(&f)
            .map(Some)
            .map_err(|e| format!("OZEN_SYNC_KEY_FILE {}: {e}", f.to_string_lossy()));
    }
    read_at(SERVICE)
}

/// The throwaway key file a debug build uses instead of the Keychain (`read`), for reading, storing and
/// forgetting alike: a dev sandbox must never write the user's real key.
fn key_file() -> Option<std::ffi::OsString> {
    std::env::var_os("OZEN_SYNC_KEY_FILE").filter(|_| cfg!(debug_assertions))
}

/// The raw item under Keychain service `service` (tests use a throwaway one).
fn read_at(service: &str) -> Result<Option<Vec<u8>>, String> {
    match get_generic_password(service, ACCOUNT) {
        Ok(k) => Ok(Some(k)),
        Err(e) if e.code() == -25300 => Ok(None), // errSecItemNotFound
        Err(e) => Err(format!("Keychain: {e}")),
    }
}

/// A stored key's bytes as a key.
fn typed(k: Option<Vec<u8>>) -> Result<Option<Key>, String> {
    k.map(from_bytes).transpose()
}

/// The key in the login Keychain if `ozen sync init` or `join` stored one; never makes one.
pub fn stored() -> Result<Option<Key>, String> {
    typed(read()?)
}

/// `stored`, under Keychain service `service`.
#[cfg(test)]
pub fn stored_at(service: &str) -> Result<Option<Key>, String> {
    typed(read_at(service)?)
}

/// Saves `k` as the key in the login Keychain, replacing any other.
pub fn store(k: &Key) -> Result<(), String> {
    match key_file() {
        Some(f) => std::fs::write(&f, &k[..]).map_err(|e| format!("OZEN_SYNC_KEY_FILE: {e}")),
        None => store_at(SERVICE, k),
    }
}

/// `store`, under Keychain service `service`.
pub fn store_at(service: &str, k: &Key) -> Result<(), String> {
    set_generic_password(service, ACCOUNT, &k[..]).map_err(|e| format!("Keychain: {e}"))
}

/// Removes the key from the login Keychain (`ozen sync off`); fine if there was none.
pub fn forget() -> Result<(), String> {
    match key_file() {
        Some(f) => match std::fs::remove_file(&f) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("OZEN_SYNC_KEY_FILE: {e}"))
            }
            _ => Ok(()),
        },
        None => forget_at(SERVICE),
    }
}

/// `forget`, under Keychain service `service`.
pub fn forget_at(service: &str) -> Result<(), String> {
    match security_framework::passwords::delete_generic_password(service, ACCOUNT) {
        Err(e) if e.code() != -25300 => Err(format!("Keychain: {e}")), // -25300: errSecItemNotFound
        _ => Ok(()),
    }
}

/// The key in the login Keychain, made on first use.
pub fn keychain() -> Result<Key, String> {
    load_or_create(read, store)
}

#[cfg(test)]
#[path = "key_tests.rs"]
mod tests;
