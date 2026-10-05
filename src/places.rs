//! Places (places.json): labeled spots whose setting replaces Always/Meetings while you're within their radius.
//! Where you are comes from the locate binary; this decides which place that is.
use crate::crdt::{self, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;

pub const FILE: &str = "places.json";
pub const HERE: &str = "here.json"; // kept current by `locate watch` while some place has coordinates
const DEFAULT_RADIUS: f64 = 150.0; // meters

// Field order is the file's key order: sorted, as the menu bar app writes it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Place {
    pub action: String, // "record" | "meetings" | "off"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>, // src/crdt.rs: None for a new place (saving mints one)
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lon: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f64>,
}

fn raw(path: &str) -> Vec<Row> {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn load(path: &str) -> Vec<Place> {
    crdt::live_rows(&raw(path))
        .into_iter()
        .filter_map(|r| serde_json::from_value(Value::Object(r)).ok())
        .collect()
}

/// Save `places` as the live list: places.json keeps versions and tombstones (src/crdt.rs).
/// Refuses a place every other Mac would drop on receipt (sync/valid.rs: lat, lon and radius in range),
/// which would otherwise be sent again at every exchange.
pub fn check(p: &Place) -> Result<(), String> {
    let mut row = serde_json::to_value(p).map_err(|e| e.to_string())?;
    row["id"] = "new".into();
    crate::sync::valid::record("places", "new", &row)
}

pub fn save(path: &str, places: &[Place]) -> Result<(), String> {
    let live: Vec<Row> = places
        .iter()
        .filter_map(|p| serde_json::to_value(p).ok()?.as_object().cloned())
        .collect();
    let tmp = format!("{path}.tmp");
    let json = serde_json::to_string_pretty(&crdt::edit_rows(&raw(path), &live))
        .map_err(|e| e.to_string())?
        + "\n";
    fs::write(&tmp, json)
        .and_then(|_| fs::rename(&tmp, path))
        .map_err(|e| e.to_string())
}

/// Great-circle distance in meters (haversine on the mean Earth radius): well under a meter off at place scale.
pub fn distance(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let (dp, dl) = ((lat2 - lat1).to_radians(), (lon2 - lon1).to_radians());
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    2.0 * 6_371_008.8 * a.sqrt().asin()
}

/// The first place whose radius contains (lat, lon): list order decides overlaps, as in the Places window.
pub fn at(places: &[Place], lat: f64, lon: f64) -> Option<&Place> {
    places.iter().find(|p| match (p.lat, p.lon) {
        (Some(plat), Some(plon)) => {
            distance(lat, lon, plat, plon) <= p.radius.unwrap_or(DEFAULT_RADIUS)
        }
        _ => false,
    })
}

pub fn tracked(places: &[Place]) -> bool {
    places.iter().any(|p| p.lat.is_some() && p.lon.is_some())
}

/// Set place `i` to (lat, lon) in `path`.
pub fn set_location(path: &str, i: usize, lat: f64, lon: f64) -> Result<(), String> {
    let mut places = load(path);
    let p = places.get_mut(i).ok_or(format!("no place {i}"))?;
    (p.lat, p.lon) = (Some(lat), Some(lon));
    save(path, &places)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_other_macs_would_drop_is_refused() {
        assert!(check(&place("Home", Some(32.1), Some(34.8), Some(150.0))).is_ok());
        assert!(check(&place("Home", None, None, None)).is_ok());
        assert!(check(&place("Home", Some(200.0), Some(34.8), None)).is_err());
        assert!(check(&place("Home", Some(32.1), Some(34.8), Some(0.0))).is_err());
    }

    fn place(label: &str, lat: Option<f64>, lon: Option<f64>, radius: Option<f64>) -> Place {
        Place {
            action: "record".into(),
            id: None,
            label: label.into(),
            lat,
            lon,
            radius,
        }
    }

    #[test]
    fn distance_matches_known_values() {
        assert!(distance(32.0, 34.0, 32.0, 34.0).abs() < 1e-6);
        // one degree of latitude is ~111.2 km
        assert!((distance(32.0, 34.0, 33.0, 34.0) - 111_195.0).abs() < 50.0);
        // Tel Aviv to Jerusalem, ~54 km
        assert!((distance(32.0853, 34.7818, 31.7683, 35.2137) / 1000.0 - 54.0).abs() < 1.5);
    }

    #[test]
    fn a_place_matches_inside_its_radius_only_and_the_first_one_wins() {
        let home = place("Home", Some(32.0), Some(34.0), None);
        let near = place("Near", Some(32.0), Some(34.0), Some(1000.0));
        let unset = place("Work", None, None, None);
        let ps = vec![unset, home, near];
        let north = |m: f64| 32.0 + m / 111_195.0;
        assert_eq!(
            at(&ps, north(140.0), 34.0).map(|p| p.label.as_str()),
            Some("Home")
        );
        assert_eq!(
            at(&ps, north(160.0), 34.0).map(|p| p.label.as_str()),
            Some("Near")
        );
        assert!(at(&ps, north(1100.0), 34.0).is_none());
        assert!(tracked(&ps) && !tracked(&ps[..1]));
    }

    #[test]
    fn reads_the_apps_file_and_setting_a_location_keeps_the_rest() {
        let dir = std::env::temp_dir().join(format!("ozen-places-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join(FILE);
        let f = f.to_str().unwrap();
        // as the Swift app writes it
        fs::write(
            f,
            r#"[ { "action" : "meetings", "label" : "Home", "lat" : 32.07, "lon" : 34.8 },
                          { "action" : "record", "label" : "Work" } ]"#,
        )
        .unwrap();
        set_location(f, 1, 32.1, 34.7).unwrap();
        let ps = load(f);
        let mut home = place("Home", Some(32.07), Some(34.8), None).tap_action("meetings");
        home.id = Some("0000-Home".into()); // src/crdt.rs: an old place's id, kept from the first save on
        assert_eq!(ps[0], home);
        assert_eq!(
            (ps[1].lat, ps[1].lon, ps[1].action.as_str()),
            (Some(32.1), Some(34.7), "record")
        );
        assert!(set_location(f, 5, 0.0, 0.0).is_err());
        assert!(load(dir.join("missing.json").to_str().unwrap()).is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }

    impl Place {
        fn tap_action(mut self, a: &str) -> Self {
            self.action = a.into();
            self
        }
    }
}
