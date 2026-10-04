use super::*;
use crate::sync::protocol::Session;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

#[test]
fn the_tag_is_stable_per_vault_and_is_not_the_vault_id() {
    let v = "a".repeat(64);
    assert_eq!(tag(&v), tag(&v));
    assert_eq!(tag(&v).len(), 16);
    assert!(!v.contains(&tag(&v)));
    assert_ne!(tag(&v), tag(&"b".repeat(64)));
}

/// Handshakes a dialer holding `a` with a listener holding `b`; returns (dialer, listener) results.
fn shake(a: Key, b: Key) -> (Result<(), String>, Result<(), String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let listener = std::thread::spawn(move || handshake(&mut l.accept().unwrap().0, &b, false));
    let dialer = handshake(&mut TcpStream::connect(addr).unwrap(), &a, true);
    (dialer, listener.join().unwrap())
}

#[test]
fn only_macs_holding_the_vault_key_get_past_the_handshake() {
    assert_eq!(shake([1; 32], [1; 32]), (Ok(()), Ok(())));
    let (d, l) = shake([1; 32], [2; 32]);
    assert!(d.unwrap_err().contains("doesn't hold this vault's key"));
    assert!(l.is_err());
}

#[test]
fn a_peer_that_is_not_ozen_fails_the_handshake() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let _ = s.write_all(&[0xAB; 64]); // a nonce and a made-up proof
        std::thread::sleep(Duration::from_secs(1));
    });
    assert!(handshake(&mut TcpStream::connect(addr).unwrap(), &[1; 32], true).is_err());
}

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

fn folder(lines: &[Value], tags: Value) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let rows: String = lines.iter().map(|r| r.to_string() + "\n").collect();
    std::fs::write(d.path().join("lines.jsonl"), rows).unwrap();
    std::fs::write(d.path().join("tags.json"), tags.to_string()).unwrap();
    d
}

/// A Mac syncing `dir` with its vault's other Macs on loopback only; counts its connections.
fn mac(dir: &Path, vault: &str, key: Key, peers: Arc<AtomicUsize>) -> Local {
    let (dir, v) = (PathBuf::from(dir), vault.to_string());
    start(vault, key, IfKind::LoopbackV4, move |s| {
        peers.fetch_add(1, Ordering::SeqCst);
        let mut session = Session::new([9; 32], &v);
        let _ = talk(s, |f| {
            at(&dir, || match f {
                None => session.hello(),
                Some(f) => session.receive(f),
            })
        });
    })
    .unwrap()
}

/// (lines by id, tags) in `dir`.
fn data(dir: &Path) -> (Vec<Value>, Value) {
    let mut lines: Vec<Value> = std::fs::read_to_string(dir.join("lines.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    lines.sort_by_key(|r| r["id"].to_string());
    let tags = serde_json::from_slice(&std::fs::read(dir.join("tags.json")).unwrap()).unwrap();
    (lines, tags)
}

#[test]
fn two_macs_on_one_network_find_each_other_and_converge_with_no_relay() {
    let line = |id: &str, text: &str| json!({"id": id, "t": 1.0, "text": text, "v": 1});
    let a = folder(
        &[line("1@a", "from a")],
        json!({"1@a": {"v": 1, "val": "Dana"}}),
    );
    let b = folder(&[line("2@b", "from b")], json!({}));
    let other = folder(&[line("3@c", "another vault")], json!({}));
    let (vault, key) = ("v".repeat(64), [5; 32]);
    let counts: Vec<_> = (0..3).map(|_| Arc::new(AtomicUsize::new(0))).collect();
    let _ma = mac(a.path(), &vault, key, counts[0].clone());
    let _mb = mac(b.path(), &vault, key, counts[1].clone());
    let _mo = mac(other.path(), &"o".repeat(64), [6; 32], counts[2].clone());
    // no relay is configured or reachable: OZEN_SYNC_URL is unset and nothing listens for one
    let started = Instant::now();
    while data(a.path()) != data(b.path()) || data(a.path()).0.len() < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no convergence: {:?} vs {:?}",
            data(a.path()),
            data(b.path())
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let (lines, tags) = data(b.path());
    assert_eq!(lines.len(), 2);
    assert_eq!(tags["1@a"]["val"], "Dana");
    assert_eq!(
        counts[0].load(Ordering::SeqCst),
        1,
        "one connection per pair"
    );
    assert_eq!(counts[1].load(Ordering::SeqCst), 1);
    assert_eq!(
        counts[2].load(Ordering::SeqCst),
        0,
        "another vault is never connected"
    );
    assert_eq!(data(other.path()).0.len(), 1);
}
