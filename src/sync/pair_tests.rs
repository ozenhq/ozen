use super::*;
use std::cell::RefCell;

/// Every byte different, so an order or offset bug shows.
const K_BYTES: [u8; 32] = {
    let mut k = [0; 32];
    let mut i = 0;
    while i < 32 {
        k[i] = (i as u8).wrapping_mul(37).wrapping_add(11);
        i += 1;
    }
    k
};
#[allow(non_snake_case)]
fn K() -> Key {
    crate::sync::key::key(K_BYTES)
}
const URL: &str = "wss://relay.example/ozen";

#[test]
fn a_code_round_trips_ignoring_dashes_spaces_and_case() {
    let c = code(&K(), URL);
    assert_eq!(parse(&c), Ok((K(), URL.into())));
    let sloppy = c.replace('-', " ").to_lowercase();
    assert_eq!(parse(&sloppy), Ok((K(), URL.into())));
}

#[test]
fn every_single_character_typo_is_refused() {
    let c = code(&K(), URL);
    for (i, ch) in c.char_indices().filter(|(_, ch)| *ch != '-') {
        let other = if ch == 'A' { 'B' } else { 'A' };
        let typo = format!("{}{other}{}", &c[..i], &c[i + 1..]);
        // a typo in the last group's spare bits can make the code undecodable: refused as well
        let e = parse(&typo).unwrap_err();
        assert!(
            e.contains("typo") || e.contains("not a pairing code"),
            "{i}: {e}"
        );
    }
    let typo = format!("{}{}", if c.starts_with('A') { "B" } else { "A" }, &c[1..]);
    assert!(
        parse(&typo).unwrap_err().contains("typo"),
        "a typo is named as one"
    );
}

#[test]
fn garbage_and_unsafe_relays_are_refused_and_rich_text_dashes_are_fine() {
    assert!(parse("hello").is_err());
    assert!(
        parse(&code(&K(), "http://evil.example")).is_err(),
        "only wss relays"
    );
    let pasted = code(&K(), URL).replace('-', "\u{2013}"); // en dashes from a rich-text paste
    assert_eq!(parse(&pasted), Ok((K(), URL.into())));
}

/// `join` with `existing` already stored and a relay that answers: what it wrote and saved, and its result.
fn run(
    c: &str,
    force: bool,
    existing: Option<Key>,
) -> (Option<Key>, Option<String>, Result<String, String>) {
    run_with(c, force, existing, true)
}

/// `run`, with a relay that answers or not.
fn run_with(
    c: &str,
    force: bool,
    existing: Option<Key>,
    relay_up: bool,
) -> (Option<Key>, Option<String>, Result<String, String>) {
    let (wrote, saved) = (RefCell::new(None), RefCell::new(None));
    let r = join(
        c,
        force,
        existing,
        |k| {
            *wrote.borrow_mut() = Some(k.clone());
            Ok(())
        },
        |u| {
            if !relay_up {
                return Err(format!("no sync relay answering at {u}"));
            }
            *saved.borrow_mut() = Some(u.to_string());
            Ok(u.to_string())
        },
    );
    (wrote.into_inner(), saved.into_inner(), r)
}

#[test]
fn pair_then_join_stores_the_key_and_relay() {
    let (wrote, saved, r) = run(&code(&K(), URL), false, None);
    assert_eq!((wrote, saved.as_deref()), (Some(K()), Some(URL)));
    assert!(r.is_ok());
    let (wrote, _, r) = run(&code(&K(), URL), false, Some(K()));
    assert!(
        r.is_ok() && wrote.is_none(),
        "the same key again: nothing to replace"
    );
}

#[test]
fn join_keeps_a_different_key_unless_forced() {
    let (wrote, saved, r) = run(
        &code(&K(), URL),
        false,
        Some(crate::sync::key::key([1; 32])),
    );
    assert!(r.unwrap_err().contains("--force"));
    assert_eq!((wrote, saved), (None, None), "nothing changed");
    let (wrote, _, r) = run(&code(&K(), URL), true, Some(crate::sync::key::key([1; 32])));
    assert!(r.is_ok());
    assert_eq!(wrote, Some(K()));
}

#[test]
fn nothing_key_derived_is_printed() {
    let (_, _, r) = run(&code(&K(), URL), false, None);
    let out = r.unwrap();
    let hex: String = K().iter().map(|b| format!("{b:02x}")).collect();
    for secret in [code(&K(), URL), hex, key::token(&K()), key::vault_id(&K())] {
        assert!(!out.contains(&secret), "{out}");
    }
}

#[test]
fn the_clipboard_item_is_concealed_and_forgotten_unless_replaced() {
    // a private pasteboard: the user's clipboard is never touched
    let pb = NSPasteboard::pasteboardWithUniqueName();
    let n = put(&pb, "CODE-1234").unwrap();
    let types: Vec<String> = pb.types().unwrap().iter().map(|t| t.to_string()).collect();
    assert!(types.contains(&CONCEALED.to_string()), "{types:?}");
    assert_eq!(
        pb.stringForType(unsafe { NSPasteboardTypeString })
            .map(|s| s.to_string()),
        Some("CODE-1234".into())
    );
    assert!(forget(&pb, n));
    assert_eq!(
        pb.stringForType(unsafe { NSPasteboardTypeString }),
        None,
        "gone"
    );

    // the user copied something else since: leave it
    let n = put(&pb, "CODE-1234").unwrap();
    pb.clearContents();
    pb.setString_forType(&NSString::from_str("mine"), unsafe {
        NSPasteboardTypeString
    });
    assert!(!forget(&pb, n));
    assert!(
        pb.stringForType(unsafe { NSPasteboardTypeString })
            .is_some()
    );
    // not in objc2-app-kit's bindings: free the private pasteboard on the pasteboard server
    let _: () = unsafe { objc2::msg_send![&*pb, releaseGlobally] };
    assert_eq!(CLEAR_AFTER, Duration::from_secs(120));
}

#[test]
fn a_relay_that_is_down_leaves_the_key_as_it_was() {
    for (existing, force) in [
        (None, false),
        (Some(crate::sync::key::key([1; 32])), true),
        (Some(K()), false),
    ] {
        let (wrote, saved, r) = run_with(&code(&K(), URL), force, existing, false);
        assert!(r.is_err());
        assert_eq!((wrote, saved), (None, None), "nothing replaced");
    }
}

#[test]
fn errors_print_nothing_key_derived_either() {
    let c = code(&K(), URL);
    let hex: String = K().iter().map(|b| format!("{b:02x}")).collect();
    let errors = [
        run(&c, false, Some(crate::sync::key::key([1; 32])))
            .2
            .unwrap_err(),
        run_with(&c, false, None, false).2.unwrap_err(),
        parse(&c[..c.len() - 3]).unwrap_err(),
    ];
    for e in errors {
        for secret in [
            c.clone(),
            hex.clone(),
            key::token(&K()),
            key::vault_id(&K()),
        ] {
            assert!(!e.contains(&secret), "{e}");
        }
    }
}

/// Against the real login Keychain, with a throwaway service name (never ozen's own item):
/// `cargo nextest run --release --run-ignored ignored-only real_keychain`.
#[test]
#[ignore]
fn real_keychain_join_round_trip() {
    let service = format!("ozen-sync-test-{}", std::process::id());
    let cleanup = || {
        let _ = security_framework::passwords::delete_generic_password(&service, "vault-key");
    };
    cleanup();
    let first = join(
        &code(&K(), URL),
        false,
        key::stored_at(&service).unwrap(),
        |k| key::store_at(&service, k),
        |u| Ok(u.to_string()),
    );
    let after_first = key::stored_at(&service);
    // a different key is refused, then --force replaces it
    let other: Key = crate::sync::key::key([7; 32]);
    let refused = join(
        &code(&other, URL),
        false,
        key::stored_at(&service).unwrap(),
        |k| key::store_at(&service, k),
        |u| Ok(u.to_string()),
    );
    let forced = join(
        &code(&other, URL),
        true,
        key::stored_at(&service).unwrap(),
        |k| key::store_at(&service, k),
        |u| Ok(u.to_string()),
    );
    let after_force = key::stored_at(&service);
    cleanup();
    assert!(first.is_ok(), "{first:?}");
    assert_eq!(after_first, Ok(Some(K())));
    assert!(refused.is_err());
    assert!(forced.is_ok());
    assert_eq!(after_force, Ok(Some(other)));
    assert_eq!(key::stored_at(&service), Ok(None), "cleaned up");
}

#[test]
fn a_lan_only_vault_pairs_with_no_relay_in_the_code() {
    // `pair` on a LAN-only Mac: the code carries the key and no relay URL
    let c = code(&K(), "");
    assert_eq!(parse(&c), Ok((K(), String::new())));
    let (wrote, saved, r) = run(&c, false, None);
    assert_eq!(wrote, Some(K()), "the key is stored");
    assert_eq!(
        saved,
        Some(String::new()),
        "no relay URL to save: the joining Mac goes LAN-only"
    );
    assert!(r.unwrap().contains("LAN only"));
}
