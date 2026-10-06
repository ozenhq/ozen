use super::*;

/// What the panel footer says: nothing when idle or for a small exchange, the counts for a big one.
#[test]
fn the_footer_shows_only_a_big_exchange_in_progress() {
    assert_eq!(line(&Value::Null), None, "idle");
    assert_eq!(
        line(&json!({"mac": "Studio", "done": 3, "total": 150})),
        None,
        "small"
    );
    assert_eq!(
        line(&json!({"mac": "Studio", "done": 201, "total": 201})),
        None,
        "done"
    );
    assert_eq!(
        line(&json!({"mac": "Studio", "done": 420, "total": 1300})).as_deref(),
        Some("Syncing with Studio: 420 / 1,300")
    );
    assert_eq!(
        line(&json!({"mac": "", "done": 0, "total": 1_234_567})).as_deref(),
        Some("Syncing with another Mac: 0 / 1,234,567")
    );
}

/// Records missing here, newer there or different at the same version are coming; the rest aren't.
#[test]
fn coming_counts_what_the_other_mac_will_send() {
    let id = |k: &str| ("lines".to_string(), k.to_string());
    let m = |v: u64, h: &str| (v, h.to_string());
    let ours = BTreeMap::from([
        (id("same"), m(2, "a")),
        (id("older"), m(1, "a")),
        (id("newer"), m(3, "a")),
        (id("differ"), m(2, "a")),
    ]);
    let theirs = BTreeMap::from([
        (id("same"), m(2, "a")),
        (id("older"), m(2, "b")),
        (id("newer"), m(2, "b")),
        (id("differ"), m(2, "c")),
        (id("missing"), m(1, "x")),
    ]);
    assert_eq!(coming(&ours, &theirs), 3); // older, differ, missing
}

/// A session notes a big exchange as it goes and removes the note when the last record merged; a
/// small one writes nothing, and a stale note (a dropped connection) is not reported.
#[test]
fn a_big_exchange_is_noted_until_done() {
    let d = tempfile::tempdir().unwrap();
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    let now = crate::sync::status::now();
    let mut p = Progress::default();
    p.heard("Studio");
    p.expect(50);
    let small = std::path::Path::new(FILE).exists();
    p.expect(300);
    p.got(120);
    let midway = read_at(now);
    let stale = read_at(now + STALE_SECS);
    p.got(180);
    let done = std::path::Path::new(FILE).exists();
    std::env::set_current_dir(back).unwrap();
    assert!(!small, "a small exchange writes nothing");
    assert_eq!(
        midway,
        Some(json!({"mac": "Studio", "done": 120, "total": 300}))
    );
    assert_eq!(stale, None, "a stopped exchange isn't shown");
    assert!(!done, "the note goes when the last record merged");
}
