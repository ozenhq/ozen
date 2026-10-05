use super::*;
use serde_json::json;
use std::net::TcpListener;

/// What `ozen sync preview` prints for the ozen folder `d`.
fn preview_of(d: &tempfile::TempDir) -> String {
    render(&read_synced(&format!("{}/", d.path().display())))
}

/// A sandbox with a bit of everything sync carries, live and deleted.
fn sandbox() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let w = |f: &str, v: String| std::fs::write(d.path().join(f), v).unwrap();
    let lines = [
        json!({"id": "1@a", "v": 1, "t": 1_767_225_600.0, "spk": "Dana", "text": "hi", "e": [0.1, 0.2]}),
        json!({"id": "2@a", "v": 1, "t": 1_767_312_000.0, "text": "a note", "src": "note"}),
        json!({"id": "3@a", "v": 2, "t": 1_775_000_000.0, "text": "later", "e": [0.3, 0.4]}),
        json!({"id": "4@a", "v": 3, "del": true}),
    ];
    w(
        "lines.jsonl",
        lines.iter().map(|l| format!("{l}\n")).collect(),
    );
    w(
        "tags.json",
        json!({"1@a": {"v": 1, "val": "Dana"}, "9@a": {"v": 2}}).to_string(),
    );
    w("fixes.json", json!({"1@a": "hi there"}).to_string());
    w(
        "vocab.json",
        json!({"ozen": {"v": 1, "val": true}, "Kev": {"v": 1, "val": true}}).to_string(),
    );
    w(
        "places.json",
        json!([
            {"id": "p1@a", "v": 1, "action": "record", "label": "Home", "lat": 32.1, "lon": 34.8},
            {"id": "p2@a", "v": 1, "action": "off", "label": "Gym"},
            {"id": "p3@a", "v": 2, "del": true}
        ])
        .to_string(),
    );
    d
}

#[test]
fn preview_counts_what_would_sync_and_connects_nowhere() {
    let d = sandbox();
    // a relay that would see any connection: preview must never reach it
    let relay = TcpListener::bind("127.0.0.1:0").unwrap();
    relay.set_nonblocking(true).unwrap();
    let url = format!("ws://{}", relay.local_addr().unwrap());
    // SAFETY: nextest runs each test in its own process
    unsafe { std::env::set_var("OZEN_SYNC_URL", &url) };
    let out = preview_of(&d);
    insta::assert_snapshot!(out);
    assert!(relay.accept().is_err(), "preview connected to the relay");
}

#[test]
fn an_empty_folder_previews_zeroes() {
    let d = tempfile::tempdir().unwrap();
    let out = preview_of(&d);
    assert!(out.contains("- 0 transcript lines,"), "{out}");
    assert!(!out.contains("deleted"), "{out}");
}

/// The preview reads files and nothing else: no socket, relay config or key in its code.
#[test]
fn the_preview_has_no_way_to_reach_the_network() {
    let src = include_str!("preview.rs");
    let code = &src[..src.find("#[cfg(test)]").unwrap()];
    for no in [
        "TcpStream",
        "UdpSocket",
        "std::net",
        "config::",
        "key::",
        "local::",
        "talk::",
        "Command::",
    ] {
        assert!(!code.contains(no), "preview.rs uses {no}");
    }
}
