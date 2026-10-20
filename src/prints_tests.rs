use super::*;
use proptest::prelude::*;
use serde_json::json;

/// Runs `f` in a fresh folder, as ozen runs from its own.
fn in_folder<T>(f: impl FnOnce() -> T) -> T {
    let _cwd = crate::CWD.lock().unwrap_or_else(PoisonError::into_inner);
    let back = std::env::current_dir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    std::env::set_current_dir(dir.path()).unwrap();
    let r = f();
    std::env::set_current_dir(back).unwrap();
    r
}

fn line(i: usize) -> String {
    json!({"id": format!("{i}@m"), "t": i, "d": 1.5, "text": "said", "e": [i as f64 / 7.0, -0.25, 1e-9]})
        .to_string()
        + "\n"
}

fn append(text: &str) {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(LINES)
        .unwrap()
        .write_all(text.as_bytes())
        .unwrap();
}

/// What a parse of the whole file gives, ignoring the cache.
fn fresh() -> Vec<Print> {
    parse(&fs::read(LINES).unwrap_or_default())
}

/// The cache's (offset covered, record count).
fn header() -> (u64, u64) {
    let b = fs::read(CACHE).unwrap();
    let n = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().unwrap());
    (n(16), n(24))
}

#[test]
fn appends_parse_only_the_tail_and_match_a_full_parse() {
    in_folder(|| {
        append(&(0..50).map(line).collect::<String>());
        assert_eq!(read(), fresh());
        assert_eq!(header(), (fs::metadata(LINES).unwrap().len(), 50));
        append(&(50..60).map(line).collect::<String>());
        assert_eq!(read(), fresh());
        assert_eq!(
            header().1,
            60,
            "the 10 appended lines were added to the cache"
        );
        assert_eq!(read(), fresh(), "a warm cache with nothing new");
    });
}

#[test]
fn a_swapped_file_rebuilds_the_cache() {
    in_folder(|| {
        append(&(0..20).map(line).collect::<String>());
        read();
        // a rewrite (an edit or a merge) swaps in a new file, here a shorter one with other lines
        fs::write("new", (100..105).map(line).collect::<String>()).unwrap();
        fs::rename("new", LINES).unwrap();
        assert_eq!(read(), fresh());
        assert_eq!(header().1, 5);
    });
}

#[test]
fn a_deleted_or_corrupt_cache_is_rebuilt() {
    in_folder(|| {
        append(&(0..30).map(line).collect::<String>());
        read();
        fs::remove_file(CACHE).unwrap();
        assert_eq!(read(), fresh(), "deleted");
        let mut b = fs::read(CACHE).unwrap();
        b.truncate(b.len() - 5);
        fs::write(CACHE, &b).unwrap();
        assert_eq!(read(), fresh(), "truncated");
        fs::write(CACHE, b"not a cache at all, just bytes").unwrap();
        assert_eq!(read(), fresh(), "garbage");
        let mut b = fs::read(CACHE).unwrap();
        b[24..32].copy_from_slice(&u64::MAX.to_le_bytes()); // a count far past what's there
        fs::write(CACHE, &b).unwrap();
        assert_eq!(read(), fresh(), "impossible count");
    });
}

#[test]
fn a_crash_between_appending_records_and_the_header_loses_nothing() {
    in_folder(|| {
        append(&(0..10).map(line).collect::<String>());
        read();
        // records appended, header not yet updated: junk past the header's count
        OpenOptions::new()
            .append(true)
            .open(CACHE)
            .unwrap()
            .write_all(&[7; 100])
            .unwrap();
        append(&(10..12).map(line).collect::<String>());
        assert_eq!(read(), fresh());
        assert_eq!(read(), fresh(), "and again, from the repaired cache");
        assert_eq!(header().1, 12);
    });
}

#[test]
fn a_line_still_being_written_counts_but_is_cached_only_once_whole() {
    in_folder(|| {
        append(&(0..3).map(line).collect::<String>());
        let half = line(3);
        let (a, b) = half.split_at(half.len() - 1); // everything but the newline
        append(a);
        assert_eq!(read(), fresh());
        assert_eq!(header().1, 3);
        append(b);
        assert_eq!(read(), fresh(), "no duplicate once the line is whole");
        assert_eq!(header().1, 4);
    });
}

/// A lines.jsonl row as ozen or an older/odd writer might leave it.
fn row() -> impl Strategy<Value = String> {
    let id = prop_oneof![
        Just(None),
        (0u8..6).prop_map(|i| Some(json!(format!("{i}@m")))), // repeats: duplicate ids
        Just(Some(json!(7))),
    ];
    let d = prop_oneof![
        Just(None),
        (0.0f64..3.0).prop_map(|d| Some(json!(d))),
        Just(Some(json!("2.5")))
    ];
    let e = prop_oneof![
        Just(None),
        Just(Some(Value::Null)),
        Just(Some(json!([]))),
        proptest::collection::vec(-1.0f64..1.0, 1..6).prop_map(|v| Some(json!(v))),
    ];
    (id, d, e).prop_map(|(id, d, e)| {
        let mut r = json!({"t": 1.0, "text": "said"});
        for (k, v) in [("id", id), ("d", d), ("e", e)] {
            if let Some(v) = v {
                r[k] = v;
            }
        }
        r.to_string()
    })
}

proptest! {
    /// Through the cache, cold and warm, the prints equal what src/train.rs and src/ignore.rs read before
    /// it existed: JSON maps (`fixes::lines`), then id, d and e picked out.
    #[test]
    fn equal_the_map_parse_on_any_rows(
        first in proptest::collection::vec(row(), 0..30),
        more in proptest::collection::vec(row(), 0..10),
    ) {
        in_folder(|| {
            let before = || -> Vec<Print> {
                crate::fixes::lines()
                    .into_iter()
                    .filter_map(|r| {
                        let e = r.get("e")?.as_array()?.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect();
                        Some(Print {
                            id: r.get("id")?.as_str()?.to_string(),
                            d: r.get("d").and_then(Value::as_f64),
                            e,
                        })
                    })
                    .collect()
            };
            append(&first.iter().map(|r| r.clone() + "\n").collect::<String>());
            assert_eq!(read(), before());
            append(&more.iter().map(|r| r.clone() + "\n").collect::<String>());
            assert_eq!(read(), before());
        });
    }
}

/// On 50k lines with 192-float prints (100 MB), reading the prints from a warm cache. Ignored by default
/// (it builds 100 MB); run by hand: `cargo nextest run --release --run-ignored only -E 'test(warm_cache)'`.
#[test]
#[ignore]
fn fifty_thousand_lines_from_a_warm_cache_in_30ms() {
    in_folder(|| {
        let e: Vec<f64> = (0..192).map(|i| (i as f64 * 0.37).sin() / 9.0).collect();
        let text: String = (0..50_000)
            .map(|i| {
                json!({"id": format!("{i}@m"), "t": i, "d": 2.5, "text": "said", "e": e})
                    .to_string()
                    + "\n"
            })
            .collect();
        append(&text);
        let t = std::time::Instant::now();
        let cold = read();
        let parse = t.elapsed();
        let t = std::time::Instant::now();
        let warm = read();
        let cached = t.elapsed();
        eprintln!("50k lines: full parse {parse:?}, warm cache {cached:?}");
        assert_eq!(warm, cold);
        assert!(cached.as_millis() <= 30, "warm cache took {cached:?}");
    });
}
