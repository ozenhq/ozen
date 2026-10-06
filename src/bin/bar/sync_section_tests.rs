use super::{View, set_up_args, view};
use serde_json::json;

/// `ozen sync status` with sync on and running, plus `extra`.
fn on(extra: serde_json::Value) -> View {
    let mut s = json!({"sync": "on", "running": true, "lan_only": false, "error": null,
        "other_macs_online": 0, "macs": []});
    for (k, v) in extra.as_object().unwrap() {
        s[k] = v.clone();
    }
    view(&s)
}

#[test]
fn off_offers_set_up_and_join() {
    let v = view(&json!({"sync": "off", "running": false, "macs": []}));
    assert_eq!(
        v,
        View {
            line: "Sync is off. Set it up on your first Mac, then Join from the others.".into(),
            set_up: true,
            pair: false,
            lan_only: None,
            paused: false,
        }
    );
    assert_eq!(
        view(&serde_json::Value::Null),
        v,
        "no answer from the CLI reads as off"
    );
}

#[test]
fn set_up_alone_says_to_pair() {
    assert_eq!(
        on(json!({})),
        View {
            line: "Sync is on. No other Mac is online now.\n\
                   No other Mac has synced with this one yet: Pair a Mac to add one."
                .into(),
            set_up: false,
            pair: true,
            lan_only: Some(false),
            paused: false,
        }
    );
}

#[test]
fn a_peer_online_is_counted_and_listed() {
    let v = on(json!({"other_macs_online": 1,
        "macs": [{"id": "a1", "name": "Desk", "days_ago": 0}]}));
    assert_eq!(
        v.line,
        "Sync is on. 1 other Mac is online now.\nLast synced with Desk today"
    );
}

#[test]
fn a_peer_last_seen_long_ago_says_why() {
    let v = on(json!({"macs": [
        {"id": "a1", "name": "Desk", "days_ago": 1},
        {"id": "b2", "name": "Old laptop", "days_ago": 9}]}));
    assert_eq!(
        v.line,
        "Sync is on. No other Mac is online now.\n\
         Last synced with Desk yesterday\n\
         Last synced with Old laptop 9 days ago (Macs sync only while both are on and online at the same time)"
    );
}

#[test]
fn lan_only_shows_the_switch_on() {
    let v = on(json!({"lan_only": true, "other_macs_online": null,
        "macs": [{"id": "a1", "name": "Desk", "days_ago": 0}]}));
    assert_eq!(
        v.line,
        "Sync is on, with Macs on this network only.\nLast synced with Desk today"
    );
    assert_eq!(v.lan_only, Some(true));
}

#[test]
fn an_error_is_shown_as_is() {
    let v = on(json!({"running": false, "error": "Local Network access is off"}));
    assert!(
        v.line
            .starts_with("Sync isn't working: Local Network access is off\n")
    );
    assert!(v.pair, "still set up: pairing works once it runs");
}

#[test]
fn starting_and_paused() {
    assert!(
        on(json!({"running": false}))
            .line
            .starts_with("Sync is starting…")
    );
    let p = view(&json!({"sync": "paused", "running": false, "macs": []}));
    assert_eq!(
        p.line,
        "Sync is paused (after Undo). Set up sync turns it back on."
    );
    assert!(p.set_up && !p.pair && p.lan_only.is_none() && p.paused);
}

#[test]
fn set_up_with_a_relay_without_one_or_again_after_undo() {
    assert_eq!(
        set_up_args("wss://relay.example", false),
        ["sync", "init", "--server", "wss://relay.example"]
    );
    assert_eq!(set_up_args("", false), ["sync", "init", "--lan-only"]);
    assert_eq!(
        set_up_args("", true),
        ["sync", "init"],
        "keeps the saved choice"
    );
}
