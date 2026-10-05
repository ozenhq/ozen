//! Sync with the vault's other Macs on this network, no relay: nothing leaves the network, not even
//! sealed frames, and sync keeps working with the relay or the internet down. Each Mac advertises
//! `_ozen-sync._tcp` over Bonjour with a tag derived from the LAN key; a Mac that finds a peer with its
//! tag connects over TCP, both prove they hold the vault key (`handshake`), and the summary protocol
//! (protocol.rs) runs over the connection with the same sealed frames the relay would carry.
#![allow(dead_code)] // ponytail: started with the background connection (OFE-7)
use super::key::Key;
use hmac::{Hmac, KeyInit, Mac};
use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use security_framework::random::SecRandom;
use sha2::Sha256;
use socket2::{SockRef, TcpKeepalive};
use std::collections::HashSet;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// The Bonjour service type, as declared in Ozen.app's Info.plist (app_plist.rs).
pub fn service_type() -> String {
    format!("{}.local.", crate::app_plist::BONJOUR)
}
/// The relay's frame cap too: a sealed frame is at most 60 KiB (seal.rs).
const MAX_FRAME: usize = 64 << 10;
/// The whole handshake must finish in this long, however slowly the peer trickles bytes.
const HANDSHAKE: Duration = Duration::from_secs(10);
/// Connections (handshaking or syncing) one Mac accepts at once; more are closed straight away.
/// A user has a few Macs; this only bounds what a stranger on the network can make it hold.
const MAX_PEERS: usize = 16;
/// A peer that stops reading for this long is dropped (and a dead one by TCP keepalive).
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Redial a vanished connection after this, doubling up to `REDIAL_MAX`, while the peer advertises.
const REDIAL: Duration = Duration::from_secs(1);
const REDIAL_MAX: Duration = Duration::from_secs(60);

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn random() -> Result<[u8; 32], String> {
    let mut b = [0; 32];
    SecRandom::default()
        .copy_bytes(&mut b)
        .map_err(|e| e.to_string())?;
    Ok(b)
}

fn hmac(lan: &Key, parts: &[&[u8]]) -> Hmac<Sha256> {
    let mut m = Hmac::<Sha256>::new_from_slice(lan).expect("any key size");
    for p in parts {
        m.update(p);
    }
    m
}

/// Advertised so Macs of one vault find each other. Keyed with the LAN key, so neither the relay
/// (which knows the vault id and token) nor anyone else can compute it or link it to a vault.
pub fn tag(lan: &Key) -> String {
    hex(&hmac(lan, &[b"ozen-sync lan tag"]).finalize().into_bytes()[..8])
}

/// `read_exact` that gives up at `deadline`, not per read: a peer trickling a byte at a time can't
/// stretch it.
fn read_by(s: &mut TcpStream, buf: &mut [u8], deadline: Instant) -> std::io::Result<()> {
    let mut got = 0;
    while got < buf.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ErrorKind::TimedOut.into());
        }
        s.set_read_timeout(Some(left))?;
        match s.read(&mut buf[got..])? {
            0 => return Err(ErrorKind::UnexpectedEof.into()),
            n => got += n,
        }
    }
    Ok(())
}

/// Both sides prove they hold the LAN key (key::lan_key) without sending it: each sends a fresh random
/// nonce, then an HMAC over both nonces labelled with its role, so a proof can't be replayed or
/// reflected back. Fails on a wrong key, a non-ozen peer, or not finishing within `within`.
pub fn handshake(
    s: &mut TcpStream,
    lan: &Key,
    dialer: bool,
    within: Duration,
) -> Result<(), String> {
    let err = |e: std::io::Error| format!("handshake: {e}");
    let deadline = Instant::now() + within;
    s.set_write_timeout(Some(within)).map_err(err)?;
    let mine = random()?;
    s.write_all(&mine).map_err(err)?;
    let mut theirs = [0; 32];
    read_by(s, &mut theirs, deadline).map_err(err)?;
    let (d, l) = if dialer {
        (&mine, &theirs)
    } else {
        (&theirs, &mine)
    };
    let (me, them) = if dialer {
        ("dialer", "listener")
    } else {
        ("listener", "dialer")
    };
    let proof = |role: &str| hmac(lan, &[role.as_bytes(), d, l]);
    s.write_all(&proof(me).finalize().into_bytes())
        .map_err(err)?;
    let mut answer = [0; 32];
    read_by(s, &mut answer, deadline).map_err(err)?;
    proof(them)
        .verify_slice(&answer) // constant time
        .map_err(|_| "handshake: the peer doesn't hold this vault's key".to_string())?;
    s.set_read_timeout(None).map_err(err)?;
    s.set_write_timeout(Some(WRITE_TIMEOUT)).map_err(err)?;
    // a peer that vanishes (sleep, Wi-Fi off) is noticed within a few minutes
    let ka = TcpKeepalive::new()
        .with_time(Duration::from_secs(60))
        .with_interval(Duration::from_secs(10));
    SockRef::from(&*s).set_tcp_keepalive(&ka).map_err(err)
}

fn write_frame(s: &mut TcpStream, f: &[u8]) -> Result<(), String> {
    if f.len() > MAX_FRAME {
        return Err(format!("frame of {} bytes is over {MAX_FRAME}", f.len()));
    }
    s.write_all(&(f.len() as u32).to_be_bytes())
        .and_then(|()| s.write_all(f))
        .map_err(|e| e.to_string())
}

/// The next frame, or None when the peer hung up.
fn read_frame(s: &mut TcpStream) -> Result<Option<Vec<u8>>, String> {
    let mut n = [0; 4];
    match s.read_exact(&mut n) {
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        r => r.map_err(|e| e.to_string())?,
    }
    let n = u32::from_be_bytes(n) as usize;
    if n > MAX_FRAME {
        return Err(format!("frame of {n} bytes is over {MAX_FRAME}"));
    }
    let mut f = vec![0; n];
    s.read_exact(&mut f).map_err(|e| e.to_string())?;
    Ok(Some(f))
}

/// Runs the summary protocol with one authenticated peer until it hangs up. `step(None)` makes our
/// hello, `step(Some(frame))` handles one of theirs (protocol::Session); each returns frames to send.
/// Frames are written from their own thread, so two Macs sending big hellos at once never wait on each
/// other's reads.
pub fn talk(
    s: TcpStream,
    mut step: impl FnMut(Option<&[u8]>) -> Result<Vec<Vec<u8>>, String>,
) -> Result<(), String> {
    let mut w = s.try_clone().map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let writer = std::thread::spawn(move || {
        for f in rx {
            if let Err(e) = write_frame(&mut w, &f) {
                let _ = w.shutdown(std::net::Shutdown::Both); // unblocks the reader too
                return Err(e);
            }
        }
        Ok::<(), String>(())
    });
    let mut r = s;
    let send = |fs: Vec<Vec<u8>>| fs.into_iter().all(|f| tx.send(f).is_ok());
    let mut out = Ok(());
    if send(step(None)?) {
        out = loop {
            match read_frame(&mut r) {
                Ok(Some(f)) => match step(Some(&f)) {
                    Ok(fs) => {
                        if !send(fs) {
                            break Ok(()); // the writer stopped: its error is below
                        }
                    }
                    Err(e) => break Err(e),
                },
                Ok(None) => break Ok(()),
                Err(e) => break Err(e),
            }
        };
    }
    let _ = r.shutdown(std::net::Shutdown::Both); // ends the writer if it's blocked
    drop(tx);
    let wrote = writer.join().unwrap_or(Err("writer panicked".into()));
    out.and(wrote)
}

/// Advertising, browsing and accepting; dropping it stops all three (open connections run until the
/// peer hangs up).
pub struct Local {
    daemon: ServiceDaemon,
    fullname: String,
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
}

impl Local {
    /// The TCP port peers connect to.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }
}

impl Drop for Local {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(("127.0.0.1", self.addr.port())); // wakes the accept loop to see `stop`
        if let Ok(done) = self.daemon.unregister(&self.fullname) {
            let _ = done.recv_timeout(Duration::from_secs(1)); // the goodbye went out
        }
        let _ = self.daemon.shutdown();
    }
}

/// Whether a connection from `ip` may reach the handshake: only this network (loopback, private and
/// link-local IPv4, link-local and unique-local IPv6). A Mac with a public or VPN address would otherwise
/// let anyone on the internet probe the port and tie up handshake slots (OFE-86).
pub fn local_source(ip: std::net::IpAddr) -> bool {
    use std::net::IpAddr::{V4, V6};
    match ip {
        V4(a) => a.is_loopback() || a.is_private() || a.is_link_local(),
        V6(a) => match a.to_ipv4_mapped() {
            Some(v4) => local_source(V4(v4)),
            None => a.is_loopback() || a.is_unicast_link_local() || a.is_unique_local(),
        },
    }
}

/// Counts a live connection; frees its slot when dropped.
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(n: &Arc<AtomicUsize>) -> Option<Slot> {
        (n.fetch_add(1, Ordering::SeqCst) < MAX_PEERS)
            .then(|| Slot(n.clone()))
            .or_else(|| {
                n.fetch_sub(1, Ordering::SeqCst);
                None
            })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Advertises this Mac on `interfaces` and connects to the vault's other Macs found there. Each
/// authenticated connection, either way, runs `on_peer` on its own thread. Of two Macs that find each
/// other, only the one with the smaller instance id dials, and it redials with backoff whenever the
/// connection ends, for as long as the other Mac advertises.
pub fn start(
    lan: Key,
    interfaces: IfKind,
    on_peer: impl Fn(TcpStream) + Send + Sync + 'static,
) -> Result<Local, String> {
    let err = |e: &dyn std::fmt::Display| format!("local sync: {e}");
    // loopback only (tests) listens on loopback only; otherwise every interface, filtered by source
    let any = if matches!(interfaces, IfKind::LoopbackV4) {
        "127.0.0.1"
    } else {
        "0.0.0.0"
    };
    let listener = TcpListener::bind((any, 0)).map_err(|e| err(&e))?;
    let addr = listener.local_addr().map_err(|e| err(&e))?;
    let id = hex(&random()?[..8]);
    let tag = tag(&lan);
    let daemon = ServiceDaemon::new().map_err(|e| err(&e))?;
    daemon.disable_interface(IfKind::All).map_err(|e| err(&e))?;
    daemon.enable_interface(interfaces).map_err(|e| err(&e))?;
    let props = [("tag", tag.as_str()), ("id", id.as_str())];
    let host = format!("{id}.local.");
    let me = ServiceInfo::new(&service_type(), &id, &host, "", addr.port(), &props[..])
        .map_err(|e| err(&e))?
        .enable_addr_auto();
    let fullname = me.get_fullname().to_string();
    daemon.register(me).map_err(|e| err(&e))?;
    let found = daemon.browse(&service_type()).map_err(|e| err(&e))?;

    let (on_peer, stop, slots) = (
        Arc::new(on_peer),
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicUsize::new(0)),
    );
    let (on_accept, stopped, accept_slots) = (on_peer.clone(), stop.clone(), slots.clone());
    std::thread::spawn(move || {
        while !stopped.load(Ordering::SeqCst) {
            let mut s = match listener.accept() {
                Ok((s, from)) if local_source(from.ip()) => s,
                Ok(_) => continue, // from outside this network: closed before a byte is read
                // out of file descriptors or similar: back off instead of spinning
                Err(_) => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
            };
            let Some(slot) = Slot::take(&accept_slots) else {
                continue; // dropping `s` closes it
            };
            let on_peer = on_accept.clone();
            std::thread::spawn(move || {
                let _slot = slot;
                if handshake(&mut s, &lan, false, HANDSHAKE).is_ok() {
                    on_peer(s);
                }
            });
        }
    });

    // Instance ids advertising now; a dial loop runs while its peer's id is in here.
    let present = Arc::new(Mutex::new(HashSet::<String>::new()));
    let stopped = stop.clone();
    std::thread::spawn(move || {
        while let Ok(ev) = found.recv() {
            if stopped.load(Ordering::SeqCst) {
                return;
            }
            let r = match ev {
                ServiceEvent::ServiceResolved(r) => r,
                ServiceEvent::ServiceRemoved(_, name) => {
                    let gone = name.split('.').next().unwrap_or_default().to_string();
                    present.lock().unwrap().remove(&gone);
                    continue;
                }
                _ => continue,
            };
            let theirs = r.get_property_val_str("id").unwrap_or_default().to_string();
            if r.get_property_val_str("tag") != Some(tag.as_str()) || theirs <= id {
                continue; // another vault, ourselves, or theirs to dial
            }
            if !present.lock().unwrap().insert(theirs.clone()) {
                continue; // already dialing it
            }
            let port = r.get_port();
            let addrs: Vec<SocketAddr> = r
                .get_addresses()
                .iter()
                .map(|a| (a.to_ip_addr(), port).into())
                .collect();
            let (on_peer, present, stopped, slots) = (
                on_peer.clone(),
                present.clone(),
                stopped.clone(),
                slots.clone(),
            );
            std::thread::spawn(move || {
                let mut wait = REDIAL;
                let here =
                    || present.lock().unwrap().contains(&theirs) && !stopped.load(Ordering::SeqCst);
                while here() {
                    let s = addrs
                        .iter()
                        .find_map(|a| TcpStream::connect_timeout(a, HANDSHAKE).ok());
                    if let (Some(mut s), Some(slot)) = (s, Slot::take(&slots)) {
                        if handshake(&mut s, &lan, true, HANDSHAKE).is_ok() {
                            on_peer(s);
                            wait = REDIAL; // it worked; a later drop retries quickly
                        }
                        drop(slot);
                    }
                    std::thread::sleep(wait);
                    wait = (wait * 2).min(REDIAL_MAX);
                }
            });
        }
    });
    Ok(Local {
        daemon,
        fullname,
        stop,
        addr,
    })
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
