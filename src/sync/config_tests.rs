use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

/// A relay stand-in that answers one `/health` with `body`; returns its URL.
fn relay(body: &'static str) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://127.0.0.1:{}", l.local_addr().unwrap().port());
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let _ = s.read(&mut [0; 1024]);
        let _ = write!(
            s,
            "HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
    });
    url
}

#[test]
fn env_wins_over_saved_and_unset_is_off() {
    let d = tempfile::tempdir().unwrap();
    let saved = d.path().join(FILE);
    assert_eq!(resolve(None, &saved), Ok(None));
    assert_eq!(resolve(Some(" ".into()), &saved), Ok(None));
    std::fs::write(&saved, "wss://saved.example/\n").unwrap();
    assert_eq!(
        resolve(None, &saved),
        Ok(Some("wss://saved.example".into()))
    );
    assert_eq!(
        resolve(Some("wss://env.example".into()), &saved),
        Ok(Some("wss://env.example".into()))
    );
}

#[test]
fn ws_only_to_this_mac() {
    for bad in [
        "ws://relay.example",
        "ws://localhost.evil.example",
        "ws://localhost@evil.example",
        "ws://127.0.0.2",
        "https://relay.example",
        "relay.example",
        "wss://u:p@relay.example",
        "wss://relay.example/?a=b",
        "wss://relay.example/#f",
    ] {
        assert!(check(bad).is_err(), "{bad}");
    }
    assert!(resolve(Some("ws://relay.example".into()), Path::new("/nonexistent")).is_err());
    assert_eq!(
        check("ws://localhost:8080/"),
        Ok("ws://localhost:8080".into())
    );
    assert_eq!(check("ws://127.0.0.1:1"), Ok("ws://127.0.0.1:1".into()));
    assert_eq!(check("ws://[0:0:0:0:0:0:0:1]:9"), Ok("ws://[::1]:9".into()));
    assert_eq!(
        check("wss://relay.example/base/"),
        Ok("wss://relay.example/base".into())
    );
}

#[test]
fn saves_only_a_relay_that_answers() {
    let d = tempfile::tempdir().unwrap();
    let saved = d.path().join(FILE);
    // nothing listens on port 1
    assert!(save("ws://127.0.0.1:1", &saved).is_err());
    assert!(save(&relay("not a relay"), &saved).is_err());
    assert!(save("ws://relay.example", &saved).is_err());
    assert!(!saved.exists());
    // a bracketed IPv6 relay reaches curl as a host, not a glob
    let l = std::net::TcpListener::bind("[::1]:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let _ = s.read(&mut [0; 1024]);
        let _ = s.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");
    });
    assert!(healthy(&format!("ws://[::1]:{port}")).is_ok());
    let url = relay("ok");
    assert_eq!(save(&url, &saved), Ok(url.clone()));
    assert_eq!(resolve(None, &saved), Ok(Some(url)));
}
