use super::*;
use std::cell::RefCell;

const KEY: Key = {
    let mut k = [0; 32];
    let mut i = 0;
    while i < 32 {
        k[i] = i as u8;
        i += 1;
    }
    k
};

#[test]
fn ids_are_pinned_for_a_fixed_key() {
    // computed independently with Python's hashlib/hmac (RFC 5869, no salt)
    assert_eq!(
        vault_id(&KEY),
        "96e30ee41d949acadae54cca5765bad30e12e1539cf36207ddfd359431628c69"
    );
    assert_eq!(
        token(&KEY),
        "e9511b0a65cafbf6f5deb78fece4c4d2327a14e5963f35a3be927922c5b970e3"
    );
    assert_eq!(
        hex(&seal_key(&KEY)),
        "9b466a61cbc81fe78245d956eced4fed2eec6b15c97be91bf2822f22dc21f3f3"
    );
    let hex64 =
        |s: &str| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    assert!(hex64(&vault_id(&KEY)) && hex64(&token(&KEY)));
    assert_ne!(token(&KEY), hex(&seal_key(&KEY)));
    // what the relay recomputes from the bearer token alone
    assert_eq!(vault_id(&KEY), hex(&Sha256::digest(token(&KEY).as_bytes())));
}

#[test]
fn init_twice_keeps_the_key() {
    let stored: RefCell<Option<Vec<u8>>> = RefCell::new(None);
    let run = || {
        load_or_create(
            || Ok(stored.borrow().clone()),
            |k| {
                *stored.borrow_mut() = Some(k.to_vec());
                Ok(())
            },
        )
    };
    let first = run().unwrap();
    assert_ne!(first, [0; 32]);
    assert_eq!(run().unwrap(), first);
}

#[test]
fn a_wrong_sized_stored_key_is_an_error() {
    let r = load_or_create(|| Ok(Some(vec![1; 5])), |_| panic!("must not overwrite"));
    assert!(r.is_err());
}

/// Debug builds read a throwaway key from `OZEN_SYNC_KEY_FILE` (scripts/sync-dev.sh), never the
/// Keychain; release builds ignore it.
#[cfg(debug_assertions)]
#[test]
fn a_debug_build_takes_the_key_from_ozen_sync_key_file() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("key");
    std::fs::write(&f, [7u8; 32]).unwrap();
    // SAFETY: nextest runs each test in its own process
    unsafe { std::env::set_var("OZEN_SYNC_KEY_FILE", &f) };
    assert_eq!(stored().unwrap(), Some([7u8; 32]));
    assert_eq!(
        keychain().unwrap(),
        [7u8; 32],
        "and never makes a Keychain one"
    );
}

/// With `OZEN_SYNC_KEY_FILE` (debug builds), storing and forgetting use the file too: a dev sandbox's
/// rotate, join or off never writes or deletes the user's real Keychain key.
#[cfg(debug_assertions)]
#[test]
fn a_debug_build_stores_and_forgets_in_ozen_sync_key_file() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("key");
    // SAFETY: nextest runs each test in its own process
    unsafe { std::env::set_var("OZEN_SYNC_KEY_FILE", &f) };
    store(&[8; 32]).unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), [8u8; 32]);
    assert_eq!(stored().unwrap(), Some([8u8; 32]));
    forget().unwrap();
    assert!(!f.exists());
}
