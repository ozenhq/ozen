//! After `ozen sync rotate` (OFE-19): a Mac still on the old key gets nothing from the new vault, and a
//! Mac that joined with the new key converges with the rotating one, through the relay.
use super::tests::{Relay, folder, ids, mac, wait_for};
use std::time::Duration;

#[test]
fn a_mac_on_the_old_key_gets_nothing_and_one_on_the_new_key_converges() {
    let relay = Relay::start();
    let (old, new) = (
        super::super::local::random().unwrap(),
        super::super::local::random().unwrap(),
    );
    let (rotated, rejoined, lost) = (folder(&["1@a"]), folder(&["2@b"]), folder(&["3@c"]));
    let _a = mac(&relay.url(), rotated.path(), &new);
    let _b = mac(&relay.url(), rejoined.path(), &new);
    let _c = mac(&relay.url(), lost.path(), &old); // the lost Mac, still on the old key
    wait_for(
        Duration::from_secs(20),
        "the rejoined Mac converges",
        || ids(rotated.path()) == ids(rejoined.path()) && ids(rotated.path()).len() == 2,
    );
    // give the lost Mac every chance: it is connected to the same relay all along
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(ids(lost.path()), ["3@c"], "the lost Mac got nothing");
    assert!(
        !ids(rotated.path()).contains(&"3@c".to_string()),
        "and sent nothing"
    );
}
