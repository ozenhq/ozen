use super::*;
use std::cell::RefCell;

const K: Key = [42; 32];
const URL: &str = "wss://relay.example/ozen";

#[test]
fn a_code_round_trips_ignoring_dashes_spaces_and_case() {
    let c = code(&K, URL);
    assert_eq!(parse(&c), Ok((K, URL.into())));
    let sloppy = c.replace('-', " ").to_lowercase();
    assert_eq!(parse(&sloppy), Ok((K, URL.into())));
}

#[test]
fn a_mistyped_code_fails_the_checksum() {
    let c = code(&K, URL);
    let i = c.len() / 2;
    let other = if &c[i..=i] == "A" { "B" } else { "A" };
    let typo = format!("{}{}{}", &c[..i], other, &c[i + 1..]);
    let e = parse(&typo).unwrap_err();
    assert!(
        e.contains("typo") || e.contains("not a pairing code"),
        "{e}"
    );
    assert!(parse("hello").is_err());
    assert!(
        parse(&code(&K, "http://evil.example")).is_err(),
        "only wss relays"
    );
}

/// `join` with `existing` already stored: what it wrote and saved, and its result.
fn run(
    c: &str,
    force: bool,
    existing: Option<Key>,
) -> (Option<Key>, Option<String>, Result<String, String>) {
    let (wrote, saved) = (RefCell::new(None), RefCell::new(None));
    let r = join(
        c,
        force,
        existing,
        |k| {
            *wrote.borrow_mut() = Some(*k);
            Ok(())
        },
        |u| {
            *saved.borrow_mut() = Some(u.to_string());
            Ok(u.to_string())
        },
    );
    (wrote.into_inner(), saved.into_inner(), r)
}

#[test]
fn pair_then_join_stores_the_key_and_relay() {
    let (wrote, saved, r) = run(&code(&K, URL), false, None);
    assert_eq!((wrote, saved.as_deref()), (Some(K), Some(URL)));
    assert!(r.is_ok());
    let (wrote, _, r) = run(&code(&K, URL), false, Some(K));
    assert!(
        r.is_ok() && wrote.is_none(),
        "the same key again: nothing to replace"
    );
}

#[test]
fn join_keeps_a_different_key_unless_forced() {
    let (wrote, saved, r) = run(&code(&K, URL), false, Some([1; 32]));
    assert!(r.unwrap_err().contains("--force"));
    assert_eq!((wrote, saved), (None, None), "nothing changed");
    let (wrote, _, r) = run(&code(&K, URL), true, Some([1; 32]));
    assert!(r.is_ok());
    assert_eq!(wrote, Some(K));
}

#[test]
fn nothing_key_derived_is_printed() {
    let (_, _, r) = run(&code(&K, URL), false, None);
    let out = r.unwrap();
    let hex: String = K.iter().map(|b| format!("{b:02x}")).collect();
    for secret in [code(&K, URL), hex, key::token(&K), key::vault_id(&K)] {
        assert!(!out.contains(&secret), "{out}");
    }
}

#[test]
fn the_clipboard_item_is_concealed_and_forgotten_unless_replaced() {
    // a private pasteboard: the user's clipboard is never touched
    let pb = NSPasteboard::pasteboardWithUniqueName();
    let n = put(&pb, "CODE-1234");
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
    let n = put(&pb, "CODE-1234");
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
