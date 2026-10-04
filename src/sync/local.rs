//! Sync with the vault's other Macs on this network, no relay: nothing leaves the network, not even
//! sealed frames, and sync keeps working with the relay or the internet down. Each Mac advertises
//! `_ozen-sync._tcp` over Bonjour with a tag derived from the vault id; a Mac that finds a peer with
//! its tag connects over TCP, both prove they hold the vault key (`handshake`), and the summary protocol
//! (protocol.rs) runs over the connection with the same sealed frames the relay would carry.
#![allow(dead_code)] // ponytail: started with the background connection (OFE-7)
use super::key::Key;
use hmac::{Hmac, KeyInit, Mac};
use mdns_sd::{IfKind, ServiceDaemon, ServiceEvent, ServiceInfo};
use security_framework::random::SecRandom;
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::{ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

/// The Bonjour service type, as declared in Ozen.app's Info.plist (app_plist.rs).
pub fn service_type() -> String {
    format!("{}.local.", crate::app_plist::BONJOUR)
}
/// The relay's frame cap too: a sealed frame is at most 60 KiB (seal.rs).
const MAX_FRAME: usize = 64 << 10;
/// A peer that doesn't finish the handshake in this long is dropped.
const HANDSHAKE: Duration = Duration::from_secs(10);

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

/// Advertised instead of the vault id: Macs of one vault match on it, and it can't be turned back into
/// the vault id or linked to the vault on the relay.
pub fn tag(vault: &str) -> String {
    hex(&Sha256::digest(format!("ozen-sync lan tag {vault}").as_bytes())[..8])
}

fn proof(lan: &Key, role: &str, dialer: &[u8; 32], listener: &[u8; 32]) -> Hmac<Sha256> {
    let mut m = Hmac::<Sha256>::new_from_slice(lan).expect("any key size");
    m.update(role.as_bytes());
    m.update(dialer);
    m.update(listener);
    m
}

/// Both sides prove they hold the LAN key (key::lan_key) without sending it: each sends a fresh random
/// nonce, then an HMAC over both nonces labelled with its role, so a proof can't be replayed or
/// reflected back. Fails on a wrong key, a non-ozen peer, or no answer within `HANDSHAKE`.
pub fn handshake(s: &mut TcpStream, lan: &Key, dialer: bool) -> Result<(), String> {
    let err = |e: std::io::Error| format!("handshake: {e}");
    s.set_read_timeout(Some(HANDSHAKE)).map_err(err)?;
    let mine = random()?;
    s.write_all(&mine).map_err(err)?;
    let mut theirs = [0; 32];
    s.read_exact(&mut theirs).map_err(err)?;
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
    s.write_all(&proof(lan, me, d, l).finalize().into_bytes())
        .map_err(err)?;
    let mut answer = [0; 32];
    s.read_exact(&mut answer).map_err(err)?;
    proof(lan, them, d, l)
        .verify_slice(&answer) // constant time
        .map_err(|_| "handshake: the peer doesn't hold this vault's key".to_string())?;
    s.set_read_timeout(None).map_err(err)
}

fn write_frame(s: &mut TcpStream, f: &[u8]) -> Result<(), String> {
    let n = u32::try_from(f.len()).map_err(|e| e.to_string())?;
    s.write_all(&n.to_be_bytes())
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
pub fn talk(
    mut s: TcpStream,
    mut step: impl FnMut(Option<&[u8]>) -> Result<Vec<Vec<u8>>, String>,
) -> Result<(), String> {
    for f in step(None)? {
        write_frame(&mut s, &f)?;
    }
    while let Some(f) = read_frame(&mut s)? {
        for r in step(Some(&f))? {
            write_frame(&mut s, &r)?;
        }
    }
    Ok(())
}

/// Advertising and browsing; dropping it stops both (open connections run until the peer hangs up).
pub struct Local {
    daemon: ServiceDaemon,
}

impl Drop for Local {
    fn drop(&mut self) {
        let _ = self.daemon.shutdown();
    }
}

/// Advertises this Mac on `interfaces` and connects to the vault's other Macs found there. Each
/// authenticated connection, either way, runs `on_peer` on its own thread. Of two Macs that find each
/// other, only the one with the smaller instance id dials, so a pair opens one connection.
pub fn start(
    vault: &str,
    lan: Key,
    interfaces: IfKind,
    on_peer: impl Fn(TcpStream) + Send + Sync + 'static,
) -> Result<Local, String> {
    let err = |e: &dyn std::fmt::Display| format!("local sync: {e}");
    let listener = TcpListener::bind(("0.0.0.0", 0)).map_err(|e| err(&e))?;
    let port = listener.local_addr().map_err(|e| err(&e))?.port();
    let id = hex(&random()?[..8]);
    let tag = tag(vault);
    let daemon = ServiceDaemon::new().map_err(|e| err(&e))?;
    daemon.disable_interface(IfKind::All).map_err(|e| err(&e))?;
    daemon.enable_interface(interfaces).map_err(|e| err(&e))?;
    let props = [("tag", tag.as_str()), ("id", id.as_str())];
    let me = ServiceInfo::new(
        &service_type(),
        &id,
        &format!("{id}.local."),
        "",
        port,
        &props[..],
    )
    .map_err(|e| err(&e))?
    .enable_addr_auto();
    daemon.register(me).map_err(|e| err(&e))?;
    let found = daemon.browse(&service_type()).map_err(|e| err(&e))?;

    let on_peer = Arc::new(on_peer);
    let (accept, peer) = (on_peer.clone(), on_peer);
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let on_peer = accept.clone();
            std::thread::spawn(move || {
                let mut s = s;
                if handshake(&mut s, &lan, false).is_ok() {
                    on_peer(s);
                }
            });
        }
    });
    std::thread::spawn(move || {
        let mut dialed = HashSet::new();
        while let Ok(ev) = found.recv() {
            let ServiceEvent::ServiceResolved(r) = ev else {
                continue;
            };
            let theirs = r.get_property_val_str("id").unwrap_or_default().to_string();
            if r.get_property_val_str("tag") != Some(tag.as_str()) || theirs <= id {
                continue; // another vault, ourselves, or theirs to dial
            }
            if !dialed.insert(theirs) {
                continue;
            }
            let port = r.get_port();
            let addrs: Vec<_> = r
                .get_addresses()
                .iter()
                .map(|a| (a.to_ip_addr(), port))
                .collect();
            let on_peer = peer.clone();
            std::thread::spawn(move || {
                let Some(mut s) = addrs.iter().find_map(|a| TcpStream::connect(a).ok()) else {
                    return;
                };
                if handshake(&mut s, &lan, true).is_ok() {
                    on_peer(s);
                }
            });
        }
    });
    Ok(Local { daemon })
}

#[cfg(test)]
#[path = "local_tests.rs"]
mod tests;
