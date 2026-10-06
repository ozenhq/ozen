use super::*;
use serde_json::json;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::PoisonError;
use std::time::Instant;

pub(super) use super::relay_tests::Relay;

/// Runs `f` with `dir` as the working directory, as ozen runs from its folder.
pub(super) fn at<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let _cwd = crate::CWD.lock().unwrap_or_else(PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();
    let r = f();
    std::env::set_current_dir(back).unwrap();
    r
}

pub(super) fn line(id: &str) -> String {
    json!({"id": id, "t": 1.0, "text": format!("said {id}"), "v": 1}).to_string() + "\n"
}

pub(super) fn folder(lines: &[&str]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("lines.jsonl"),
        lines.iter().map(|i| line(i)).collect::<String>(),
    )
    .unwrap();
    d
}

pub(super) fn ids(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_to_string(dir.join("lines.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .map(|r| r["id"].as_str().unwrap_or_default().to_string())
        .collect();
    v.sort();
    v
}

/// A Mac syncing `dir` through the relay at `url`.
pub(super) fn mac(url: &str, dir: &Path, key: &Key) -> Link {
    let dir = PathBuf::from(dir);
    start(url, key, Coalesced::new(|| {}), move |step| at(&dir, step))
}

pub(super) fn wait_for(limit: Duration, what: &str, mut ok: impl FnMut() -> bool) {
    let started = Instant::now();
    while !ok() {
        assert!(started.elapsed() < limit, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn two_macs_converge_through_the_relay_and_a_later_edit_follows() {
    let relay = Relay::start();
    let (a, b) = (folder(&["1@a"]), folder(&["2@b"]));
    let key = [3; 32];
    let (_la, _lb) = (
        mac(&relay.url(), a.path(), &key),
        mac(&relay.url(), b.path(), &key),
    );
    wait_for(Duration::from_secs(20), "first exchange", || {
        ids(a.path()).len() == 2 && ids(b.path()).len() == 2
    });
    at(a.path(), || {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open("lines.jsonl")
            .unwrap();
        std::io::Write::write_all(&mut f, line("3@a").as_bytes()).unwrap();
    });
    wait_for(Duration::from_secs(20), "the later edit", || {
        ids(b.path()).contains(&"3@a".to_string())
    });
    let s = at(a.path(), status);
    assert_eq!(
        (s["connected"].clone(), s["online"].clone()),
        (json!(true), json!(2))
    );
}

#[test]
fn a_restarted_relay_is_reconnected_and_what_changed_meanwhile_arrives() {
    let relay = Relay::start();
    let addr = relay.addr;
    let (a, b) = (folder(&["1@a"]), folder(&["2@b"]));
    let key = [4; 32];
    let (_la, _lb) = (
        mac(&relay.url(), a.path(), &key),
        mac(&relay.url(), b.path(), &key),
    );
    wait_for(Duration::from_secs(20), "first exchange", || {
        ids(b.path()).len() == 2
    });
    drop(relay); // the relay restarts: both connections drop
    at(a.path(), || {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open("lines.jsonl")
            .unwrap();
        std::io::Write::write_all(&mut f, line("3@a").as_bytes()).unwrap();
    });
    std::thread::sleep(Duration::from_secs(2));
    let relay = Relay::on(addr);
    // backoff 1 s, 2 s, 4 s... (jittered): both are back well within 20 s, and say hello again
    wait_for(
        Duration::from_secs(20),
        "the edit made while the relay was down",
        || ids(b.path()).contains(&"3@a".to_string()),
    );
    assert!(relay.accepted.load(Ordering::SeqCst) >= 2);
}

#[test]
fn a_wrong_token_is_a_refusal_not_a_retry() {
    let relay = Relay::start();
    // a token that doesn't hash to the vault id asked for: the relay answers 401
    let (wrong_vault, token) = (key::vault_id(&[6; 32]), key::token(&[5; 32]));
    assert_eq!(
        connect(&relay.url(), &wrong_vault, &token).unwrap_err(),
        End::Refused("refused the key (401 Unauthorized)".into())
    );
    assert_eq!(relay.refused.load(Ordering::SeqCst), 1);
}

#[test]
fn a_refused_link_records_it_for_health_and_never_retries() {
    // a relay that refuses everything: its own token check against a vault no key here produces
    let relay = Relay::start();
    let d = folder(&[]);
    let refusing = format!("{}/x", relay.url()); // path /x/v/<vault> never matches /v/<vault>
    let dir = PathBuf::from(d.path());
    let _link = start(&refusing, &[7; 32], Coalesced::new(|| {}), move |s| {
        at(&dir, s)
    });
    wait_for(Duration::from_secs(10), "the refusal", || {
        at(d.path(), status)["refused"] == json!(true)
    });
    std::thread::sleep(Duration::from_secs(3)); // past the first backoff steps
    assert_eq!(
        relay.refused.load(Ordering::SeqCst),
        1,
        "no retry after a refusal"
    );
    let line = at(d.path(), health).expect("a health line");
    assert!(line.contains("refused the key"), "{line}");
}

#[test]
fn a_relay_that_never_answers_doesnt_hold_anything_up() {
    // accepts TCP, never speaks WebSocket
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}", silent.local_addr().unwrap());
    let d = folder(&["1@a"]);
    let dir = PathBuf::from(d.path());
    let t = Instant::now();
    let link = start(&url, &[8; 32], Coalesced::new(|| {}), move |s| at(&dir, s));
    assert!(
        t.elapsed() < Duration::from_millis(200),
        "start returns at once"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(at(d.path(), status)["connected"], json!(false));
    let t = Instant::now();
    drop(link);
    assert!(
        t.elapsed() < Duration::from_millis(50),
        "stopping doesn't wait on the hung attempt"
    );
}

/// Through the real relay (ozenhq/sync), not the stand-in above. Ignored by default: the relay's repo is
/// private. Run by hand with one listening on plain ws:
/// `BIND=127.0.0.1:8787 cargo run --release` in ozenhq/sync, then
/// `OZEN_RELAY_URL=ws://127.0.0.1:8787 cargo nextest run --release --run-ignored only -E 'test(real_relay)'`.
#[test]
#[ignore]
fn two_macs_converge_through_the_real_relay() {
    let url = std::env::var("OZEN_RELAY_URL").expect("OZEN_RELAY_URL");
    let (a, b) = (folder(&["1@a"]), folder(&["2@b"]));
    let key = [42; 32];
    let (_la, _lb) = (mac(&url, a.path(), &key), mac(&url, b.path(), &key));
    wait_for(Duration::from_secs(30), "first exchange", || {
        ids(a.path()).len() == 2 && ids(b.path()).len() == 2
    });
    at(a.path(), || {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open("lines.jsonl")
            .unwrap();
        std::io::Write::write_all(&mut f, line("3@a").as_bytes()).unwrap();
    });
    wait_for(Duration::from_secs(30), "the later edit", || {
        ids(b.path()).contains(&"3@a".to_string())
    });
    assert_eq!(at(a.path(), status)["online"], json!(2));
}

/// TLS as the app does it: `wss` through rustls with macOS's own certificate checks (rustls-platform-
/// verifier). Ignored by default (it needs the internet). Run by hand against any public WebSocket server,
/// e.g. `OZEN_WSS_URL=wss://echo.websocket.org cargo nextest run --release --run-ignored only -E 'test(public_wss)'`.
/// A server that isn't a relay answers the upgrade or refuses the path; either way the TLS handshake and the
/// certificate check have passed. An untrusted certificate fails before that.
#[test]
#[ignore]
fn wss_with_the_platforms_certificate_checks_reaches_a_public_wss_server() {
    let url = std::env::var("OZEN_WSS_URL").expect("OZEN_WSS_URL");
    match connect(&url, "0", "0") {
        Ok(_) => {}
        Err(End::Refused(e)) => eprintln!("TLS fine, path refused: {e}"),
        Err(End::Retry(e)) => {
            assert!(
                !e.contains("certificate") && !e.contains("tls") && !e.contains("TLS"),
                "{e}"
            );
            eprintln!("TLS fine, then: {e}");
        }
    }
    // and a certificate macOS doesn't trust is refused before any WebSocket talk
    let bad = connect("wss://self-signed.badssl.com", "0", "0").unwrap_err();
    assert!(
        matches!(&bad, End::Retry(e) if e.to_lowercase().contains("certificate")),
        "{bad:?}"
    );
}
