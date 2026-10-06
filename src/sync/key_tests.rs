use super::*;
use std::cell::RefCell;

/// A throwaway keychain for this test process, unlocked; every Keychain call in tests goes there
/// (`test_keychain`). Gone, file and all, when dropped.
pub(crate) fn throwaway() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let path = d.path().join("test.keychain-db");
    security_framework::os::macos::keychain::CreateOptions::new()
        .password(TEST_PASSWORD)
        .create(&path)
        .unwrap();
    // SAFETY: nextest runs each test in its own process
    unsafe { std::env::set_var("OZEN_TEST_KEYCHAIN", &path) };
    d
}

#[test]
fn tests_never_fall_back_to_the_login_keychain() {
    // SAFETY: as in `throwaway`
    unsafe { std::env::remove_var("OZEN_TEST_KEYCHAIN") };
    let e = stored().unwrap_err();
    assert!(e.contains("never use the login Keychain"), "{e}");
    assert!(store(&key(BYTES)).is_err() && forget().is_err() && keychain(None).is_err());
}

#[test]
fn the_keychain_key_is_made_once_kept_and_forgotten() {
    let _kc = throwaway();
    assert_eq!(stored().unwrap(), None);
    let k = keychain(None).unwrap();
    assert_eq!(stored().unwrap(), Some(k.clone()));
    assert_eq!(keychain(None).unwrap(), k, "made once");
    store(&key(BYTES)).unwrap();
    assert_eq!(stored().unwrap(), Some(key(BYTES)), "replaced");
    forget().unwrap();
    assert_eq!(stored().unwrap(), None);
    forget().unwrap(); // fine when there's none
}

const BYTES: [u8; 32] = {
    let mut k = [0; 32];
    let mut i = 0;
    while i < 32 {
        k[i] = i as u8;
        i += 1;
    }
    k
};

#[allow(non_snake_case)]
fn KEY() -> Key {
    key(BYTES)
}

#[test]
fn ids_are_pinned_for_a_fixed_key() {
    // computed independently with Python's hashlib/hmac (RFC 5869, no salt)
    assert_eq!(
        vault_id(&KEY()),
        "96e30ee41d949acadae54cca5765bad30e12e1539cf36207ddfd359431628c69"
    );
    assert_eq!(
        *token(&KEY()),
        "e9511b0a65cafbf6f5deb78fece4c4d2327a14e5963f35a3be927922c5b970e3"
    );
    assert_eq!(
        hex(&seal_key(&KEY())[..]),
        "9b466a61cbc81fe78245d956eced4fed2eec6b15c97be91bf2822f22dc21f3f3"
    );
    let hex64 =
        |s: &str| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    assert!(hex64(&vault_id(&KEY())) && hex64(&token(&KEY())));
    assert_ne!(*token(&KEY()), hex(&seal_key(&KEY())[..]));
    // what the relay recomputes from the bearer token alone
    assert_eq!(
        vault_id(&KEY()),
        hex(&Sha256::digest(token(&KEY()).as_bytes()))
    );
}

#[test]
fn init_twice_keeps_the_key() {
    let stored: RefCell<Option<Vec<u8>>> = RefCell::new(None);
    let run = || {
        load_or_create(
            None,
            || Ok(stored.borrow().clone()),
            |k| {
                *stored.borrow_mut() = Some(k.to_vec());
                Ok(())
            },
        )
    };
    let first = run().unwrap();
    assert_ne!(first, key([0; 32]));
    assert_eq!(run().unwrap(), first);
}

#[test]
fn a_wrong_sized_stored_key_is_an_error() {
    let r = load_or_create(
        None,
        || Ok(Some(vec![1; 5])),
        |_| panic!("must not overwrite"),
    );
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
    assert_eq!(stored().unwrap(), Some(key([7u8; 32])));
    assert_eq!(
        keychain(None).unwrap(),
        key([7u8; 32]),
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
    store(&key([8; 32])).unwrap();
    assert_eq!(std::fs::read(&f).unwrap(), [8u8; 32]);
    assert_eq!(stored().unwrap(), Some(key([8u8; 32])));
    forget().unwrap();
    assert!(!f.exists());
}

/// The vault key and every key derived from it are wiped from memory when dropped (OFE-60).
#[test]
fn keys_are_wiped_when_dropped() {
    fn wiped_on_drop<T: zeroize::ZeroizeOnDrop>(_: &T) {}
    let k = key([7; 32]);
    wiped_on_drop(&k);
    wiped_on_drop(&seal_key(&k));
    wiped_on_drop(&lan_key(&k));
    let mut k = key([7; 32]);
    zeroize::Zeroize::zeroize(&mut k);
    assert_eq!(*k, [0; 32]);
}

/// No raw `[u8; 32]` holds key bytes in sync's code: keys are `Key` (OFE-60). A line that holds other
/// bytes says so with `not a key`.
#[test]
fn no_raw_key_arrays_in_sync_code() {
    let mut found = vec![];
    for e in std::fs::read_dir("src/sync").unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") || name == "key.rs" {
            continue;
        }
        for (i, l) in std::fs::read_to_string(&p).unwrap().lines().enumerate() {
            let bare: String = l.chars().filter(|c| !c.is_whitespace()).collect();
            if (bare.contains("[u8;32]") || bare.contains("[0u8;32]")) && !l.contains("not a key") {
                found.push(format!("{name}:{}: {l}", i + 1));
            }
        }
    }
    assert!(found.is_empty(), "{found:#?}");
}

/// A key gone from the Keychain on a Mac that was in a vault errors with how to rejoin and makes no
/// new key; a first run (nothing remembered) still makes one, and a stored key loads either way (OFE-77).
#[test]
fn a_lost_key_asks_to_rejoin_instead_of_making_a_new_vault() {
    let e = load_or_create(
        Some("96e30ee4"),
        || Ok(None),
        |_| panic!("must not make a key"),
    )
    .unwrap_err();
    assert!(
        e.contains("vault 96e30ee4") && e.contains("ozen sync join"),
        "{e}"
    );
    let mut made = 0;
    load_or_create(
        None,
        || Ok(None),
        |_| {
            made += 1;
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(made, 1, "a first run makes the key");
    let k = load_or_create(
        Some("96e30ee4"),
        || Ok(Some(vec![7; 32])),
        |_| panic!("kept"),
    )
    .unwrap();
    assert_eq!(k, key([7; 32]));
}

/// `init` remembers the vault by its short id only, and after that a missing key is a lost one.
#[test]
fn init_remembers_the_vault_so_a_lost_key_is_noticed() {
    let d = tempfile::tempdir().unwrap();
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    assert_eq!(remembered(), None, "a first run");
    remember(&key([7; 32])).unwrap();
    let id = vault_id(&key([7; 32]));
    assert_eq!(remembered().as_deref(), Some(short(&id)));
    assert!(
        !std::fs::read_to_string(VAULT).unwrap().contains(&id[..9]),
        "never the full id"
    );
    let r = load_or_create(
        remembered().as_deref(),
        || Ok(None),
        |_| panic!("no new key"),
    );
    std::env::set_current_dir(back).unwrap();
    assert!(r.unwrap_err().contains("ozen sync join"));
}

/// On a (throwaway) Keychain: a key deleted there while the vault is remembered stays gone; init
/// errors and puts no new key in (OFE-77).
#[test]
fn a_deleted_keychain_key_is_not_replaced() {
    let _kc = throwaway();
    let k = keychain(None).unwrap();
    let v = short(&vault_id(&k)).to_string();
    forget().unwrap();
    assert!(keychain(Some(&v)).unwrap_err().contains("ozen sync join"));
    assert_eq!(stored().unwrap(), None, "no new key was made");
}
