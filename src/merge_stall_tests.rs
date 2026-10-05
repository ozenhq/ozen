//! A big received batch (a whole history on join) must never stall the transcriber's appends (OFE-54).
use super::*;
use std::io::Write;
use std::time::{Duration, Instant};

/// A line as the transcriber writes it: text plus a 192-float voiceprint, about 4 KB.
fn line(id: &str, t: f64) -> Row {
    let e: Vec<f64> = (0..192)
        .map(|i| ((i as f64) * 0.37 + t).sin() / 9.0)
        .collect();
    json!({"id": id, "t": t, "d": 2.5, "src": "room", "spk": "S1", "text": format!("said {id}"), "e": e})
        .as_object()
        .unwrap()
        .clone()
}

/// Appends one line the way the transcriber does (`transcribe::open_lines`: append, take the lock, and
/// retry if the file was swapped meanwhile); returns how long it waited for the lock.
fn append(n: usize) -> Duration {
    use std::os::unix::fs::MetadataExt;
    let asked = Instant::now();
    let mut f = loop {
        let f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(LINES)
            .unwrap();
        f.lock().unwrap();
        if fs::metadata(LINES).is_ok_and(|m| m.ino() == f.metadata().unwrap().ino()) {
            break f;
        }
    };
    let waited = asked.elapsed();
    let row = Value::Object(line(&format!("live-{n}"), 9e9 + n as f64));
    writeln!(f, "{}", crate::crdt::canonical(&row)).unwrap();
    waited
}

#[test]
fn applying_twenty_thousand_records_never_delays_a_live_append_past_200ms() {
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    let d = tempfile::tempdir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    let mine: String = (0..1000)
        .map(|i| {
            crate::crdt::canonical(&Value::Object(line(&format!("{i}@here"), i as f64))) + "\n"
        })
        .collect();
    fs::write(LINES, &mine).unwrap();
    let theirs = Synced {
        lines: (0..20_000)
            .map(|i| line(&format!("{i}@there"), i as f64))
            .collect(),
        ..Default::default()
    };

    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = done.clone();
    let writer = std::thread::spawn(move || {
        let (mut worst, mut n) = (Duration::ZERO, 0);
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            worst = worst.max(append(n));
            n += 1;
            std::thread::sleep(Duration::from_millis(100));
        }
        (worst, n)
    });
    let started = Instant::now();
    let applied = apply(&theirs);
    let took = started.elapsed();
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let (worst, appended) = writer.join().unwrap();
    let rows = parse_jsonl(&fs::read_to_string(LINES).unwrap());
    std::env::set_current_dir(back).unwrap();

    applied.unwrap();
    eprintln!("apply took {took:?}; worst wait of {appended} live appends {worst:?}");
    // nothing lost, and the same data a single locked merge of everything gives
    assert_eq!(rows.len(), 1000 + 20_000 + appended, "apply took {took:?}");
    let live: Vec<Row> = (0..appended)
        .map(|n| line(&format!("live-{n}"), 9e9 + n as f64))
        .collect();
    let all = merge_rows(&merge_rows(&parse_jsonl(&mine), &theirs.lines), &live);
    assert_eq!(merge_rows(&rows, &[]), all);
    assert!(
        worst <= Duration::from_millis(200),
        "a live append waited {worst:?} for the lock (apply took {took:?}, {appended} appends)"
    );
}

#[test]
fn files_swapped_in_while_merging_lose_nothing() {
    // `ozen mcp` edits rewrite lines.jsonl and swap it in under the lock: the merge must start over
    // (or fall back to merging under the lock) and keep every edit
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    let d = tempfile::tempdir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    let mine: Vec<Row> = (0..200)
        .map(|i| line(&format!("{i}@here"), i as f64))
        .collect();
    fs::write(LINES, jsonl(&mine)).unwrap();
    let theirs = Synced {
        lines: (0..2000)
            .map(|i| line(&format!("{i}@there"), i as f64))
            .collect(),
        ..Default::default()
    };
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop = done.clone();
    let rewriter = std::thread::spawn(move || {
        let mut n = 0;
        while !stop.load(std::sync::atomic::Ordering::SeqCst) {
            let _lock = locked().unwrap();
            let mut rows = parse_jsonl(&fs::read_to_string(LINES).unwrap());
            rows.push(line(&format!("note-{n}"), 8e9 + n as f64));
            write_atomic(LINES, jsonl(&rows).as_bytes()).unwrap();
            n += 1;
            std::thread::sleep(Duration::from_millis(50));
        }
        n
    });
    let applied = apply(&theirs);
    done.store(true, std::sync::atomic::Ordering::SeqCst);
    let notes = rewriter.join().unwrap();
    let rows = parse_jsonl(&fs::read_to_string(LINES).unwrap());
    std::env::set_current_dir(back).unwrap();
    applied.unwrap();
    let edits: Vec<Row> = (0..notes)
        .map(|n| line(&format!("note-{n}"), 8e9 + n as f64))
        .collect();
    let all = merge_rows(&merge_rows(&mine, &theirs.lines), &edits);
    assert_eq!(
        merge_rows(&rows, &[]),
        all,
        "{notes} rewrites during the merge"
    );
}
