use super::*;

fn watch(ips: &[[u8; 4]]) -> (Watch, SystemTime, Instant) {
    let (wall, mono) = (SystemTime::now(), Instant::now());
    let ips = ips.iter().map(|o| IpAddr::from(*o)).collect();
    let wake = PathBuf::from("/nonexistent/.sync-wake");
    (
        Watch {
            wall,
            mono,
            ips,
            wake,
            woke: None,
        },
        wall,
        mono,
    )
}

#[test]
fn a_sleep_shows_as_the_wall_clock_running_ahead_of_the_monotonic_one() {
    let (mut w, wall, mono) = watch(&[[192, 168, 1, 5]]);
    let ips = w.ips.clone();
    // 2 s later on both clocks: awake all along
    assert!(!w.changed_at(
        wall + Duration::from_secs(2),
        mono + Duration::from_secs(2),
        ips.clone()
    ));
    // the wall clock moved an hour, the monotonic clock 2 s: the Mac slept in between
    let (wall, mono) = (
        wall + Duration::from_secs(3602),
        mono + Duration::from_secs(4),
    );
    assert!(w.changed_at(wall, mono, ips.clone()));
    // only once
    assert!(!w.changed_at(
        wall + Duration::from_secs(2),
        mono + Duration::from_secs(2),
        ips
    ));
}

#[test]
fn another_network_shows_as_other_addresses() {
    let (mut w, wall, mono) = watch(&[[192, 168, 1, 5]]);
    let other = vec![IpAddr::from([10, 0, 0, 7])];
    assert!(w.changed_at(
        wall + Duration::from_secs(2),
        mono + Duration::from_secs(2),
        other.clone()
    ));
    assert!(!w.changed_at(
        wall + Duration::from_secs(4),
        mono + Duration::from_secs(4),
        other
    ));
}

#[test]
fn small_clock_adjustments_are_not_a_sleep() {
    let (mut w, wall, mono) = watch(&[]);
    assert!(!w.changed_at(
        wall + Duration::from_secs(4),
        mono + Duration::from_secs(2),
        vec![]
    ));
}

#[test]
fn this_macs_addresses_are_read_and_steady() {
    let a = addrs();
    assert!(a.iter().all(|ip| !ip.is_loopback()));
    assert_eq!(a, addrs(), "two looks a moment apart agree");
}

#[test]
fn ozen_sync_wake_from_the_app_counts_once() {
    let d = tempfile::tempdir().unwrap();
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    let mut w = Watch::new();
    std::thread::sleep(Duration::from_millis(20));
    touch().unwrap(); // what Ozen.app runs on NSWorkspaceDidWakeNotification
    let first = w.changed();
    let again = w.changed();
    std::thread::sleep(Duration::from_millis(20));
    touch().unwrap();
    let second = w.changed();
    std::env::set_current_dir(back).unwrap();
    assert!(first && !again && second);
}
