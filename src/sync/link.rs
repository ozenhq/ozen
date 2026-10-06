//! The relay connection (ozenhq/sync): `ozen sync run` holds one WebSocket to `<relay>/v/<vault id>` with the
//! bearer token (key.rs) and runs the summary protocol (protocol.rs) over it, as local.rs does with Macs on
//! the same network. The relay forwards each binary frame to the vault's other Macs and keeps nothing, so
//! this connection is the only way changes reach a Mac elsewhere: it comes up on its own, reconnects with
//! jittered backoff after any drop, and stops only when the relay refuses the key. The relay's text frame
//! `{"online":N}` says how many Macs are connected; a rise means someone new to say hello to.
//! What the link knows is kept in `STATUS` for the rest of ozen (`ozen health`, later `ozen sync status`).
use super::apply::Coalesced;
use super::key::{self, Key};
use super::protocol::Session;
use super::seal::Sealed;
use serde_json::{Value, json};
use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Connector, Message, WebSocket};

/// `{"connected": bool, "online": N, "error": "...", "refused": bool}`, rewritten as it changes.
pub const STATUS: &str = ".sync-link.json";
/// Connecting and the WebSocket handshake each give up after this, so a relay that accepts and never
/// answers costs a retry, not a hung link.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// How often an idle connection checks for local edits to send (as local.rs's connections do).
const TICK: Duration = Duration::from_secs(2);
/// Why a connection was dropped after a wake or a network change (start reconnects at once).
const WOKE: &str = "woke from sleep or changed networks";
/// A relay that stops reading for this long is dropped and redialed.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Reconnect after this, doubling to `BACKOFF_MAX`, each wait jittered down by up to half so Macs
/// dropped together by a relay restart don't all come back in the same instant.
const BACKOFF: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(300);

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;
/// One protocol step on the connection's session; returns the frames to send.
type Step<'a> = dyn FnMut(&mut Session) -> Result<Vec<Sealed>, String> + 'a;

/// Why a connection attempt or connection ended.
#[derive(Debug, PartialEq)]
enum End {
    /// The relay said no to this key (401/403): retrying can't help until the user re-pairs.
    Refused(String),
    /// Anything else (network, relay restart, timeout): try again after the backoff.
    Retry(String),
}

/// The running link; dropping it stops it (an attempt in progress gives up within `HANDSHAKE`).
pub struct Link {
    stop: Arc<AtomicBool>,
    /// Tests stand in for a wake with it; in use, wake.rs notices wakes and network changes itself.
    #[cfg(test)]
    nudge: Arc<AtomicBool>,
}

#[cfg(test)]
impl Link {
    /// Reconnect now, as after a wake or a network change.
    pub fn nudge(&self) {
        self.nudge.store(true, Ordering::SeqCst);
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn write_status(connected: bool, online: u64, error: Option<&str>, refused: bool) {
    let v = json!({"connected": connected, "online": online, "error": error, "refused": refused});
    let tmp = format!("{STATUS}.{}.tmp", std::process::id());
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(tmp, STATUS);
    }
}

/// What `STATUS` says, or Null when no link ran here.
pub fn status() -> Value {
    std::fs::read(STATUS)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

/// One line for `ozen health` when the relay refused this Mac's key; a passing drop isn't news.
pub fn health() -> Option<String> {
    let s = status();
    let e = s.get("error")?.as_str()?;
    (s.get("refused") == Some(&json!(true)))
        .then(|| format!("Sync relay {e}: this Mac's vault key isn't accepted; pair it again"))
}

/// The TCP stream under a WebSocket, to set its timeouts.
fn tcp(ws: &mut Ws) -> &mut TcpStream {
    match ws.get_mut() {
        MaybeTlsStream::Plain(s) => s,
        MaybeTlsStream::Rustls(s) => s.get_mut(),
        _ => unreachable!("only plain and rustls streams are built"),
    }
}

/// Opens `<url>/v/<vault>` with the token. `url` is a checked relay URL (config::check: wss, or ws to this Mac).
fn connect(url: &str, vault: &str, token: &str) -> Result<Ws, End> {
    let retry = |e: &dyn std::fmt::Display| End::Retry(e.to_string());
    // the relay's request path needs the full id; it's never shown (errors below don't include it)
    let mut req = format!("{url}/v/{vault}") // vault-id: request path
        .into_client_request()
        .map_err(|e| retry(&e))?;
    let auth = format!("Bearer {token}").parse().map_err(|e| retry(&e))?;
    req.headers_mut().insert("authorization", auth);
    let host = req
        .uri()
        .host()
        .unwrap_or_default()
        .trim_matches(['[', ']'])
        .to_string();
    let tls = req.uri().scheme_str() == Some("wss");
    let port = req.uri().port_u16().unwrap_or(if tls { 443 } else { 80 });
    let addrs: Vec<_> = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| retry(&e))?
        .collect();
    let stream = addrs
        .iter()
        .find_map(|a| TcpStream::connect_timeout(a, HANDSHAKE).ok())
        .ok_or_else(|| End::Retry(format!("can't reach {host}:{port}")))?;
    stream
        .set_read_timeout(Some(HANDSHAKE))
        .map_err(|e| retry(&e))?;
    stream
        .set_write_timeout(Some(HANDSHAKE))
        .map_err(|e| retry(&e))?;
    let connector = if tls {
        use rustls_platform_verifier::ConfigVerifierExt;
        let config = rustls::ClientConfig::with_platform_verifier().map_err(|e| retry(&e))?;
        Some(Connector::Rustls(Arc::new(config)))
    } else {
        Some(Connector::Plain)
    };
    match tungstenite::client_tls_with_config(req, stream, None, connector) {
        Ok((mut ws, _)) => {
            let s = tcp(&mut ws);
            s.set_read_timeout(Some(TICK)).map_err(|e| retry(&e))?;
            s.set_write_timeout(Some(WRITE_TIMEOUT))
                .map_err(|e| retry(&e))?;
            Ok(ws)
        }
        Err(tungstenite::HandshakeError::Failure(tungstenite::Error::Http(r)))
            if matches!(r.status().as_u16(), 401 | 403) =>
        {
            Err(End::Refused(format!("refused the key ({})", r.status())))
        }
        Err(e) => Err(retry(&e)),
    }
}

fn send(ws: &mut Ws, frames: Vec<Sealed>) -> Result<(), End> {
    for f in frames {
        ws.send(Message::Binary(f.into_bytes().into()))
            .map_err(|e| End::Retry(e.to_string()))?;
    }
    Ok(())
}

/// Runs the summary protocol over one connection until it ends or `stop` is set. `step` runs each
/// protocol step (tests switch to a Mac's folder there).
fn talk(
    ws: &mut Ws,
    session: &mut Session,
    within: &(dyn Fn(&mut dyn FnMut()) + Send + Sync),
    stop: &AtomicBool,
    woken: &mut dyn FnMut() -> bool,
) -> Result<(), End> {
    let mut run = |f: &mut Step| {
        let mut out = Ok(vec![]);
        within(&mut || out = f(session));
        out.map_err(End::Retry)
    };
    send(ws, run(&mut |s| s.hello())?)?;
    let mut online = 0;
    within(&mut || write_status(true, online, None, false));
    while !stop.load(Ordering::SeqCst) {
        if woken() {
            return Err(End::Retry(WOKE.into())); // the connection is likely dead: start a fresh one
        }
        let frames = match ws.read() {
            Ok(Message::Binary(f)) => run(&mut |s| s.receive(&f))?,
            Ok(Message::Text(t)) => {
                let n = serde_json::from_str::<Value>(&t)
                    .ok()
                    .and_then(|v| v.get("online")?.as_u64());
                match n {
                    Some(n) => {
                        let rose = n > online;
                        online = n;
                        within(&mut || write_status(true, online, None, false));
                        if rose {
                            run(&mut |s| s.hello())?
                        } else {
                            vec![]
                        }
                    }
                    None => vec![],
                }
            }
            Ok(Message::Close(_)) => {
                return Err(End::Retry("the relay closed the connection".into()));
            }
            Ok(_) => vec![], // ping/pong: tungstenite answers on the next write or flush
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
            {
                run(&mut |s| s.tick())?
            }
            Err(e) => return Err(End::Retry(e.to_string())),
        };
        send(ws, frames)?;
        ws.flush().map_err(|e| End::Retry(e.to_string()))?;
    }
    let _ = ws.close(None);
    Ok(())
}

/// A random duration in [wait/2, wait].
fn jitter(wait: Duration) -> Duration {
    let mut b = [0u8; 2];
    let _ = security_framework::random::SecRandom::default().copy_bytes(&mut b);
    let f = 0.5 + f64::from(u16::from_le_bytes(b)) / f64::from(u16::MAX) / 2.0;
    wait.mul_f64(f)
}

/// Starts the link to the relay at `url` for `key` on its own thread and returns at once. Each connection
/// runs a fresh protocol::Session; `after` runs once received records changed something.
pub fn start(
    url: &str,
    key: &Key,
    after: Coalesced,
    within: impl Fn(&mut dyn FnMut()) + Send + Sync + 'static,
) -> Link {
    let (url, token, vault, seal) = (
        url.to_string(),
        key::token(key),
        key::vault_id(key),
        key::seal_key(key),
    );
    let (stop, nudge) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let (stopped, nudged) = (stop.clone(), nudge.clone());
    std::thread::spawn(move || {
        within(&mut || write_status(false, 0, None, false));
        let mut watch = super::wake::Watch::new();
        let mut woken = || nudged.swap(false, Ordering::SeqCst) || watch.changed();
        let mut wait = BACKOFF;
        while !stopped.load(Ordering::SeqCst) {
            let error = match connect(&url, &vault, &token) {
                Ok(mut ws) => {
                    wait = BACKOFF; // it worked: a later drop retries quickly
                    let mut session = Session::with(seal, &vault, after.clone());
                    match talk(&mut ws, &mut session, &within, &stopped, &mut woken) {
                        Ok(()) => return,                            // stopped
                        Err(End::Retry(e)) if e == WOKE => continue, // reconnect at once
                        Err(End::Retry(e) | End::Refused(e)) => e,
                    }
                }
                Err(End::Refused(e)) => {
                    within(&mut || write_status(false, 0, Some(&e), true));
                    return; // retrying with the same key can't help
                }
                Err(End::Retry(e)) => e,
            };
            within(&mut || write_status(false, 0, Some(&error), false));
            let pause = jitter(wait);
            let start = std::time::Instant::now();
            wait = (wait * 2).min(BACKOFF_MAX);
            while start.elapsed() < pause && !stopped.load(Ordering::SeqCst) {
                if woken() {
                    wait = BACKOFF; // a new network or a fresh wake: try now, and soon again
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    });
    Link {
        stop,
        #[cfg(test)]
        nudge,
    }
}

#[cfg(test)]
#[path = "link_tests.rs"]
mod tests;
#[cfg(test)]
#[path = "link_wake_tests.rs"]
mod wake_tests;
