//! Joining Macs that already have their own data (OFE-20): two folders with different histories, one
//! from before CRDTs (lines without `v`, tags and fixes as plain strings), meet through the relay for
//! the first time. Both end with the same synced files, no record twice and no tag lost, in one
//! connection each; then one retrain each gives them the same voiceprints.
use super::tests::{Relay, at, wait_for};
use super::*;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::AtomicUsize;

/// A 192-float print near person `p`'s, varied by `i`, as the transcriber writes them.
fn print(p: usize, i: usize) -> Vec<f64> {
    (0..192)
        .map(|k| (((k * (p + 2)) as f64) * 0.13).sin() + (i as f64) * 0.0005)
        .collect()
}

fn line(id: &str, p: usize, i: usize, v: Option<u64>) -> Value {
    let mut r = json!({"id": id, "t": i as f64, "d": 2.0, "src": "room", "text": format!("said {id}"), "e": print(p, i)});
    if let Some(v) = v {
        r["v"] = json!(v);
    }
    r
}

fn write_lines(dir: &Path, rows: &[Value]) {
    let text: String = rows.iter().map(|r| r.to_string() + "\n").collect();
    std::fs::write(dir.join("lines.jsonl"), text).unwrap();
}

/// What a folder syncs, order-independent: every live record of every kind as canonical JSON.
fn state(dir: &Path) -> Vec<String> {
    let s = crate::merge::read_synced(&format!("{}/", dir.display()));
    let mut out: Vec<String> = [("tags", &s.tags), ("fixes", &s.fixes), ("vocab", &s.vocab)]
        .iter()
        .flat_map(|(k, m)| {
            m.iter()
                .map(move |(key, v)| format!("{k} {key} {}", crate::crdt::canonical(v)))
        })
        .collect();
    out.extend(s.lines.iter().map(|r| {
        format!(
            "lines {}",
            crate::crdt::canonical(&Value::Object(r.clone()))
        )
    }));
    out.sort();
    out
}

/// The live tags here: line id -> name.
fn tags(dir: &Path) -> std::collections::BTreeMap<String, String> {
    at(dir, || crate::crdt::read_map("tags.json"))
        .into_iter()
        .filter_map(|(k, v)| Some((k, v.as_str()?.to_string())))
        .collect()
}

fn voiceprints(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![];
    for sub in ["voices/voices", "voices/samples"] {
        for f in std::fs::read_dir(dir.join(sub)).unwrap() {
            let p = f.unwrap().path();
            out.push((
                p.strip_prefix(dir).unwrap().display().to_string(),
                std::fs::read(&p).unwrap(),
            ));
        }
    }
    out.sort();
    out
}

#[test]
fn two_macs_with_their_own_histories_join_and_end_the_same() {
    // Mac a: months of meetings from before CRDTs; Mac b: newer, versioned data
    let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let old: Vec<Value> = (0..40)
        .map(|i| line(&format!("{i}@a"), i % 2, i, None))
        .collect();
    write_lines(a.path(), &old);
    std::fs::write(
        a.path().join("tags.json"),
        json!({"0@a": "Dana", "1@a": "Noa", "2@a": "Dana", "3@a": "Noa"}).to_string(),
    )
    .unwrap();
    std::fs::write(
        a.path().join("fixes.json"),
        json!({"fix-a": "Glorbnax"}).to_string(),
    )
    .unwrap();
    let new: Vec<Value> = (0..30)
        .map(|i| line(&format!("{i}@b"), 1 + i % 2, 100 + i, Some(5)))
        .collect();
    write_lines(b.path(), &new);
    std::fs::write(
        b.path().join("tags.json"),
        json!({"0@b": {"v": 5, "val": "Noa"}, "1@b": {"v": 5, "val": "Avi"},
               "2@b": {"v": 5, "val": "Noa"}, "3@b": {"v": 5, "val": "Avi"}})
        .to_string(),
    )
    .unwrap();
    let all_tags: std::collections::BTreeMap<String, String> =
        tags(a.path()).into_iter().chain(tags(b.path())).collect();
    for d in [&a, &b] {
        std::fs::write(d.path().join(crate::sync::run::ON), "").unwrap(); // sync is set up
    }

    let relay = Relay::start();
    let key = crate::sync::local::random().unwrap(); // both joined one vault (pairing gave b a's key)
    let retrains = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    let links: Vec<Link> = [&a, &b]
        .iter()
        .zip(&retrains)
        .map(|(d, n)| {
            let (dir, n) = (d.path().to_path_buf(), n.clone());
            // the quiet wait `ozen sync run`'s retrain uses
            let after = Coalesced::after_quiet(crate::sync::apply::QUIET, move || {
                n.fetch_add(1, Ordering::SeqCst);
            });
            start(&relay.url(), &key, after, move |step| at(&dir, step))
        })
        .collect();
    wait_for(
        Duration::from_secs(60),
        "both histories on both Macs",
        || state(a.path()) == state(b.path()) && state(a.path()).len() > 70,
    );
    std::thread::sleep(crate::sync::apply::QUIET * 2); // nothing more is coming, and the retrain ran
    drop(links);
    assert_eq!(state(a.path()), state(b.path()));
    for d in [&a, &b] {
        let ids: Vec<String> = crate::merge::read_synced(&format!("{}/", d.path().display()))
            .lines
            .iter()
            .filter_map(|r| r.get("id")?.as_str().map(String::from))
            .collect();
        let unique: std::collections::BTreeSet<&String> = ids.iter().collect();
        assert_eq!(
            (ids.len(), unique.len()),
            (70, 70),
            "every line once, none lost"
        );
        assert_eq!(tags(d.path()), all_tags, "no tag lost");
    }
    assert_eq!(
        relay.accepted.load(Ordering::SeqCst),
        2,
        "one connection each: one session"
    );
    for (i, n) in retrains.iter().enumerate() {
        assert_eq!(
            n.load(Ordering::SeqCst),
            1,
            "Mac {i}: one retrain after the whole exchange"
        );
    }

    // the one retrain each, as `ozen retrain` runs it after a received batch
    for d in [&a, &b] {
        at(d.path(), || crate::train::retrain(true));
    }
    let (va, vb) = (voiceprints(a.path()), voiceprints(b.path()));
    assert_eq!(va.len(), 6, "Dana, Noa and Avi: a print and samples each");
    assert_eq!(va, vb, "the same voiceprints on both Macs");
}
