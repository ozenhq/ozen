use super::*;
use crate::sync::apply::{Coalesced, received};
use serde_json::json;
use std::path::Path;

/// Runs `f` in a fresh ozen folder with sync set up: a line, the switches and a relay URL.
fn in_synced_folder<T>(f: impl FnOnce() -> T) -> T {
    let d = tempfile::tempdir().unwrap();
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    fs::write(
        "lines.jsonl",
        "{\"id\":\"1@a\",\"t\":1.0,\"text\":\"hi\",\"v\":1}\n",
    )
    .unwrap();
    for f in [run::ON, config::FILE, config::LAN_ONLY] {
        fs::write(f, "wss://relay.example").unwrap();
    }
    let r = f();
    std::env::set_current_dir(back).unwrap();
    r
}

#[test]
fn off_removes_the_key_and_the_switches_and_leaves_the_meetings() {
    in_synced_folder(|| {
        let lines = fs::read("lines.jsonl").unwrap();
        let mut forgot = 0;
        let said = off_with(|| {
            forgot += 1;
            Ok(())
        })
        .unwrap();
        assert_eq!(forgot, 1, "the Keychain key is removed");
        for f in [run::ON, config::FILE, config::LAN_ONLY] {
            assert!(!Path::new(f).exists(), "{f} is gone");
        }
        assert!(!run::configured(), "nothing starts `ozen sync run` again");
        assert_eq!(
            fs::read("lines.jsonl").unwrap(),
            lines,
            "meetings untouched"
        );
        assert!(said.contains("no copy of them is on any server"), "{said}");
    });
}

#[test]
fn after_off_a_frame_still_in_flight_merges_nothing() {
    in_synced_folder(|| {
        off_with(|| Ok(())).unwrap();
        let lines = fs::read("lines.jsonl").unwrap();
        let theirs = crate::merge::Synced {
            lines: vec![
                json!({"id": "2@b", "t": 2.0, "text": "late", "v": 1})
                    .as_object()
                    .unwrap()
                    .clone(),
            ],
            ..Default::default()
        };
        assert!(received(&theirs, &Coalesced::new(|| {})).is_err());
        assert_eq!(fs::read("lines.jsonl").unwrap(), lines);
    });
}

#[test]
fn a_keychain_error_is_reported_after_sync_is_already_off() {
    in_synced_folder(|| {
        let e = off_with(|| Err("Keychain: denied".into())).unwrap_err();
        assert!(
            e.contains("denied") && e.contains("run `ozen sync off` again"),
            "{e}"
        );
        assert!(
            !run::configured(),
            "sync is off even so: nothing restarts it"
        );
    });
}

/// The real login Keychain under a throwaway service. Ignored by default (Keychain access can prompt):
/// `cargo nextest run --release --run-ignored ignored-only real_keychain_forget`.
#[test]
#[ignore]
fn real_keychain_forget_removes_the_item_and_a_second_forget_is_fine() {
    let service = format!("ozen-sync-test-{}", std::process::id());
    crate::sync::key::store_at(&service, &crate::sync::key::key([4; 32])).unwrap();
    crate::sync::key::forget_at(&service).unwrap();
    assert_eq!(crate::sync::key::stored_at(&service).unwrap(), None);
    crate::sync::key::forget_at(&service).unwrap();
}

#[test]
fn after_off_the_next_launch_starts_no_sync() {
    in_synced_folder(|| {
        off_with(|| Ok(())).unwrap();
        let e = run::run().unwrap_err();
        assert!(e.contains("sync is off"), "{e}");
    });
}

#[test]
fn off_waits_for_a_batch_being_merged() {
    in_synced_folder(|| {
        let held = restore::merging().unwrap(); // a batch is mid-merge
        // same working directory: this test holds the CWD lock for the thread too
        let going = std::thread::spawn(|| off_with(|| Ok(())));
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!(Path::new(run::ON).exists(), "off didn't wait for the merge");
        drop(held);
        going.join().unwrap().unwrap();
        assert!(!Path::new(run::ON).exists());
    });
}

#[test]
fn turning_sync_on_again_after_off_merges_again() {
    in_synced_folder(|| {
        off_with(|| Ok(())).unwrap();
        crate::sync::turn_on().unwrap(); // what `ozen sync join` and `init` do
        assert!(run::configured() && !Path::new(restore::PAUSED).exists());
        let theirs = crate::merge::Synced {
            lines: vec![
                json!({"id": "2@b", "t": 2.0, "text": "after", "v": 1})
                    .as_object()
                    .unwrap()
                    .clone(),
            ],
            ..Default::default()
        };
        received(&theirs, &Coalesced::new(|| {})).unwrap();
        assert!(fs::read_to_string("lines.jsonl").unwrap().contains("2@b"));
    });
}
