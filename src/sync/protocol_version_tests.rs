//! Macs on different ozen versions keep syncing in the version they share (OFE-82, version.rs).
use super::tests::{Mac, folder};
use super::*;
use serde_json::json;

/// A Mac whose newest protocol version is `own`, with tag `who` only it has.
fn mac(own: u8, who: &str) -> (tempfile::TempDir, Mac) {
    let d = folder(json!([]), json!({who: {"v": 1, "val": "Dana"}}));
    let mut m = Mac::new(d.path());
    m.s.versions = Versions::new(own);
    (d, m)
}

fn version_of(m: &Mac, frame: &[u8]) -> u8 {
    seal::open(&m.s.seal_key, &m.s.vault, frame).unwrap()[0]
}

/// Both say hello, then every frame is carried to the other until both are quiet; returns the
/// protocol versions of every frame sent after the hellos.
fn meet(a: &mut Mac, b: &mut Mac) -> Vec<u8> {
    let (mut to_b, mut to_a) = (a.hello(), b.hello());
    let mut versions = vec![];
    while !to_a.is_empty() || !to_b.is_empty() {
        for f in std::mem::take(&mut to_b) {
            let r = b.receive(&f);
            versions.extend(r.iter().map(|f| version_of(b, f)));
            to_a.extend(r);
        }
        for f in std::mem::take(&mut to_a) {
            let r = a.receive(&f);
            versions.extend(r.iter().map(|f| version_of(a, f)));
            to_b.extend(r);
        }
    }
    versions
}

#[test]
fn a_v2_mac_and_a_v1_mac_converge_in_v1() {
    let ((_da, mut a), (_db, mut b)) = (mac(2, "a"), mac(1, "b"));
    let sent = meet(&mut a, &mut b);
    assert!(!sent.is_empty() && sent.iter().all(|v| *v == 1), "{sent:?}");
    assert_eq!(a.synced(), b.synced());
    assert_eq!(a.synced().len(), 2);
    assert_eq!(
        (a.s.dropped.clone(), b.s.dropped.clone()),
        (Dropped::default(), Dropped::default())
    );
}

#[test]
fn two_v2_macs_speak_v2() {
    let ((_da, mut a), (_db, mut b)) = (mac(2, "a"), mac(2, "b"));
    let sent = meet(&mut a, &mut b);
    assert!(!sent.is_empty() && sent.iter().all(|v| *v == 2), "{sent:?}");
    assert_eq!(a.synced(), b.synced());
}

#[test]
fn a_mac_too_old_for_a_v3_only_peer_is_told_to_update() {
    // a v3 build reads v2 and v3; this v1 Mac reads only v1
    let ((_da, mut new), (_db, mut old)) = (mac(3, "a"), mac(1, "b"));
    meet(&mut new, &mut old);
    assert!(old.s.dropped.newer > 0);
    assert!(old.s.dropped.advice().is_some());
    assert!(!old.synced().contains_key(&("tags".into(), "a".into())));
    // and the v3 Mac says why it can't read the v1 Mac's hello
    let e = new.s.dropped.last_error.clone().unwrap_or_default();
    assert!(
        e.contains("older ozen") && e.contains("update ozen on that Mac"),
        "{e}"
    );
}
