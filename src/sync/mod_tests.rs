use super::*;

const VAULT: &str = "96e30ee41d949acadae54cca5765bad30e12e1539cf36207ddfd359431628c69";

#[test]
fn init_shows_only_the_short_vault_id() {
    for out in [
        report(VAULT, Some("wss://relay.example")),
        report(VAULT, None),
    ] {
        assert!(out.contains("vault 96e30ee4"), "{out}");
        assert!(
            !out.contains(&VAULT[..9]),
            "more than 8 chars of the id: {out}"
        );
    }
}

/// No format string in src/sync interpolates a vault id by name: anything shown goes through
/// key::short. (A heuristic: it catches `{vault}`, `{vault_id}` and `{self.vault}` captures.)
#[test]
fn no_source_in_sync_formats_a_full_vault_id() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sync");
    let mut found = vec![];
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        for (i, l) in std::fs::read_to_string(&p).unwrap().lines().enumerate() {
            // a line marked `vault-id: request path` builds the relay URL, which needs the full id
            // and is never shown (link.rs)
            if ["{vault}", "{vault_id}", "{self.vault}"]
                .iter()
                .any(|c| l.contains(c))
                && !l.contains("// vault-id: request path")
            {
                found.push(format!("{name}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    assert!(found.is_empty(), "{found:#?}");
}

#[test]
fn the_short_id_is_at_most_8_chars_whatever_it_is_given() {
    assert_eq!(key::short(VAULT), "96e30ee4");
    assert_eq!(key::short("abc"), "abc");
    assert_eq!(key::short("aaaaaaaé…"), "aaaaaaaé");
}

/// Puts the working directory back when dropped, even if the test panics first.
pub(super) struct Back(pub(super) std::path::PathBuf);
impl Drop for Back {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

/// `init` notes the vault in `.sync-vault`; once the key is gone from the Keychain, `init` refuses a
/// new one and says how to rejoin, and the other commands say so too (OFE-77).
#[test]
fn init_remembers_the_vault_and_refuses_a_new_key_once_it_is_lost() {
    let _kc = key::tests::throwaway();
    let d = tempfile::tempdir().unwrap();
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _back = Back(std::env::current_dir().unwrap());
    std::env::set_current_dir(d.path()).unwrap();
    assert!(init(None, Some(true)).is_ok(), "a first run makes the key");
    let k = key::stored().unwrap().expect("made");
    assert_eq!(
        key::remembered().as_deref(),
        Some(key::short(&key::vault_id(&k)))
    );
    key::forget().unwrap();
    let e = init(None, Some(true)).unwrap_err();
    assert!(e.contains("ozen sync join"), "{e}");
    assert_eq!(key::stored().unwrap(), None, "no new key");
    assert!(key::missing("first run").contains("ozen sync join"));
    std::fs::remove_file(key::VAULT).unwrap();
    assert_eq!(key::missing("first run"), "first run");
}
