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
    // a bracketed IPv6 relay is a host
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

#[test]
fn a_relay_that_never_answers_times_out() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://127.0.0.1:{}", l.local_addr().unwrap().port());
    std::thread::spawn(move || {
        let (_s, _) = l.accept().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(5)); // holds the socket, says nothing
    });
    let started = std::time::Instant::now();
    let err = healthy_within(&url, std::time::Duration::from_millis(300)).unwrap_err();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(
        err.starts_with(&format!("no sync relay answering at {url}")),
        "{err}"
    );
}

#[test]
fn a_wrong_body_says_what_came_back() {
    let url = relay("not a relay");
    let err = healthy(&url).unwrap_err();
    assert!(
        err.starts_with(&format!("no sync relay answering at {url}")),
        "{err}"
    );
    assert!(err.contains("not a relay"), "{err}");
}

#[test]
fn sync_runs_no_external_program() {
    // ozen's own binary (apply.rs runs `ozen retrain`) is the only program sync starts; tests may start
    // others (CPU load in run_tests.rs)
    let start = ["Command", "::new("].concat(); // spelled apart so this file doesn't match itself
    for f in std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/src/sync")).unwrap() {
        let p = f.unwrap().path();
        if p.to_string_lossy().ends_with("_tests.rs") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&p) else {
            continue; // snapshots/
        };
        for line in src.lines().filter(|l| l.contains(&start)) {
            assert!(
                line.contains(&format!("{start}exe)")),
                "{}: {line}",
                p.display()
            );
        }
    }
}

#[test]
fn a_redirect_to_something_that_answers_ok_is_not_a_relay() {
    let target = relay("ok");
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("ws://127.0.0.1:{}", l.local_addr().unwrap().port());
    let to = target.replacen("ws", "http", 1);
    std::thread::spawn(move || {
        let (mut s, _) = l.accept().unwrap();
        let _ = s.read(&mut [0; 1024]);
        let _ = write!(
            s,
            "HTTP/1.1 301 Moved\r\nlocation: {to}/health\r\ncontent-length: 0\r\n\r\n"
        );
    });
    assert!(healthy(&url).is_err());
}

#[test]
fn lan_only_means_no_relay_whatever_is_saved_or_set() {
    let d = tempfile::tempdir().unwrap();
    let saved = d.path().join(FILE);
    std::fs::write(&saved, "wss://relay.example").unwrap();
    let env = Some("wss://other.example".to_string());
    assert_eq!(pick(true, env.clone(), &saved), Ok(None));
    assert_eq!(pick(true, None, &saved), Ok(None));
    // off again: the saved URL (and the env override) come back as they were
    assert_eq!(
        pick(false, None, &saved),
        Ok(Some("wss://relay.example".into()))
    );
    assert_eq!(
        pick(false, env, &saved),
        Ok(Some("wss://other.example".into()))
    );
}
