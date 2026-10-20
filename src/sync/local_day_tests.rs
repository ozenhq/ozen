//! Macs whose UTC days differ, or change while they run, still find each other (OFE-83).
use super::*;

#[test]
fn macs_on_either_side_of_midnight_still_find_each_other() {
    let line = |id: &str| json!({"id": id, "t": 1.0, "text": "x", "v": 1});
    let a = folder(&[line("1@a")], json!({}));
    let b = folder(&[line("2@b")], json!({}));
    let (vault, key) = ("v".repeat(64), random().unwrap());
    let peers = Arc::new(AtomicUsize::new(0));
    // one Mac advertised at 23:59:50, the other looks at 00:00:30 the next day
    let _ma = mac_at(
        || 20_000 * 86_400 - 10,
        IfKind::LoopbackV4,
        a.path(),
        &vault,
        crate::sync::key::key(key),
        peers.clone(),
    );
    let _mb = mac_at(
        || 20_000 * 86_400 + 30,
        IfKind::LoopbackV4,
        b.path(),
        &vault,
        crate::sync::key::key(key),
        peers.clone(),
    );
    let started = Instant::now();
    while data(a.path()).0.len() < 2 || data(b.path()).0.len() < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "they never met"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(20_000 * 86_400);

#[test]
fn a_running_mac_advertises_a_new_tag_and_id_when_its_day_changes() {
    let line = |id: &str| json!({"id": id, "t": 1.0, "text": "x", "v": 1});
    let a = folder(&[line("1@a")], json!({}));
    let b = folder(&[line("2@b")], json!({}));
    let (vault, key) = ("v".repeat(64), random().unwrap());
    let peers = Arc::new(AtomicUsize::new(0));
    let clock = || CLOCK.load(Ordering::SeqCst);
    let ma = mac_at(
        clock,
        IfKind::LoopbackV4,
        a.path(),
        &vault,
        crate::sync::key::key(key),
        peers.clone(),
    );
    let before = ma.fullname.lock().unwrap().clone();
    CLOCK.store(20_002 * 86_400, Ordering::SeqCst); // two days on: a's first tag is too old to match
    std::thread::sleep(Duration::from_secs(2)); // a notices within a second
    assert_ne!(
        *ma.fullname.lock().unwrap(),
        before,
        "a new day, a new instance id"
    );
    // a Mac on that day finds a only by the tag a advertises now
    let _mb = mac_at(
        || 20_002 * 86_400,
        IfKind::LoopbackV4,
        b.path(),
        &vault,
        crate::sync::key::key(key),
        peers,
    );
    let started = Instant::now();
    while data(a.path()).0.len() < 2 || data(b.path()).0.len() < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "a never re-advertised"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}
