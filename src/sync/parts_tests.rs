use super::*;
use serde_json::json;

const BUDGET: usize = 60 << 10;

/// A note of about `kib` KiB, as an agent might write through MCP.
fn note(kib: usize) -> Value {
    let text: String = (0..kib * 1024)
        .map(|i| (b'a' + (i % 26) as u8) as char)
        .collect();
    json!({"id": "n1@a", "v": 7, "t": 1.0, "text": text})
}

fn assemble(parts: Vec<Part>) -> Result<Option<(String, String, Value)>, String> {
    let (mut p, now) = (Pending::default(), Instant::now());
    let mut last = Ok(None);
    for part in parts {
        last = p.add(part, BUDGET, now);
    }
    last
}

#[test]
fn a_300_kib_record_splits_and_reassembles_byte_identical() {
    let r = note(300);
    let mut parts = split("lines", "n1@a", &r, BUDGET).unwrap();
    assert!(parts.len() >= 6, "{} parts", parts.len());
    for p in &parts {
        assert!(
            serde_json::to_vec(p).unwrap().len() < BUDGET,
            "a part fits a frame"
        );
    }
    parts.reverse(); // any order
    let (k, key, got) = assemble(parts).unwrap().expect("complete");
    assert_eq!((k.as_str(), key.as_str()), ("lines", "n1@a"));
    assert_eq!(
        serde_json::to_vec(&got).unwrap(),
        serde_json::to_vec(&r).unwrap()
    );
}

#[test]
fn parts_that_never_complete_expire_after_the_timeout() {
    let mut parts = split("lines", "n1@a", &note(200), BUDGET).unwrap();
    parts.pop(); // the last part never comes
    let (mut p, now) = (Pending::default(), Instant::now());
    for part in parts {
        assert_eq!(p.add(part, BUDGET, now).unwrap(), None);
    }
    assert_eq!(p.expire(now + TIMEOUT - Duration::from_secs(1)), 0);
    assert_eq!(p.expire(now + TIMEOUT), 1);
}

#[test]
fn the_cap_is_enforced_both_ways() {
    assert!(
        split("lines", "n1@a", &note(2100), BUDGET).is_err(),
        "over 2 MiB isn't sent"
    );
    let big = split("lines", "n1@a", &note(1500), BUDGET).unwrap();
    // a peer claiming more parts than a 2 MiB record needs, or an index past n, is refused
    let mut liar = big[0].clone();
    liar.n = 1_000;
    assert!(
        Pending::default()
            .add(liar, BUDGET, Instant::now())
            .is_err()
    );
    let mut past = big[0].clone();
    past.i = past.n;
    assert!(
        Pending::default()
            .add(past, BUDGET, Instant::now())
            .is_err()
    );
}

#[test]
fn at_most_a_few_records_wait_in_parts() {
    let (mut p, now) = (Pending::default(), Instant::now());
    for i in 0..PENDING {
        let first = split("lines", &format!("n{i}"), &note(100), BUDGET)
            .unwrap()
            .remove(0);
        assert_eq!(p.add(first, BUDGET, now).unwrap(), None);
    }
    let one_more = split("lines", "n9", &note(100), BUDGET).unwrap().remove(0);
    assert!(p.add(one_more, BUDGET, now).is_err());
}

#[test]
fn a_record_that_does_not_match_its_parts_is_refused() {
    let mut parts = split("lines", "n1@a", &note(100), BUDGET).unwrap();
    for p in &mut parts {
        p.v = 8; // the record inside says 7
    }
    assert!(assemble(parts).is_err());
}

#[test]
fn two_macs_records_at_the_same_version_never_mix() {
    // an equal-version conflict: both arrive in parts, interleaved; each reassembles to itself
    let mut other = note(150);
    other["text"] = json!("z".repeat(150 * 1024));
    let (a, b) = (
        split("lines", "n1@a", &note(150), BUDGET).unwrap(),
        split("lines", "n1@a", &other, BUDGET).unwrap(),
    );
    assert_eq!(a.len(), b.len());
    let (mut p, now) = (Pending::default(), Instant::now());
    let mut done = vec![];
    for (x, y) in a.into_iter().zip(b) {
        done.extend(p.add(x, BUDGET, now).unwrap());
        done.extend(p.add(y, BUDGET, now).unwrap());
    }
    let got: Vec<Value> = done.into_iter().map(|(_, _, r)| r).collect();
    assert_eq!(got.len(), 2);
    assert!(got.contains(&note(150)) && got.contains(&other));
}
