use super::*;

fn facts<'a>(
    mode: &'a str,
    state: &'a str,
    place: Option<&'a Place>,
    mic: Option<&'a str>,
) -> Facts<'a> {
    Facts {
        now: 1000.0,
        since_launch: 100.0,
        mic_app: mic,
        place,
        located: true,
        places_set: true,
        mode,
        state,
    }
}

fn place(action: &str) -> Place {
    Place {
        label: "Home".into(),
        action: action.into(),
    }
}

#[test]
fn always_starts_once_and_manual_stop_sticks() {
    let mut m = Memory::default();
    assert_eq!(
        decide(&facts("always", "stopped", None, None), &mut m),
        Act::Start
    );
    assert_eq!(
        decide(&facts("always", "stopped", None, None), &mut m),
        Act::None
    ); // stopped by hand: stays
}

#[test]
fn first_decision_never_stops_a_running_recording() {
    let mut m = Memory::default();
    assert_eq!(
        decide(&facts("meetings", "recording", None, None), &mut m),
        Act::None
    );
    assert!(m.adopted);
    // a meeting starts then ends: now it may stop
    let mut f = facts("meetings", "recording", None, Some("Zoom"));
    assert_eq!(decide(&f, &mut m), Act::None); // wanted, already recording
    f.mic_app = None;
    f.now += MEETING_GRACE + 1.0;
    assert_eq!(decide(&f, &mut m), Act::Stop);
}

#[test]
fn meetings_mode_follows_the_mic_with_grace() {
    let mut m = Memory {
        adopted: true,
        last_wanted: Some(false),
        ..Default::default()
    };
    let mut f = facts("meetings", "stopped", None, Some("Chrome"));
    assert_eq!(decide(&f, &mut m), Act::Start);
    assert_eq!(m.meeting_name.as_deref(), Some("Chrome"));
    f.mic_app = None;
    f.state = "recording";
    f.now += 10.0; // mic dropped briefly
    assert_eq!(decide(&f, &mut m), Act::None);
}

#[test]
fn a_place_overrides_the_mode_and_arriving_applies_at_once() {
    let off = place("off");
    let mut m = Memory {
        adopted: true,
        last_wanted: Some(false),
        ..Default::default()
    };
    // Always mode, but at a place set to off: stop
    assert_eq!(
        decide(&facts("always", "recording", Some(&off), None), &mut m),
        Act::Stop
    );
    // paused by hand, then leave the place: the mode applies again right away
    assert_eq!(
        decide(&facts("always", "paused", None, None), &mut m),
        Act::Start
    );
    let meetings = place("meetings");
    let mut m = Memory {
        adopted: true,
        ..Default::default()
    };
    assert_eq!(
        decide(&facts("always", "stopped", Some(&meetings), None), &mut m),
        Act::None
    );
    assert_eq!(
        decide(
            &facts("always", "stopped", Some(&meetings), Some("Teams")),
            &mut m
        ),
        Act::Start
    );
}

#[test]
fn starts_while_processing_a_backlog() {
    // a backlog still being transcribed must not block a new recording (#76)
    let mut m = Memory::default();
    assert_eq!(
        decide(&facts("always", "processing", None, None), &mut m),
        Act::Start
    );
}

#[test]
fn waits_for_a_location_right_after_launch() {
    let mut m = Memory::default();
    let mut f = facts("always", "stopped", None, None);
    f.located = false;
    f.since_launch = 5.0;
    assert_eq!(decide(&f, &mut m), Act::None);
    assert_eq!(m.last_wanted, None);
    f.since_launch = 31.0; // gave up waiting
    assert_eq!(decide(&f, &mut m), Act::Start);
}
