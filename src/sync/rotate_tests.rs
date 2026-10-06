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
            old,
            |k| {
                stored = Some(*k);
                Ok(())
            },
            || {
                paired += 1;
                Ok("pairing code copied to the clipboard".into())
            },
        )
        .unwrap();
        let new = stored.expect("a new key is stored");
        assert_ne!(new, old);
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
        [1; 32],
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
            [1; 32],
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
