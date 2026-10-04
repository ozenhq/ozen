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
