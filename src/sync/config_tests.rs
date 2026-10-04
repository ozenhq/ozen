use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

/// A relay stand-in that answers one `/health` with `body`; returns its URL.
fn relay(body: &'static str) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
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
    std::fs::write(&saved, "https://saved.example/\n").unwrap();
    assert_eq!(
        resolve(None, &saved),
        Ok(Some("https://saved.example".into()))
    );
    assert_eq!(
        resolve(Some("https://env.example".into()), &saved),
        Ok(Some("https://env.example".into()))
    );
}

#[test]
fn http_only_to_this_mac() {
    assert!(check("http://relay.example").is_err());
    assert!(check("http://localhost.evil.example").is_err());
    assert!(check("ftp://relay.example").is_err());
    assert!(check("relay.example").is_err());
    assert!(
        resolve(
            Some("http://relay.example".into()),
            Path::new("/nonexistent")
        )
        .is_err()
    );
    assert_eq!(
        check("http://localhost:8080/"),
        Ok("http://localhost:8080".into())
    );
    assert_eq!(check("http://127.0.0.1:1"), Ok("http://127.0.0.1:1".into()));
    assert_eq!(check("http://[::1]:9"), Ok("http://[::1]:9".into()));
    assert_eq!(
        check("https://relay.example/base/"),
        Ok("https://relay.example/base".into())
    );
}

#[test]
fn saves_only_a_relay_that_answers() {
    let d = tempfile::tempdir().unwrap();
    let saved = d.path().join(FILE);
    // nothing listens on port 1
    assert!(save("http://127.0.0.1:1", &saved).is_err());
    assert!(save(&relay("not a relay"), &saved).is_err());
    assert!(save("http://relay.example", &saved).is_err());
    assert!(!saved.exists());
    let url = relay("ok");
    assert_eq!(save(&url, &saved), Ok(url.clone()));
    assert_eq!(resolve(None, &saved), Ok(Some(url)));
}
