use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, PoisonError, mpsc};
use std::time::Instant;
use tungstenite::handshake::server::{ErrorResponse, Request, Response};

/// The relay's contract (ozenhq/sync src/hub): `/v/<vault>` with `Bearer <token>` where vault =
/// hex(SHA-256(token)); binary frames go to the vault's other connections; `{"online":N}` on every
/// join and leave. In-process, since the relay's repo is private and CI can't build it.
struct Relay {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    /// Connections accepted (handshake passed) and refused.
    accepted: Arc<AtomicUsize>,
    refused: Arc<AtomicUsize>,
}

type Vaults = Arc<Mutex<HashMap<String, Vec<(usize, mpsc::Sender<Message>)>>>>;

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Tells every connection in `vault` how many are online.
fn announce(vaults: &Vaults, vault: &str) {
    let v = vaults.lock().unwrap_or_else(PoisonError::into_inner);
    let peers = v.get(vault).map_or(&[][..], Vec::as_slice);
    for (_, tx) in peers {
        let _ = tx.send(Message::Text(
            json!({"online": peers.len()}).to_string().into(),
        ));
    }
}

impl Relay {
    fn start() -> Relay {
        Relay::on("127.0.0.1:0".parse().unwrap())
    }

    /// On `addr` (a fixed port, to restart the "same" relay).
    #[allow(clippy::result_large_err)] // the handshake callback's signature is tungstenite's
    fn on(addr: SocketAddr) -> Relay {
        let listener = TcpListener::bind(addr).unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, accepted, refused) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let vaults: Vaults = Arc::default();
        let (st, acc, refu) = (stop.clone(), accepted.clone(), refused.clone());
        std::thread::spawn(move || {
            let mut next = 0;
            while !st.load(Ordering::SeqCst) {
                let Ok((s, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                };
                s.set_nonblocking(false).unwrap();
                next += 1;
                let (id, st, vaults, acc, refu) =
                    (next, st.clone(), vaults.clone(), acc.clone(), refu.clone());
                std::thread::spawn(move || {
                    let mut vault = String::new();
                    let check =
                        |req: &Request, resp: Response| -> Result<Response, ErrorResponse> {
                            let v = req
                                .uri()
                                .path()
                                .strip_prefix("/v/")
                                .unwrap_or("")
                                .to_string();
                            let token = req
                                .headers()
                                .get("authorization")
                                .and_then(|h| h.to_str().ok())
                                .and_then(|h| h.strip_prefix("Bearer "))
                                .unwrap_or("");
                            if hex(&Sha256::digest(token.as_bytes())) != v {
                                let mut r = ErrorResponse::new(None);
                                *r.status_mut() = tungstenite::http::StatusCode::UNAUTHORIZED;
                                return Err(r);
                            }
                            vault = v;
                            Ok(resp)
                        };
                    let Ok(mut ws) = tungstenite::accept_hdr(s, check) else {
                        refu.fetch_add(1, Ordering::SeqCst);
                        return;
                    };
                    acc.fetch_add(1, Ordering::SeqCst);
                    ws.get_mut()
                        .set_read_timeout(Some(Duration::from_millis(50)))
                        .unwrap();
                    let (tx, rx) = mpsc::channel();
                    vaults
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .entry(vault.clone())
                        .or_default()
                        .push((id, tx));
                    announce(&vaults, &vault);
                    while !st.load(Ordering::SeqCst) {
                        match ws.read() {
                            Ok(Message::Binary(f)) => {
                                let v = vaults.lock().unwrap_or_else(PoisonError::into_inner);
                                for (other, tx) in v.get(&vault).into_iter().flatten() {
                                    if *other != id {
                                        let _ = tx.send(Message::Binary(f.clone()));
                                    }
                                }
                            }
                            Ok(Message::Close(_)) => break,
                            Ok(_) => {}
                            Err(tungstenite::Error::Io(e))
                                if matches!(
                                    e.kind(),
                                    ErrorKind::WouldBlock | ErrorKind::TimedOut
                                ) => {}
                            Err(_) => break,
                        }
                        let mut gone = false;
                        while let Ok(m) = rx.try_recv() {
                            gone |= ws.send(m).is_err();
                        }
                        if gone {
                            break;
                        }
                    }
                    let _ = ws.get_mut().shutdown(std::net::Shutdown::Both);
                    vaults
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .entry(vault.clone())
                        .or_default()
                        .retain(|(o, _)| *o != id);
                    announce(&vaults, &vault);
                });
            }
        });
        Relay {
            addr,
            stop,
            accepted,
            refused,
        }
    }

    fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(200)); // its threads notice and close their sockets
    }
}

/// Runs `f` with `dir` as the working directory, as ozen runs from its folder.
fn at<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    let _cwd = crate::CWD.lock().unwrap_or_else(PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(dir).unwrap();
    let r = f();
    std::env::set_current_dir(back).unwrap();
    r
}

fn line(id: &str) -> String {
    json!({"id": id, "t": 1.0, "text": format!("said {id}"), "v": 1}).to_string() + "\n"
}

fn folder(lines: &[&str]) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("lines.jsonl"),
        lines.iter().map(|i| line(i)).collect::<String>(),
    )
    .unwrap();
    d
}

fn ids(dir: &Path) -> Vec<String> {
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
fn mac(url: &str, dir: &Path, key: &Key) -> Link {
    let dir = PathBuf::from(dir);
    start(url, key, Coalesced::new(|| {}), move |step| at(&dir, step))
}

fn wait_for(limit: Duration, what: &str, mut ok: impl FnMut() -> bool) {
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
