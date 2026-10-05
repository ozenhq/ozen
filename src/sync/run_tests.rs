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

fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ok() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn converge_then_follow_an_edit(on: impl Fn() -> IfKind) {
    let (a, b) = (
        folder(&[line("1@a", "from a")]),
        folder(&[line("2@b", "from b")]),
    );
    let key = [7; 32];
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
    let local = mac(a.path(), &[8; 32]);
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
