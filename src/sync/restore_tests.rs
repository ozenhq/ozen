use super::*;
use crate::merge::read_synced;
use crate::sync::apply::{Coalesced, received};
use serde_json::json;

/// Runs `f` in a fresh ozen folder holding one line and one tag.
fn in_folder<T>(f: impl FnOnce(&Path) -> T) -> T {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("lines.jsonl"),
        "{\"id\":\"1@a\",\"t\":1.0,\"text\":\"hi\",\"v\":1}\n",
    )
    .unwrap();
    std::fs::write(
        d.path().join("tags.json"),
        json!({"1@a": {"v": 1, "val": "Dana"}}).to_string(),
    )
    .unwrap();
    let _cwd = crate::CWD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(d.path()).unwrap();
    let r = f(d.path());
    std::env::set_current_dir(back).unwrap();
    r
}

/// A batch from another Mac with `n` new lines.
fn batch(n: usize) -> Synced {
    let lines = (0..n)
        .map(|i| {
            json!({"id": format!("{i}@b"), "t": i as f64, "text": "x", "v": 1})
                .as_object()
                .unwrap()
                .clone()
        })
        .collect();
    Synced {
        lines,
        ..Default::default()
    }
}

fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut s: Vec<(String, Vec<u8>)> = files()
        .filter_map(|f| Some((f.to_string(), std::fs::read(dir.join(f)).ok()?)))
        .collect();
    s.sort();
    s
}

#[test]
fn a_big_batch_takes_a_restore_point_and_a_small_one_doesnt() {
    in_folder(|_| {
        received(&batch(10), &Coalesced::new(|| {})).unwrap();
        assert!(points().is_empty(), "10 records: no restore point");
        received(&batch(500), &Coalesced::new(|| {})).unwrap();
        assert_eq!(points().len(), 1, "500 records: one restore point");
        assert_eq!(
            read_synced("").lines.len(),
            1 + 500,
            "the batches share ids 0..9"
        );
    });
}

#[test]
fn undo_restores_the_files_byte_for_byte_and_pauses_sync() {
    in_folder(|dir| {
        let before = snapshot(dir);
        received(&batch(500), &Coalesced::new(|| {})).unwrap();
        assert_ne!(snapshot(dir), before);
        let said = undo().unwrap();
        assert_eq!(snapshot(dir), before, "byte-identical");
        assert!(Path::new(PAUSED).exists(), "sync paused: {said}");
        assert_eq!(
            points().len(),
            2,
            "what was there before the undo is a restore point too"
        );
    });
}

#[test]
fn undo_with_no_restore_point_says_so_and_changes_nothing() {
    in_folder(|dir| {
        let before = snapshot(dir);
        assert!(undo().unwrap_err().contains("no restore point"));
        assert_eq!(snapshot(dir), before);
        assert!(!Path::new(PAUSED).exists());
    });
}

#[test]
fn restore_points_past_five_or_a_week_old_are_pruned() {
    in_folder(|_| {
        let now = now();
        let day = 24 * 60 * 60;
        let ages = [
            9 * day,
            8 * day,
            6 * day,
            5 * day,
            4 * day,
            3 * day,
            2 * day,
            day,
        ];
        for a in ages {
            std::fs::create_dir_all(Path::new(DIR).join((now - a).to_string())).unwrap();
        }
        prune(now);
        let left: Vec<u64> = points().iter().map(|(t, _)| now - t).collect();
        assert_eq!(
            left,
            [5 * day, 4 * day, 3 * day, 2 * day, day],
            "the newest five, none past a week"
        );
    });
}

#[test]
fn a_restore_point_past_a_week_old_is_pruned_even_with_room_for_it() {
    in_folder(|_| {
        let (now, day) = (now(), 24 * 60 * 60);
        for a in [8 * day, day] {
            std::fs::create_dir_all(Path::new(DIR).join((now - a).to_string())).unwrap();
        }
        prune(now);
        let left: Vec<u64> = points().iter().map(|(t, _)| now - t).collect();
        assert_eq!(left, [day]);
    });
}
