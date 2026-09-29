//! When to start or stop recording by itself: the record mode (Always, or Meetings: while a meeting app uses the
//! microphone), overridden by the place you're at (auto record, record meetings only, or auto off).
//! Acts only when "should be recording" flips, so a manual Pause or Stop sticks until the next flip.

/// A meeting app released the mic less than this long ago: still the same meeting (mute toggles, reconnects).
pub const MEETING_GRACE: f64 = 20.0;
/// Just launched with located places but no location yet: wait this long before the mode alone decides.
const LOCATION_WAIT: f64 = 30.0;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Place {
    pub label: String,
    pub action: String, // "record" | "meetings" | "off"
}

/// What the app remembers between decisions.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Memory {
    pub last_meeting: Option<f64>, // when a meeting app last held the mic
    pub meeting_name: Option<String>,
    pub place_label: Option<String>, // a change applies right away
    pub last_wanted: Option<bool>,
    pub adopted: bool, // made the first decision since launch
}

/// What the app sees now.
pub struct Facts<'a> {
    pub now: f64,
    pub since_launch: f64,
    pub mic_app: Option<&'a str>,
    pub place: Option<&'a Place>,
    pub located: bool,    // a location fix has arrived
    pub places_set: bool, // some place has coordinates
    pub mode: &'a str,    // "always" | "meetings"
    pub state: &'a str,   // recorder state from `ozen status`
}

#[derive(Debug, PartialEq)]
pub enum Act {
    Start,
    Stop,
    None,
}

pub fn in_meeting(m: &Memory, now: f64) -> bool {
    m.last_meeting.is_some_and(|t| now - t < MEETING_GRACE)
}

pub fn decide(f: &Facts, m: &mut Memory) -> Act {
    if let Some(app) = f.mic_app {
        m.last_meeting = Some(f.now);
        m.meeting_name = Some(app.to_string());
    }
    let meeting = in_meeting(m, f.now);
    let label = f.place.map(|p| p.label.clone());
    if label != m.place_label {
        m.place_label = label;
        m.last_wanted = None; // arriving at or leaving a place applies right away
    }
    // Just launched, places set, no location yet: wait rather than let the global mode decide for a place we can't
    // see yet, e.g. start recording at a place set to meetings only.
    if f.place.is_none() && !f.located && f.since_launch < LOCATION_WAIT && f.places_set {
        return Act::None;
    }
    let wanted = match f.place {
        Some(p) => p.action == "record" || p.action == "meetings" && meeting,
        None => f.mode == "always" || meeting,
    };
    let flipped = m.last_wanted != Some(wanted);
    m.last_wanted = Some(wanted);
    if !flipped {
        return Act::None;
    }
    // The first decision after launch may start recording, never stop one: a recording already running was started
    // by hand (or by the previous app), and a relaunch or rebuild shouldn't end it.
    if !m.adopted {
        m.adopted = true;
        if !wanted {
            return Act::None;
        }
    }
    match (wanted, f.state) {
        (true, "stopped" | "paused" | "processing") => Act::Start,
        (false, "recording" | "paused") => Act::Stop,
        _ => Act::None,
    }
}

#[cfg(test)]
mod tests {
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
}
