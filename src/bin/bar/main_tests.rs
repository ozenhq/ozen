use super::{idle_status, meeting_cell, menu_header, transcribe_switched};

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

#[test]
fn idle_status_says_what_ozen_is_doing_once() {
    assert_eq!(idle_status("stopped", 0, false, ""), "Not recording");
    assert_eq!(
        idle_status("stopped", 0, true, " · Work"),
        "Waiting for a meeting · Work"
    );
    assert_eq!(idle_status("paused", 0, false, " · Work"), "Paused");
    assert_eq!(
        idle_status("processing", 1, false, ""),
        "Transcribing 1 chunk…"
    );
    assert_eq!(
        idle_status("processing", 3, false, ""),
        "Transcribing 3 chunks…"
    );
}

#[test]
fn the_menu_opens_with_the_panel_status() {
    assert_eq!(
        menu_header("● Recording · Work", "recording"),
        "● Recording · Work"
    );
    assert_eq!(menu_header("", "stopped"), "stopped");
}
