use super::*;
use crate::sync::apply::Coalesced;
use crate::sync::protocol::Session;
use std::path::Path;

const NOW: u64 = 1_790_000_000;
const DAY: u64 = 86400;

/// Runs `f` in folder `dir`, as `ozen` runs in the ozen folder.
fn at<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();
    let r = f();
    std::env::set_current_dir(back).unwrap();
    r
}

/// A folder with sync on and a runner holding the lock, the relay reporting `online` Macs.
fn running(online: u64) -> (tempfile::TempDir, fs::File) {
    let d = tempfile::tempdir().unwrap();
    fs::write(d.path().join(super::super::run::ON), "").unwrap();
    let lock = fs::File::create(d.path().join(".sync.lock")).unwrap();
    lock.try_lock().unwrap();
    fs::write(
        d.path().join(super::super::link::STATUS),
        json!({"connected": true, "online": online, "error": null, "refused": false}).to_string(),
    )
    .unwrap();
    (d, lock)
}

/// Keys sorted, as without serde_json's preserve_order (the pre-push hook runs both ways).
fn snapshot(name: &str, v: Value) {
    insta::with_settings!({sort_maps => true}, { insta::assert_json_snapshot!(name, v) });
}

#[test]
fn status_when_sync_was_never_set_up() {
    let d = tempfile::tempdir().unwrap();
    snapshot(
        "status_when_sync_was_never_set_up",
        at(d.path(), || status_at(NOW)),
    );
}

#[test]
fn status_alone_on_the_relay() {
    let (d, _lock) = running(1);
    snapshot("status_alone_on_the_relay", at(d.path(), || status_at(NOW)));
}

#[test]
fn status_with_two_macs_online() {
    let (d, _lock) = running(2);
    at(d.path(), || {
        seen("a1b2c3d4", "Desk", NOW - 60).unwrap();
        seen("e5f6a7b8", "Old laptop", NOW - 9 * DAY).unwrap();
    });
    snapshot(
        "status_with_two_macs_online",
        at(d.path(), || status_at(NOW)),
    );
}

#[test]
fn a_hello_records_the_mac_that_said_it() {
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let mut sa = Session::with([7; 32], "vault", Coalesced::new(|| {}));
    let mut sb = Session::with([7; 32], "vault", Coalesced::new(|| {}));
    let hello = at(a.path(), || sa.hello()).unwrap();
    at(b.path(), || sb.receive(hello[0].as_ref())).unwrap();
    let heard = at(b.path(), macs);
    let (name, when) = &heard[crate::crdt::device()];
    assert_eq!(name, &super::name());
    assert!(now() - when < 60);
    assert!(
        at(a.path(), macs).is_empty(),
        "saying hello records nothing"
    );
}

#[test]
fn health_speaks_up_only_after_seven_days() {
    let (d, _lock) = running(1);
    let lines = |days: u64| {
        at(d.path(), || {
            seen("a1b2c3d4", "Desk", NOW - days * DAY).unwrap();
            health_at(NOW)
        })
    };
    assert!(lines(6).is_empty());
    assert_eq!(
        lines(7),
        [
            "Sync: this Mac last synced with Desk 7 days ago; Macs sync only while both are on and online at the same time"
        ]
    );
    fs::remove_file(d.path().join(super::super::run::ON)).unwrap();
    assert!(
        at(d.path(), || health_at(NOW)).is_empty(),
        "nothing while sync is off"
    );
}
