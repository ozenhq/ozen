use super::*;
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// A job that counts its runs and waits for a go from the test before finishing each one.
fn gated() -> (
    Coalesced,
    Arc<AtomicUsize>,
    mpsc::Sender<()>,
    mpsc::Receiver<()>,
) {
    let runs = Arc::new(AtomicUsize::new(0));
    let (go, wait) = mpsc::channel::<()>();
    let (started, on_start) = mpsc::channel::<()>();
    let wait = Mutex::new(wait);
    let r = runs.clone();
    let job = Coalesced::new(move || {
        r.fetch_add(1, Ordering::SeqCst);
        started.send(()).unwrap();
        wait.lock().unwrap().recv().unwrap();
    });
    (job, runs, go, on_start)
}

#[test]
fn three_requests_during_a_run_cause_exactly_one_more() {
    let (job, runs, go, started) = gated();
    job.request();
    started.recv().unwrap(); // the first run is going
    for _ in 0..3 {
        job.request();
    }
    go.send(()).unwrap();
    started.recv().unwrap(); // the one queued run
    go.send(()).unwrap();
    assert!(
        started.recv_timeout(Duration::from_millis(300)).is_err(),
        "no third run"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 2);
    job.request(); // idle again: a new request runs at once
    started.recv_timeout(Duration::from_secs(5)).unwrap();
    go.send(()).unwrap();
    assert_eq!(runs.load(Ordering::SeqCst), 3);
}

/// Runs `f` inside a fresh ozen folder holding `files`, as ozen runs from its folder.
fn in_folder<T>(files: &[(&str, String)], f: impl FnOnce() -> T) -> T {
    let d = tempfile::tempdir().unwrap();
    for (n, body) in files {
        std::fs::write(d.path().join(n), body).unwrap();
    }
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    let r = f();
    std::env::set_current_dir(back).unwrap();
    r
}

#[test]
fn a_received_tag_and_tombstone_land_once_and_the_same_again_changes_nothing() {
    let runs = Arc::new(AtomicUsize::new(0));
    let r = runs.clone();
    let after = Coalesced::new(move || {
        r.fetch_add(1, Ordering::SeqCst);
    });
    let lines = "{\"id\":\"1\",\"v\":1,\"text\":\"hi\"}\n{\"id\":\"2\",\"v\":1,\"text\":\"yo\"}\n";
    in_folder(&[("lines.jsonl", lines.into())], || {
        let mut theirs = Synced::default();
        theirs
            .tags
            .insert("1".into(), json!({"v": 2, "val": "Dana"}));
        let tomb = json!({"id": "2", "v": 5, "del": true});
        theirs.lines.push(tomb.as_object().unwrap().clone());

        assert!(received(&theirs, &after).unwrap(), "first time: changed");
        let tags: serde_json::Value =
            serde_json::from_slice(&std::fs::read("tags.json").unwrap()).unwrap();
        assert_eq!(tags["1"]["val"], "Dana");
        let lines = std::fs::read_to_string("lines.jsonl").unwrap();
        assert!(
            lines.contains("\"del\":true") && !lines.contains("yo"),
            "{lines}"
        );

        let before: Vec<_> = ["tags.json", "lines.jsonl", "vocab.json", "places.json"]
            .iter()
            .map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok())
            .collect();
        assert!(!received(&theirs, &after).unwrap(), "again: unchanged");
        let after_again: Vec<_> = ["tags.json", "lines.jsonl", "vocab.json", "places.json"]
            .iter()
            .map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok())
            .collect();
        assert_eq!(before, after_again, "no file rewritten");
    });
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "one retrain, for the first batch only"
    );
}
