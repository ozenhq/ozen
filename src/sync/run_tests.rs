use super::*;
use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Instant;

/// Runs `f` with `dir` as the working directory, as ozen runs from its folder.
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

fn line(id: &str, text: &str) -> Value {
    json!({"id": id, "t": 1.0, "text": text, "v": 1})
}

fn folder(lines: &[Value]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let rows: String = lines.iter().map(|r| r.to_string() + "\n").collect();
    fs::write(d.path().join("lines.jsonl"), rows).unwrap();
    d
}

/// Line ids in `dir`, sorted.
fn ids(dir: &Path) -> Vec<String> {
    let mut ids: Vec<String> = fs::read_to_string(dir.join("lines.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|r| r["id"].as_str().unwrap_or_default().to_string())
        .collect();
    ids.sort();
    ids
}

/// A Mac running `serve` for `dir` on loopback, as `ozen sync run` does on the network.
fn mac(dir: &Path, key: &Key) -> Local {
    mac_on(IfKind::LoopbackV4, dir, key)
}

fn mac_on(on: IfKind, dir: &Path, key: &Key) -> Local {
    let dir = PathBuf::from(dir);
    serve(key, on, Coalesced::new(|| {}), move |step| at(&dir, step)).unwrap()
}

fn wait_for(what: &str, ok: impl FnMut() -> bool) {
    wait_within(Duration::from_secs(30), what, ok);
}

fn wait_within(limit: Duration, what: &str, mut ok: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ok() {
        assert!(started.elapsed() < limit, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn converge_then_follow_an_edit(on: impl Fn() -> IfKind) {
    let (a, b) = (
        folder(&[line("1@a", "from a")]),
        folder(&[line("2@b", "from b")]),
    );
    let key = super::local::random().unwrap();
    let (_ma, _mb) = (mac_on(on(), a.path(), &key), mac_on(on(), b.path(), &key));
    wait_for("first exchange", || {
        ids(a.path()).len() == 2 && ids(b.path()).len() == 2
    });
    // the transcriber appends a line on Mac a while connected: the tick sends it, no new hello
    at(a.path(), || {
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open("lines.jsonl")
            .unwrap();
        std::io::Write::write_all(
            &mut f,
            format!("{}\n", line("3@a", "said later")).as_bytes(),
        )
        .unwrap();
    });
    wait_for("the later edit", || {
        ids(b.path()).contains(&"3@a".to_string())
    });
    assert_eq!(ids(a.path()), ids(b.path()));
}

#[test]
fn two_macs_converge_and_a_later_edit_follows_without_reconnecting() {
    converge_then_follow_an_edit(|| IfKind::LoopbackV4);
}

/// Over the Mac's real network interface (multicast on Wi-Fi or Ethernet). Ignored by default: it
/// needs a network and the Local Network permission. Run by hand:
/// `OZEN_LAN_IF=en0 cargo nextest run --release --run-ignored only -E 'test(runners_over_the_real_network)'`.
#[test]
#[ignore]
fn two_runners_over_the_real_network_converge_and_follow_an_edit() {
    let i = std::env::var("OZEN_LAN_IF").unwrap_or_else(|_| "en0".into());
    converge_then_follow_an_edit(|| IfKind::Name(i.clone()));
}

#[test]
fn a_stopped_mac_stops_advertising() {
    let a = folder(&[]);
    let local = mac(a.path(), &super::local::random().unwrap());
    let browser = ServiceDaemon::new().unwrap();
    browser.disable_interface(IfKind::All).unwrap();
    browser.enable_interface(IfKind::LoopbackV4).unwrap();
    let found = browser.browse(&local::service_type()).unwrap();
    let mut seen = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    while seen.is_none() && Instant::now() < deadline {
        if let Ok(ServiceEvent::ServiceResolved(r)) = found.recv_timeout(Duration::from_millis(500))
            && r.get_port() == local.port()
        {
            seen = Some(r.get_fullname().to_string());
        }
    }
    let name = seen.expect("advertised while running");
    drop(local);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(Instant::now() < deadline, "still advertised after stopping");
        if let Ok(ServiceEvent::ServiceRemoved(_, n)) =
            found.recv_timeout(Duration::from_millis(500))
            && n == name
        {
            break;
        }
    }
    let _ = browser.shutdown();
}

#[test]
fn status_starts_it_only_when_sync_is_on_none_runs_and_not_just_started() {
    let d = tempfile::tempdir().unwrap();
    at(d.path(), || {
        assert!(!wanted(), "sync is off: nothing starts");
        assert!(!Path::new(ASKED).exists());
        fs::write(ON, "").unwrap();
        assert!(wanted());
        assert!(Path::new(ASKED).exists());
        assert!(!wanted(), "started a moment ago: not again every poll");
        fs::remove_file(STARTED).unwrap();
        let running = File::create(LOCK).unwrap();
        running.lock().unwrap();
        assert!(!wanted(), "one is running");
        drop(running);
        assert!(wanted());
    });
}

#[test]
fn it_stops_seconds_after_the_app_stops_asking_or_sync_is_turned_off() {
    let d = tempfile::tempdir().unwrap();
    at(d.path(), || {
        fs::write(ON, "").unwrap();
        assert!(!still_wanted(), "never asked");
        let asked = File::create(ASKED).unwrap();
        assert!(still_wanted());
        let quiet = std::time::SystemTime::now() - IDLE - Duration::from_secs(1);
        asked.set_modified(quiet).unwrap();
        assert!(!still_wanted(), "the app stopped polling");
        File::create(ASKED).unwrap();
        fs::remove_file(ON).unwrap();
        assert!(!still_wanted(), "sync turned off");
    });
}

#[test]
fn a_folder_that_ran_init_before_sync_on_existed_counts_as_on() {
    let d = tempfile::tempdir().unwrap();
    at(d.path(), || {
        fs::write(crate::sync::config::FILE, "wss://relay.example").unwrap();
        assert!(wanted());
    });
}

/// Backdates `f`'s modified time by `ago`.
fn backdate(f: &str, ago: Duration) {
    let t = std::time::SystemTime::now() - ago;
    File::options()
        .write(true)
        .open(f)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

#[test]
fn restarts_back_off_from_30s_doubling_to_10_minutes() {
    let secs: Vec<u64> = (0..8).map(|n| retry_after(n).as_secs()).collect();
    assert_eq!(secs, [30, 60, 120, 240, 480, 600, 600, 600]);
    let d = tempfile::tempdir().unwrap();
    at(d.path(), || {
        fs::write(ON, "").unwrap();
        assert!(wanted(), "first start");
        // it died at once, three times in a row: each restart waits twice as long
        for (n, wait) in [(1u64, 30), (2, 60), (3, 120)] {
            assert_eq!(failed_starts(), n as u32);
            backdate(STARTED, Duration::from_secs(wait - 1));
            assert!(!wanted(), "start {n}: too early at {}s", wait - 1);
            backdate(STARTED, Duration::from_secs(wait));
            assert!(wanted(), "start {n}: due at {wait}s");
        }
    });
}

#[test]
fn a_runner_that_stayed_up_and_was_killed_comes_back_at_the_next_poll() {
    let d = tempfile::tempdir().unwrap();
    at(d.path(), || {
        fs::write(ON, "").unwrap();
        assert!(wanted());
        fs::write(ERROR, "an old error").unwrap();
        stayed_up(); // a minute in
        assert_eq!(failed_starts(), 0);
        assert!(!Path::new(ERROR).exists());
        // killed seconds later: the lock is free and nothing waits
        assert!(wanted());
    });
}

#[test]
fn health_speaks_only_when_the_app_wants_sync_and_none_runs() {
    let d = tempfile::tempdir().unwrap();
    at(d.path(), || {
        File::create(ASKED).unwrap();
        assert_eq!(health(), None, "sync isn't configured");
        fs::write(ON, "").unwrap();
        assert!(wanted()); // the app's poll started one; it died with an error
        fs::write(ERROR, "no vault key here: run `ozen sync init` first").unwrap();
        let line = health().expect("configured, wanted, not running");
        assert!(line.contains("isn't running (no vault key here"), "{line}");
        assert!(line.contains("every 30s"), "{line}");
        let runner = File::create(LOCK).unwrap();
        runner.lock().unwrap();
        assert_eq!(health(), None, "it runs");
        drop(runner);
        backdate(ASKED, IDLE + Duration::from_secs(1));
        assert_eq!(health(), None, "the app quit: not wanted, not an error");
    });
}

#[test]
fn a_denied_local_network_permission_is_named_other_send_errors_are_not() {
    let denied = local_network(Err(std::io::Error::from_raw_os_error(65))).unwrap_err();
    assert!(
        denied.contains("Privacy & Security > Local Network"),
        "{denied}"
    );
    let offline = std::io::Error::from_raw_os_error(51); // ENETUNREACH: no network
    assert_eq!(local_network(Err(offline)), Ok(()));
    assert_eq!(local_network(Ok(12)), Ok(()));
}

/// The real probe, on a Mac whose Local Network access is known. Ignored by default: CI's macOS runner
/// denies it (there the probe gets EHOSTUNREACH, which is how this check was confirmed). Run by hand:
/// `OZEN_LOCAL_NETWORK=allowed|denied cargo nextest run --release --run-ignored only -E 'test(real_probe)'`.
#[test]
#[ignore]
fn the_real_probe_matches_this_macs_local_network_setting() {
    let allowed = std::env::var("OZEN_LOCAL_NETWORK").as_deref() != Ok("denied");
    assert_eq!(local_network(probe()).is_ok(), allowed);
}

#[test]
fn sync_runs_at_background_priority() {
    // nextest runs each test in its own process, so this lowers only this test
    assert!(!is_background());
    background().unwrap();
    assert!(is_background());
}

/// Kills the CPU burners when the test ends, however it ends.
struct Burners(Vec<std::process::Child>);

impl Drop for Burners {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[test]
fn an_exchange_at_background_priority_completes_with_half_the_cores_busy() {
    // normal-priority processes on half the cores, as during a meeting with a busy call and transcriber;
    // not all of them: the suite runs tests in parallel, and on a Mac already saturated by other work a
    // background-priority exchange against every core starved for minutes (load average 76-157)
    let cores = std::thread::available_parallelism().map_or(8, std::num::NonZero::get);
    let _load = Burners(
        (0..cores.div_ceil(2))
            .map(|_| {
                std::process::Command::new("/usr/bin/yes")
                    .stdout(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect(),
    );
    background().unwrap();
    let (a, b) = (
        folder(&[line("1@a", "from a")]),
        folder(&[line("2@b", "from b")]),
    );
    let key = super::local::random().unwrap();
    let (_ma, _mb) = (mac(a.path(), &key), mac(b.path(), &key));
    // background priority may wait on a busy Mac; what matters is that it gets through
    wait_within(Duration::from_secs(120), "an exchange under load", || {
        ids(a.path()).len() == 2 && ids(b.path()).len() == 2
    });
}

#[path = "run_edits_tests.rs"]
mod edits;
