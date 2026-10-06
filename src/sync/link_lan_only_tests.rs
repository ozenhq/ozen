//! LAN-only mode (OFE-30): Macs sync over the same network while a relay on this very machine, with its
//! URL saved, sees no connection at all; turning it off brings the relay back with the same key.
use super::super::config;
use super::super::run::serve;
use super::tests::{Relay, at, folder, ids, wait_for};
use super::*;
use mdns_sd::IfKind;
use std::path::Path;

/// `ozen sync run`'s two ways of syncing, as serve_here starts them: same network always, the relay
/// only when `config::server` (read in `dir`, as the runner reads it in the ozen folder) names one.
fn mac(dir: &Path, key: &Key) -> (super::super::local::Local, Option<Link>) {
    let d = dir.to_path_buf();
    let within = move |step: &mut dyn FnMut()| at(&d, step);
    let local = serve(
        key,
        IfKind::LoopbackV4,
        Coalesced::new(|| {}),
        within.clone(),
    )
    .unwrap();
    let url = at(dir, config::server).unwrap();
    let link = url.map(|u| start(&u, key, Coalesced::new(|| {}), within));
    (local, link)
}

#[test]
fn lan_only_macs_converge_on_the_network_and_never_touch_the_relay() {
    let relay = Relay::start();
    let (a, b) = (folder(&["1@a"]), folder(&["2@b"]));
    for d in [&a, &b] {
        std::fs::write(d.path().join(config::FILE), relay.url()).unwrap(); // a relay is saved...
        std::fs::write(d.path().join(config::LAN_ONLY), "").unwrap(); // ...and LAN-only is on
    }
    let key = super::super::local::random().unwrap();
    let (_ma, la) = mac(a.path(), &crate::sync::key::key(key));
    let (_mb, lb) = mac(b.path(), &crate::sync::key::key(key));
    assert!(
        la.is_none() && lb.is_none(),
        "no relay link in LAN-only mode"
    );
    wait_for(Duration::from_secs(30), "same-network exchange", || {
        ids(a.path()).len() == 2 && ids(b.path()).len() == 2
    });
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(
        relay.accepted.load(Ordering::SeqCst),
        0,
        "the relay saw no connection"
    );
    assert_eq!(relay.refused.load(Ordering::SeqCst), 0);

    // `ozen sync init --relay` on a: the saved relay is back, with the same key, no pairing
    std::fs::remove_file(a.path().join(config::LAN_ONLY)).unwrap();
    let (_ma2, la2) = mac(a.path(), &crate::sync::key::key(key));
    assert!(la2.is_some());
    wait_for(Duration::from_secs(10), "a on the relay", || {
        at(a.path(), status)["connected"] == json!(true)
    });
    assert_eq!(relay.accepted.load(Ordering::SeqCst), 1);
}
