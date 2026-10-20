//! A wake or a network change reconnects at once (OFE-48); without one, the backoff runs as before.
use super::tests::{Relay, folder, ids, mac, wait_for};
use super::*;
use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

/// A free port with nothing listening on it.
fn free_port() -> std::net::SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

#[test]
fn after_a_wake_the_link_reconnects_and_syncs_within_2s_whatever_its_backoff() {
    let addr = free_port();
    let url = format!("ws://{addr}");
    let (a, b) = (folder(&["1@a"]), folder(&["2@b"]));
    let key = [21; 32];
    let la = mac(&url, a.path(), &crate::sync::key::key(key));
    // the relay is down: Mac a fails at about 0, 1, 3 and 7 s, then waits 4-8 s more
    std::thread::sleep(Duration::from_secs(8));
    let _relay = Relay::on(addr);
    let _lb = mac(&url, b.path(), &crate::sync::key::key(key));
    la.nudge(); // the lid opens
    let woke = Instant::now();
    wait_for(Duration::from_secs(10), "a's line at b", || {
        ids(b.path()).len() == 2
    });
    assert!(
        woke.elapsed() < Duration::from_secs(2),
        "took {:?}",
        woke.elapsed()
    );
}

/// A server that accepts and at once hangs up: every attempt is a quick failure. Returns its count of attempts.
fn hang_up_server() -> (String, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://{}", l.local_addr().unwrap());
    let n = Arc::new(AtomicUsize::new(0));
    let count = n.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            count.fetch_add(1, Ordering::SeqCst);
            drop(s);
        }
    });
    (url, n)
}

#[test]
fn without_a_wake_the_backoff_is_unchanged_and_each_wake_resets_it() {
    let d = folder(&[]);
    let (url, attempts) = hang_up_server();
    let _quiet = mac(&url, d.path(), &crate::sync::key::key([22; 32]));
    std::thread::sleep(Duration::from_secs(8));
    // 1 s doubling, jittered down by up to half: attempts at 0, 0.5-1, 1.5-3, 3.5-7 s, at most 5 in 8 s
    let quiet = attempts.load(Ordering::SeqCst);
    assert!((3..=5).contains(&quiet), "{quiet} attempts without a wake");

    let (url, attempts) = hang_up_server();
    let woken = mac(&url, d.path(), &crate::sync::key::key([23; 32]));
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(3) {
        woken.nudge(); // a wake every 300 ms: each one tries again at once
        std::thread::sleep(Duration::from_millis(300));
    }
    let nudged = attempts.load(Ordering::SeqCst);
    assert!(nudged >= 6, "{nudged} attempts with a wake every 300 ms");
}
