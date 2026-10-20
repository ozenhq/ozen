//! The relay, in-process (link_tests.rs and its neighbours use it): ozenhq/sync's contract, kept
//! small. The relay's repo is private, so CI can't build the real one.
use super::*;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{SocketAddr, TcpListener};
use std::sync::atomic::AtomicUsize;
use std::sync::{Mutex, PoisonError, mpsc};
use tungstenite::handshake::server::{ErrorResponse, Request, Response};

/// The relay's contract (ozenhq/sync src/hub): `/v/<vault>` with `Bearer <token>` where vault =
/// hex(SHA-256(token)); binary frames go to the vault's other connections; `{"online":N}` on every
/// join and leave. In-process, since the relay's repo is private and CI can't build it.
pub(super) struct Relay {
    pub(super) addr: SocketAddr,
    stop: Arc<AtomicBool>,
    /// Connections accepted (handshake passed) and refused.
    pub(super) accepted: Arc<AtomicUsize>,
    pub(super) refused: Arc<AtomicUsize>,
    /// Every binary frame it forwarded, for checking that none carries plaintext (OFE-15).
    pub(super) frames: Arc<Mutex<Vec<Vec<u8>>>>,
    /// Connections open now, and the most ever open at once.
    pub(super) live: Arc<(AtomicUsize, AtomicUsize)>,
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
    pub(super) fn start() -> Relay {
        Relay::on("127.0.0.1:0".parse().unwrap())
    }

    /// On `addr` (a fixed port, to restart the "same" relay).
    #[allow(clippy::result_large_err)] // the handshake callback's signature is tungstenite's
    pub(super) fn on(addr: SocketAddr) -> Relay {
        let listener = TcpListener::bind(addr).unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, accepted, refused) = (
            Arc::new(AtomicBool::new(false)),
            Arc::new(AtomicUsize::new(0)),
            Arc::new(AtomicUsize::new(0)),
        );
        let vaults: Vaults = Arc::default();
        let frames: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let live: Arc<(AtomicUsize, AtomicUsize)> = Arc::default();
        let (st, acc, refu, log, lv) = (
            stop.clone(),
            accepted.clone(),
            refused.clone(),
            frames.clone(),
            live.clone(),
        );
        std::thread::spawn(move || {
            let mut next = 0;
            while !st.load(Ordering::SeqCst) {
                let Ok((s, _)) = listener.accept() else {
                    std::thread::sleep(Duration::from_millis(20));
                    continue;
                };
                s.set_nonblocking(false).unwrap();
                next += 1;
                let (id, st, vaults, acc, refu, log, lv) = (
                    next,
                    st.clone(),
                    vaults.clone(),
                    acc.clone(),
                    refu.clone(),
                    log.clone(),
                    lv.clone(),
                );
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
                    let open = lv.0.fetch_add(1, Ordering::SeqCst) + 1;
                    lv.1.fetch_max(open, Ordering::SeqCst);
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
                                log.lock()
                                    .unwrap_or_else(PoisonError::into_inner)
                                    .push(f.to_vec());
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
                    lv.0.fetch_sub(1, Ordering::SeqCst);
                    announce(&vaults, &vault);
                });
            }
        });
        Relay {
            addr,
            stop,
            accepted,
            refused,
            frames,
            live,
        }
    }

    pub(super) fn url(&self) -> String {
        format!("ws://{}", self.addr)
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(200)); // its threads notice and close their sockets
    }
}
