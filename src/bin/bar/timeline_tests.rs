use super::*;

fn seg(id: &str, t: f64, d: f64, speaker: &str) -> Segment {
    Segment {
        id: id.into(),
        t,
        d,
        speaker: speaker.into(),
        text: String::new(),
        unsure: false,
    }
}

#[test]
fn lays_out_lanes_by_talk_time_and_squeezes_long_silences() {
    let segs = [
        seg("a", 100.0, 5.0, "Dana"),
        seg("b", 103.0, 2.0, "Omer"),
        seg("c", 400.0, 10.0, "Omer"),
    ];
    let l = layout(&segs, 4.0);
    assert_eq!(l.lanes, ["Omer", "Dana"]);
    let x0 = GUTTER + 12.0;
    assert_eq!(l.bars[0].0.origin.x, x0); // a starts the axis
    assert_eq!(l.bars[1].0.origin.x, x0 + 12.0); // b overlaps a, 3s in
    assert_eq!(l.breaks, [(x0 + 20.0 + 4.0, 295.0)]); // 105 -> 400 is squeezed to a break
    assert_eq!(l.bars[2].0.origin.x, x0 + 20.0 + BREAK_W);
    assert_eq!(
        l.spans,
        [(100.0, 105.0, x0), (400.0, 410.0, x0 + 20.0 + BREAK_W)]
    );
    assert_eq!(l.bars[1].0.size.width, 8.0);
    assert_eq!(
        talk(&segs, &l.lanes),
        [(12.0, 12.0 / 17.0), (5.0, 5.0 / 17.0)]
    );
    let quiet = [
        seg("a", 0.0, 90.0, "S1"),
        seg("b", 90.0, 5.0, "S2"),
        seg("c", 95.0, 70.0, "?"),
    ];
    let q = layout(&quiet, 4.0);
    assert_eq!(q.lanes, ["S1", OTHERS]); // S1 talked a minute and a half: its own lane
    assert_eq!(q.bars[1].0.origin.y, q.bars[2].0.origin.y); // S2 and ? share Other voices
    assert_eq!(talk(&quiet, &q.lanes)[1].0, 75.0); // ? talked over a minute and still folds
    let px = fit_px(&segs, 600.0);
    assert!((layout(&segs, px).width - 600.0).abs() < 1e-6); // fitted: exactly the width
    assert_eq!(
        layout(&[seg("x", 1.0, 0.1, "S1")], 4.0).bars[0]
            .0
            .size
            .width,
        3.0
    ); // at least 3px wide
}
