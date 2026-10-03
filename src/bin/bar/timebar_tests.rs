use super::*;

struct Utc;
impl Clock for Utc {
    fn time(&self, t: f64, secs: bool) -> String {
        let d = chrono::DateTime::from_timestamp(t.floor() as i64, 0).unwrap();
        d.format(if secs { "%H:%M:%S" } else { "%H:%M" })
            .to_string()
    }
    fn date(&self, t: f64) -> String {
        let d = chrono::DateTime::from_timestamp(t.floor() as i64, 0).unwrap();
        d.format("%-d.%-m.%Y").to_string()
    }
    fn day_time(&self, t: f64) -> String {
        let d = chrono::DateTime::from_timestamp(t.floor() as i64, 0).unwrap();
        d.format("%a %H:%M").to_string()
    }
    fn utc_minus_local(&self) -> f64 {
        0.0
    }
}

fn chunk(t: f64, tag: &str, state: &str, done: Option<f64>, took: Option<f64>) -> Chunk {
    Chunk {
        t,
        tag: tag.into(),
        state: state.into(),
        done,
        took,
        ..Default::default()
    }
}

#[test]
fn formats_like_the_page() {
    assert_eq!(fmt_dur(None), "–");
    assert_eq!(fmt_dur(Some(f64::INFINITY)), "–");
    assert_eq!(fmt_dur(Some(2.5)), "3s"); // Math.round: halves up
    assert_eq!(fmt_dur(Some(-2.5)), "-2s");
    assert_eq!(fmt_dur(Some(-0.4)), "0s");
    assert_eq!(fmt_dur(Some(89.4)), "89s");
    assert_eq!(fmt_dur(Some(150.0)), "3m"); // 2.5 minutes, up
    assert_eq!(fmt_dur(Some(5400.0)), "1.5h");
    assert_eq!(fixed1(0.25), "0.3"); // toFixed: an exact tie goes up
    assert_eq!(fixed1(0.35), "0.3"); // 0.35 is really 0.34999…
    assert_eq!(fixed1(2.0), "2.0");
}

#[test]
fn zoom_keeps_the_pointer_and_pans_back_to_live() {
    let now = 10_000.0;
    let mut n = Nav::default();
    n.zoom(0.5, None, now); // the + button: keep following now
    assert_eq!(
        n,
        Nav {
            span: 900.0,
            end: None
        }
    );
    n.pan(-300.0, now);
    assert_eq!(n.end, Some(9_700.0));
    n.zoom(0.5, Some(9_250.0), now); // 9250 stays where it was, halfway across
    assert_eq!(
        n,
        Nav {
            span: 450.0,
            end: Some(9_475.0)
        }
    );
    n.zoom(1e-9, None, now);
    assert_eq!(n.span, 120.0);
    n.pan(1e9, now); // past now: live again
    assert_eq!(n.end, None);
}

#[test]
fn stats_and_charts() {
    let now = 3_600.0;
    let d = Data {
        now,
        chunks: vec![
            chunk(3_000.0, "call", "done", Some(3_030.0), Some(1.5)),
            chunk(3_000.0, "mic", "old", None, None),
            chunk(3_300.0, "local", "error", Some(3_320.0), Some(0.5)),
            chunk(3_500.0, "call", "waiting", None, None),
            chunk(3_550.0, "other", "waiting", None, None),
        ],
    };
    let (a, b) = Nav::default().range(now);
    let s = stats(&d, a, b, &Utc);
    let v: Vec<&str> = s.iter().map(|r| r[1].as_str()).collect();
    assert_eq!(v, ["5", "2", "2", "1", "15s", "15.0×", "0.2/min", "10m"]);
    assert_eq!(s[2][2], "30s audio");
    assert_eq!(s[7][2], "oldest 00:58");
    assert_eq!(
        range_label(&Nav::default(), now, &Utc),
        "Thu 00:30 – 01:00 · live"
    );
    let (ops, hits) = lanes(&d, a, b, 664.0, &Utc);
    assert_eq!(hits.len(), 4); // "other" has no lane
    // 30 min over 600px: a 15s chunk is 5px, drawn 0.5px short of its end
    assert_eq!(hits[0], (GUTTER + 400.0, GUTTER + 404.5, 3.0, 23.0, 0));
    assert!(ops.contains(&Op::Fill(Col::Done, GUTTER + 400.0, 3.0, 4.5, 20.0)));
    assert!(ops.contains(&Op::Text(Col::Dim, Align::Left, 0.0, 43.0, "room".into())));
    // 300s ticks (60s would be 20px apart): 00:30 to 01:00 is seven of them
    let grid = ops
        .iter()
        .filter(|o| matches!(o, Op::Stroke(Col::Grid, ..)))
        .count();
    assert_eq!(grid, 7);
    assert!(ops.contains(&Op::Stroke(
        Col::Now,
        2.0,
        vec![(664.0, 0.0), (664.0, 82.0)]
    )));
    let p = pace(&d, a, b, 664.0, &Utc);
    // behind: 15s (done) and 5s (skipped), under the 60s floor
    assert!(p.contains(&Op::Dot(
        Col::Done,
        GUTTER + 405.0,
        132.0 - 15.0 / 60.0 * 124.0
    )));
    assert!(p.contains(&Op::Dot(
        Col::Error,
        GUTTER + 505.0,
        132.0 - 5.0 / 60.0 * 124.0
    )));
    assert!(p.contains(&Op::Text(
        Col::Line,
        Align::Left,
        GUTTER + 4.0,
        16.0,
        "3 waiting".into()
    )));
    assert_eq!(
        tip(&d.chunks[0], &Utc),
        "call 00:50:00–00:50:15\ndone\ndone 00:50:30, 15s after recording\ntook 1.5s"
    );
}
