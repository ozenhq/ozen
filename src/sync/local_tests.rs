use super::*;
use crate::sync::protocol::Session;
use serde_json::{Value, json};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

#[test]
fn the_tag_is_stable_per_key_and_only_the_key_makes_it() {
    assert_eq!(tag(&[5; 32]), tag(&[5; 32]));
    assert_eq!(tag(&[5; 32]).len(), 16);
    assert_ne!(tag(&[5; 32]), tag(&[6; 32]));
}

/// Handshakes a dialer holding `a` with a listener holding `b`; returns (dialer, listener) results.
fn shake(a: Key, b: Key) -> (Result<(), String>, Result<(), String>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let listener =
        std::thread::spawn(move || handshake(&mut l.accept().unwrap().0, &b, false, HANDSHAKE));
    let dialer = handshake(&mut TcpStream::connect(addr).unwrap(), &a, true, HANDSHAKE);
    (dialer, listener.join().unwrap())
}

#[test]
fn only_macs_holding_the_vault_key_get_past_the_handshake() {
    assert_eq!(shake([1; 32], [1; 32]), (Ok(()), Ok(())));
    let (d, l) = shake([1; 32], [2; 32]);
    assert!(d.unwrap_err().contains("doesn't hold this vault's key"));
    assert!(l.is_err());
}

/// A peer at `addr` that runs `act` on its end of one connection.
fn peer(act: impl FnOnce(TcpStream) + Send + 'static) -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || act(l.accept().unwrap().0));
    addr
}

#[test]
fn a_peer_that_is_not_ozen_fails_the_handshake() {
    let addr = peer(|mut s| {
        let _ = s.write_all(&[0xAB; 64]); // a nonce and a made-up proof
        std::thread::sleep(Duration::from_secs(1));
    });
    let r = handshake(
        &mut TcpStream::connect(addr).unwrap(),
        &[1; 32],
        true,
        HANDSHAKE,
    );
    assert!(r.is_err());
}

#[test]
fn a_reflected_proof_fails() {
    // the peer echoes our nonce and then our own proof back, hoping it passes for theirs
    let addr = peer(|mut s| {
        let mut b = [0; 32];
        s.read_exact(&mut b).unwrap();
        s.write_all(&b).unwrap();
        s.read_exact(&mut b).unwrap();
        s.write_all(&b).unwrap();
        std::thread::sleep(Duration::from_secs(1));
    });
    let r = handshake(
        &mut TcpStream::connect(addr).unwrap(),
        &[1; 32],
        true,
        HANDSHAKE,
    );
    assert!(r.unwrap_err().contains("doesn't hold"));
}

#[test]
fn a_peer_trickling_bytes_cannot_stretch_the_handshake() {
    let addr = peer(|mut s| {
        for _ in 0..64 {
            if s.write_all(&[0]).is_err() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    });
    let started = Instant::now();
    let r = handshake(
        &mut TcpStream::connect(addr).unwrap(),
        &[1; 32],
        true,
        Duration::from_millis(400),
    );
    assert!(r.is_err());
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn frames_over_64_kib_are_refused_both_ways() {
    let addr = peer(|mut s| {
        let _ = s.write_all(&((MAX_FRAME as u32 + 1).to_be_bytes()));
        std::thread::sleep(Duration::from_secs(1));
    });
    let mut s = TcpStream::connect(addr).unwrap();
    assert!(read_frame(&mut s).unwrap_err().contains("over"));
    assert!(write_frame(&mut s, &vec![0; MAX_FRAME + 1]).is_err());
}

#[test]
fn connections_past_the_cap_are_closed_and_drop_stops_listening() {
    let local = start([3; 32], IfKind::LoopbackV4, |_| {}).unwrap();
    let addr = ("127.0.0.1", local.port());
    // each of these sits in its handshake, holding a slot
    let held: Vec<_> = (0..MAX_PEERS)
        .map(|_| TcpStream::connect(addr).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    let mut extra = TcpStream::connect(addr).unwrap();
    extra
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    assert_eq!(
        extra.read(&mut [0; 64]).unwrap_or(0),
        0,
        "closed with nothing sent"
    );
    drop(held);
    drop(local);
    std::thread::sleep(Duration::from_millis(300));
    assert!(TcpStream::connect(addr).is_err(), "no longer listening");
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

/// A Mac syncing `dir` with its vault's other Macs on loopback only; counts its connections. Its first
/// connection is hung up at once, as a dropped Wi-Fi would.
fn mac(dir: &Path, vault: &str, key: Key, peers: Arc<AtomicUsize>) -> Local {
    mac_on(IfKind::LoopbackV4, dir, vault, key, peers)
}

fn mac_on(on: IfKind, dir: &Path, vault: &str, key: Key, peers: Arc<AtomicUsize>) -> Local {
    let (dir, v) = (PathBuf::from(dir), vault.to_string());
    start(key, on, move |s| {
        if peers.fetch_add(1, Ordering::SeqCst) == 0 {
            return;
        }
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
    // the first connection was dropped, then redialed: two each, and no duplicates turn up later
    let seen = |i: usize| counts[i].load(Ordering::SeqCst);
    assert_eq!((seen(0), seen(1)), (2, 2));
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!((seen(0), seen(1)), (2, 2), "one connection per pair");
    assert_eq!(seen(2), 0, "another vault is never connected");
    assert_eq!(data(other.path()).0.len(), 1);
}

/// Over the Mac's real network interface (multicast on Wi-Fi or Ethernet, not loopback). Ignored by
/// default: it needs a connected network and the Local Network permission. Run by hand:
/// `OZEN_LAN_IF=en0 cargo nextest run --release --run-ignored only over_the_real_network`.
#[test]
#[ignore]
fn two_macs_converge_over_the_real_network() {
    let on = IfKind::Name(std::env::var("OZEN_LAN_IF").unwrap_or_else(|_| "en0".into()));
    let line = |id: &str, text: &str| json!({"id": id, "t": 1.0, "text": text, "v": 1});
    let a = folder(
        &[line("1@a", "from a")],
        json!({"1@a": {"v": 1, "val": "Dana"}}),
    );
    let b = folder(&[line("2@b", "from b")], json!({}));
    let (vault, key) = ("w".repeat(64), [8; 32]);
    let n = || Arc::new(AtomicUsize::new(1)); // no forced first hang-up here
    let _ma = mac_on(on.clone(), a.path(), &vault, key, n());
    let _mb = mac_on(on, b.path(), &vault, key, n());
    let started = Instant::now();
    while data(a.path()) != data(b.path()) || data(a.path()).0.len() < 2 {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no convergence over the network"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn only_this_networks_addresses_reach_the_handshake() {
    let ok = |a: &str| local_source(a.parse().unwrap());
    for a in [
        "127.0.0.1",
        "192.168.1.20",
        "10.0.0.4",
        "172.16.5.5",
        "169.254.3.3",
        "::1",
        "fe80::1",
        "fd12:3456::1",
        "::ffff:192.168.1.20",
    ] {
        assert!(ok(a), "{a} is local");
    }
    for a in [
        "8.8.8.8",
        "100.64.0.1",
        "172.32.0.1",
        "2001:4860::8888",
        "::ffff:8.8.8.8",
        "0.0.0.0",
    ] {
        assert!(!ok(a), "{a} is not local");
    }
}
