//! `ozen auto '<json>'`: when the menu bar app should start or stop recording on its own. The app knows the
//! live facts (mode, whether a meeting app holds the mic, recorder state); this decides from them, where you
//! are (here.json, kept by the locate watcher) and places.json, and hands back the bookkeeping the app keeps
//! between its 2 s ticks.
//!
//! Rules: at a place, its action wins ("record" always, "meetings" only in a meeting, "off" never); away from
//! places the mode does ("always", or "meetings"). Act only when "should be recording" flips, so a manual
//! Pause/Stop sticks until then; arriving at or leaving a place counts as a flip. Right after launch, with
//! places set but no location yet, wait up to 30 s rather than let the mode decide for a place we can't see.
//! The first decision after launch may start recording but never stops one (a relaunch shouldn't end it).
use crate::places::{self, Place};
use serde::{Deserialize, Serialize};

const LOCATION_WAIT_S: f64 = 30.0;

#[derive(Deserialize)]
pub struct Tick {
    #[serde(default)]
    here: Option<[f64; 2]>, // [lat, lon]; filled from here.json, not sent by the app
    mode: String, // "always" | "meetings"
    in_meeting: bool,
    state: String, // recording | paused | stopping | processing | stopped
    launched_secs: f64,
    #[serde(flatten)]
    kept: Kept,
}

/// What the app keeps between ticks and passes back unchanged.
#[derive(Deserialize, Serialize, Default, Debug, PartialEq)]
pub struct Kept {
    place: Option<String>,     // label of the place we were at
    last_wanted: Option<bool>, // "should be recording" at the last decision
    adopted: bool,             // made the first decision since launch
}

#[derive(Serialize, Debug, PartialEq)]
pub struct Decision {
    act: &'static str,            // "start" | "stop" | "none"
    place_action: Option<String>, // the current place's action, for the status line
    #[serde(flatten)]
    kept: Kept,
}

pub fn run(arg: Option<String>) -> Result<(), String> {
    let mut tick: Tick = serde_json::from_str(&arg.ok_or("usage: ozen auto '<json>'")?)
        .map_err(|e| e.to_string())?;
    let here: serde_json::Value = std::fs::read(places::HERE)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    tick.here = here["lat"]
        .as_f64()
        .zip(here["lon"].as_f64())
        .map(|(lat, lon)| [lat, lon]);
    let decision = decide(tick, &places::load(places::FILE));
    println!("{}", serde_json::to_string(&decision).expect("serialize"));
    Ok(())
}

fn decide(t: Tick, places: &[Place]) -> Decision {
    let place = t.here.and_then(|[lat, lon]| places::at(places, lat, lon));
    let label = place.map(|p| p.label.clone());
    let action = place.map(|p| p.action.clone());
    let mut kept = t.kept;
    if label != kept.place {
        kept.place = label;
        kept.last_wanted = None; // arriving at or leaving a place applies right away
    }
    let none = |kept, place_action| Decision {
        act: "none",
        place_action,
        kept,
    };
    if kept.place.is_none()
        && t.here.is_none()
        && places::tracked(places)
        && t.launched_secs < LOCATION_WAIT_S
    {
        return none(kept, action);
    }
    let wanted = match action.as_deref() {
        Some(a) => a == "record" || a == "meetings" && t.in_meeting,
        None => t.mode == "always" || t.in_meeting,
    };
    let changed = kept.last_wanted != Some(wanted);
    kept.last_wanted = Some(wanted);
    if !changed {
        return none(kept, action);
    }
    if !kept.adopted {
        kept.adopted = true;
        if !wanted {
            return none(kept, action);
        }
    }
    let act = match (wanted, t.state.as_str()) {
        (true, "stopped" | "paused" | "processing") => "start",
        (false, "recording" | "paused") => "stop",
        _ => "none",
    };
    Decision {
        act,
        place_action: action,
        kept,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const HOME: [f64; 2] = [32.074, 34.809];

    fn places() -> Vec<Place> {
        serde_json::from_value(json!([
            {"label": "Home", "action": "meetings", "lat": HOME[0], "lon": HOME[1]},
            {"label": "Work", "action": "record"},
        ]))
        .unwrap()
    }

    fn tick(
        here: Option<[f64; 2]>,
        mode: &str,
        meeting: bool,
        state: &str,
        secs: f64,
        kept: Kept,
    ) -> Tick {
        Tick {
            here,
            mode: mode.into(),
            in_meeting: meeting,
            state: state.into(),
            launched_secs: secs,
            kept,
        }
    }

    fn settled(place: Option<&str>, wanted: bool) -> Kept {
        Kept {
            place: place.map(String::from),
            last_wanted: Some(wanted),
            adopted: true,
        }
    }

    #[test]
    fn a_place_overrides_the_mode() {
        let d = decide(
            tick(
                Some(HOME),
                "always",
                false,
                "stopped",
                100.0,
                settled(Some("Home"), false),
            ),
            &places(),
        );
        assert_eq!(d.act, "none"); // at Home (meetings only), no meeting: stays off despite Always
        let d = decide(
            tick(
                Some(HOME),
                "always",
                true,
                "stopped",
                100.0,
                settled(Some("Home"), false),
            ),
            &places(),
        );
        assert_eq!(d.act, "start");
        assert_eq!(d.place_action.as_deref(), Some("meetings"));
    }

    #[test]
    fn an_old_fix_at_home_is_still_home() {
        // No age anywhere in the input: standing still sends no new fix, and the last one still counts.
        let d = decide(
            tick(
                Some(HOME),
                "always",
                false,
                "recording",
                3600.0,
                settled(None, true),
            ),
            &places(),
        );
        assert_eq!((d.act, d.kept.place.as_deref()), ("stop", Some("Home")));
    }

    #[test]
    fn leaving_a_place_applies_the_mode_right_away() {
        let far = [32.2, 34.9];
        let d = decide(
            tick(
                Some(far),
                "always",
                false,
                "stopped",
                100.0,
                settled(Some("Home"), false),
            ),
            &places(),
        );
        assert_eq!((d.act, d.kept.place), ("start", None));
    }

    #[test]
    fn starts_while_processing_a_backlog() {
        let t = tick(
            None,
            "always",
            false,
            "processing",
            100.0,
            settled(None, false),
        );
        assert_eq!(decide(t, &[]).act, "start");
    }

    #[test]
    fn manual_stop_sticks_until_the_wish_flips() {
        let d = decide(
            tick(None, "always", false, "stopped", 100.0, settled(None, true)),
            &[],
        );
        assert_eq!(d.act, "none");
    }

    #[test]
    fn launch_waits_for_a_location_when_places_have_one() {
        let d = decide(
            tick(None, "always", false, "stopped", 5.0, Kept::default()),
            &places(),
        );
        assert_eq!((d.act, d.kept.adopted), ("none", false));
        let d = decide(
            tick(None, "always", false, "stopped", 31.0, Kept::default()),
            &places(),
        );
        assert_eq!(d.act, "start"); // no fix after 30 s: the mode decides
    }

    #[test]
    fn launch_starts_but_never_stops() {
        let d = decide(
            tick(
                Some(HOME),
                "always",
                false,
                "recording",
                1.0,
                Kept::default(),
            ),
            &places(),
        );
        assert_eq!((d.act, d.kept.adopted), ("none", true)); // hand-started recording at Home survives a relaunch
        let d = decide(
            tick(None, "always", false, "stopped", 1.0, Kept::default()),
            &[],
        );
        assert_eq!(d.act, "start"); // no places, mode Always: starts at login
    }

    #[test]
    fn reads_what_the_app_sends() {
        let t: Tick = serde_json::from_value(json!({"here": null, "mode": "meetings", "in_meeting": true,
            "state": "stopped", "launched_secs": 50.0, "place": null, "last_wanted": false, "adopted": true})).unwrap();
        let d = serde_json::to_value(decide(t, &[])).unwrap();
        assert_eq!(
            d,
            json!({"act": "start", "place_action": null, "place": null, "last_wanted": true, "adopted": true})
        );
    }
}
