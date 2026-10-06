//! End to end (OFE-15): three Macs of one vault, each with its own folder, syncing only through the
//! relay (the in-process one in link_tests.rs) while they come online in pairs, never all three at
//! once. Whatever the order, once every pair has been online together they hold the same records, a
//! deleted line stays deleted, and no frame the relay forwarded carries a word of plaintext.
use super::tests::{Relay, at, folder, mac, wait_for};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// What a Mac's folder syncs, order-independent: every record of every synced kind as canonical JSON.
fn state(dir: &Path) -> Vec<String> {
    let s = crate::merge::read_synced(&format!("{}/", dir.display()));
    let canon = |kind: &str, v: &Value| format!("{kind} {}", crate::crdt::canonical(v));
    let mut out: Vec<String> = [("tags", &s.tags), ("fixes", &s.fixes), ("vocab", &s.vocab)]
        .iter()
        .flat_map(|(k, m)| m.iter().map(move |(key, v)| canon(k, &json!({key: v}))))
        .collect();
    out.extend(
        s.places
            .iter()
            .map(|p| canon("places", &Value::Object(p.clone()))),
    );
    out.extend(
        s.lines
            .iter()
            .map(|r| canon("lines", &Value::Object(r.clone()))),
    );
    out.sort();
    out
}

fn write(dir: &Path, file: &str, v: Value) {
    std::fs::write(dir.join(file), v.to_string()).unwrap();
}

fn append_line(dir: &Path, row: Value) {
    at(dir, || {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open("lines.jsonl")
            .unwrap();
        std::io::Write::write_all(&mut f, format!("{row}\n").as_bytes()).unwrap();
    });
}

/// What each Mac edits before it first meets another: the same line tagged differently on a and b,
/// a fix on c, a new place on b, and a line of c's deleted on a.
fn three_macs() -> [tempfile::TempDir; 3] {
    let (a, b, c) = (
        folder(&["line-one@mac-a"]),
        folder(&["line-two@mac-b"]),
        folder(&["line-three@mac-c"]),
    );
    write(
        a.path(),
        "tags.json",
        json!({"line-one@mac-a": {"v": 5, "val": "Dana Levinson"}}),
    );
    write(
        b.path(),
        "tags.json",
        json!({"line-one@mac-a": {"v": 6, "val": "Noa Shapira"}}),
    );
    write(
        c.path(),
        "fixes.json",
        json!({"line-one@mac-a": {"v": 5, "val": "hello there, friend"}}),
    );
    write(
        b.path(),
        "places.json",
        json!([{"id": "p1@b", "v": 5, "action": "off", "label": "Gym on Herzl"}]),
    );
    append_line(
        a.path(),
        json!({"id": "line-three@mac-c", "v": 9, "del": true}),
    );
    [a, b, c]
}

/// Mac `x` and Mac `y` online together until they hold the same records, a line added on `x`
/// meanwhile included; then both go offline.
fn meet(
    relay: &Relay,
    key: &[u8; 32],
    macs: &[tempfile::TempDir; 3],
    x: usize,
    y: usize,
    n: usize,
    edit: bool,
) {
    let (dx, dy) = (macs[x].path(), macs[y].path());
    {
        let (_lx, _ly) = (mac(&relay.url(), dx, key), mac(&relay.url(), dy, key));
        let id = format!("{n}@m{x}");
        if edit {
            let line = json!({"id": id, "v": 1, "t": n, "text": format!("said {n}")});
            append_line(dx, line);
        }
        wait_for(
            Duration::from_secs(30),
            &format!("Macs {x} and {y} in sync"),
            || state(dx) == state(dy) && (!edit || state(dx).iter().any(|r| r.contains(&id))),
        );
    }
    // both offline before the next pair comes online: a stopping link ends on its next read
    wait_for(Duration::from_secs(30), "both offline", || {
        relay.live.0.load(Ordering::SeqCst) == 0
    });
}

/// The plaintext the fixtures put in records: ids, names, words. None may cross the relay. All are
/// 10+ bytes: a shorter one turns up in sealed (random-looking) bytes by chance across many frames.
const PLAIN: [&str; 7] = [
    "line-one@mac-a",
    "line-two@mac-b",
    "line-three@mac-c",
    "Dana Levinson",
    "Noa Shapira",
    "hello there, friend",
    "Gym on Herzl",
];

/// Runs `pairs` (each pair online alone, in order) for three fresh Macs; checks the end state.
fn schedule(seed: u8, pairs: &[(usize, usize)]) {
    let relay = Relay::start();
    let key = [100u8.wrapping_add(seed); 32]; // a vault per schedule: they run side by side
    let macs = three_macs();
    // An edit reaches all three if at least two meetings follow it: the last two of a schedule only
    // carry what's there (any two distinct pairs of three Macs share one Mac).
    for (n, &(x, y)) in pairs.iter().enumerate() {
        meet(&relay, &key, &macs, x, y, n, n + 2 < pairs.len());
    }
    let want = state(macs[0].path());
    for m in &macs[1..] {
        assert_eq!(state(m.path()), want, "schedule {seed}: {pairs:?}");
    }
    assert!(
        want.iter().any(|r| r.contains("\"val\":\"Noa Shapira\"")),
        "the later tag wins"
    );
    assert!(
        want.iter()
            .any(|r| r.contains("\"del\":true") && r.contains("line-three@mac-c")),
        "the deleted line stays deleted"
    );
    assert!(
        relay.live.1.load(Ordering::SeqCst) <= 2,
        "three Macs were online at once"
    );
    let frames = relay.frames.lock().unwrap().clone();
    assert!(!frames.is_empty());
    for f in &frames {
        for p in PLAIN {
            assert!(
                !f.windows(p.len()).any(|w| w == p.as_bytes()),
                "{p} crossed the relay"
            );
        }
    }
}

const ALL: [(usize, usize); 3] = [(0, 1), (1, 2), (0, 2)];

#[test]
fn three_macs_in_every_join_order_and_twenty_random_schedules_end_the_same() {
    let started = Instant::now();
    let mut runs: Vec<(u8, Vec<(usize, usize)>)> = vec![];
    // the six join orders: x and y meet, then y and z, then z and x
    for (i, o) in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ]
    .iter()
    .enumerate()
    {
        runs.push((i as u8, vec![(o[0], o[1]), (o[1], o[2]), (o[2], o[0])]));
    }
    // twenty seeded schedules: a few random pairs, then every pair once in a random order
    for seed in 0..20u8 {
        let mut r = u64::from(seed) * 6364136223846793005 + 1442695040888963407;
        let mut next = |n: usize| {
            r = r
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (r >> 33) as usize % n
        };
        let mut pairs: Vec<(usize, usize)> = (0..next(3)).map(|_| ALL[next(3)]).collect();
        let mut cover = ALL.to_vec();
        for i in (1..cover.len()).rev() {
            cover.swap(i, next(i + 1));
        }
        pairs.extend(cover);
        runs.push((6 + seed, pairs));
    }
    let threads: Vec<_> = runs
        .into_iter()
        .map(|(seed, pairs)| std::thread::spawn(move || schedule(seed, &pairs)))
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "took {:?}",
        started.elapsed()
    );
}
