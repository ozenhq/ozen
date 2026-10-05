use super::{meeting_cell, transcribe_switched};

#[test]
fn transcribe_switch_stops_or_catches_up() {
    assert_eq!(
        transcribe_switched(false, "recording", 0),
        [&["record"][..], &["process", "stop"]]
    );
    assert_eq!(
        transcribe_switched(false, "processing", 3),
        [&["process", "stop"][..]]
    );
    assert!(transcribe_switched(false, "stopped", 3).is_empty());
    assert_eq!(transcribe_switched(true, "recording", 3), [&["start"][..]]);
    assert_eq!(transcribe_switched(true, "stopped", 3), [&["process"][..]]);
    assert!(transcribe_switched(true, "stopped", 0).is_empty());
}

#[test]
fn a_meeting_under_a_minute_says_so() {
    let m = |mins: &str| {
        vec![vec![
            "1".into(),
            "Mon".into(),
            mins.to_string(),
            "1".into(),
            "hi".into(),
        ]]
    };
    assert_eq!(meeting_cell(&m("0"), "Min", 0), "<1");
    assert_eq!(meeting_cell(&m("3"), "Min", 0), "3");
    assert_eq!(meeting_cell(&m("0"), "Lines", 0), "1");
}
