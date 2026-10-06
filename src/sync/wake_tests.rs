use super::*;

fn watch(ips: &[[u8; 4]]) -> (Watch, SystemTime, Instant) {
    let (wall, mono) = (SystemTime::now(), Instant::now());
    let ips = ips.iter().map(|o| IpAddr::from(*o)).collect();
    (Watch { wall, mono, ips }, wall, mono)
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
