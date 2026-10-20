use super::*;
use std::path::Path;

fn in_synced_folder<T>(f: impl FnOnce() -> T) -> T {
    let d = tempfile::tempdir().unwrap();
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    fs::write(run::ON, "").unwrap();
    fs::write(super::super::config::LAN_ONLY, "").unwrap(); // shareable: a LAN-only vault
    let r = f();
    std::env::set_current_dir(back).unwrap();
    r
}

#[test]
fn rotate_stores_a_new_key_keeps_sync_on_and_shares_it() {
    in_synced_folder(|| {
        let old = [1; 32];
        let mut stored = None;
        let mut paired = 0;
        let said = rotate_with(
            crate::sync::key::key(old),
            |k| {
                stored = Some(k.clone());
                Ok(())
            },
            || {
                paired += 1;
                Ok("pairing code copied to the clipboard".into())
            },
        )
        .unwrap();
        let new = stored.expect("a new key is stored");
        assert_ne!(*new, old);
        assert_eq!(paired, 1, "the new pair code goes on the clipboard");
        assert!(
            run::configured() && !Path::new(restore::PAUSED).exists(),
            "sync stays on"
        );
        assert!(said.contains(key::short(&key::vault_id(&new))), "{said}");
        assert!(
            !said.contains(&key::vault_id(&new)),
            "only the short vault id is shown"
        );
    });
}

#[test]
fn rotate_without_sync_set_up_changes_nothing() {
    let d = tempfile::tempdir().unwrap();
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    let r = rotate_with(
        crate::sync::key::key([1; 32]),
        |_| panic!("no key may be stored"),
        || panic!("no code"),
    );
    std::env::set_current_dir(back).unwrap();
    assert!(r.unwrap_err().contains("isn't set up"));
}

#[test]
fn a_failed_store_leaves_sync_paused_on_the_old_key() {
    in_synced_folder(|| {
        let e = rotate_with(
            crate::sync::key::key([1; 32]),
            |_| Err("Keychain: denied".into()),
            || panic!("no code"),
        )
        .unwrap_err();
        assert!(e.contains("denied"));
        assert!(
            Path::new(restore::PAUSED).exists(),
            "nothing syncs on a half-rotated vault"
        );
    });
}

#[test]
fn a_pair_code_failure_after_the_key_rotated_says_so() {
    in_synced_folder(|| {
        let e = rotate_with(
            crate::sync::key::key([1; 32]),
            |_| Ok(()),
            || Err("pasteboard busy".into()),
        )
        .unwrap_err();
        assert!(
            e.contains("the key rotated") && e.contains("ozen sync pair"),
            "{e}"
        );
        assert!(
            run::configured() && !Path::new(restore::PAUSED).exists(),
            "sync is on, new key"
        );
    });
}

#[test]
fn a_vault_with_no_relay_and_not_lan_only_isnt_rotated() {
    in_synced_folder(|| {
        fs::remove_file(super::super::config::LAN_ONLY).unwrap();
        // SAFETY: nextest runs each test in its own process
        unsafe { std::env::remove_var("OZEN_SYNC_URL") };
        let e = rotate_with(
            crate::sync::key::key([1; 32]),
            |_| panic!("no key may be stored"),
            || panic!("no code"),
        );
        assert!(e.unwrap_err().contains("no relay"));
        assert!(!Path::new(restore::PAUSED).exists(), "nothing paused");
    });
}
