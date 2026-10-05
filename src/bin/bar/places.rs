//! The Places window: labeled places that override the record mode while you're within their radius. Each can be
//! located by typing coordinates, "Use current location" (`ozen places here N`), clicking the map or dragging its
//! pin (map.rs, MapKit). places.json is plain JSON in the ozen folder.
//!
//! Also the app's side of location: only the app can show the location prompt (the locate binary uses the
//! answer), and it writes here.json itself when the locate binary can't (here.json.error).
use crate::{App, cli, crdt};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSControlSize, NSFont, NSLayoutAttribute,
    NSPopUpButton, NSStackView, NSTextField, NSUserInterfaceItemIdentification,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowStyleMask,
};
use objc2_core_location::{CLAuthorizationStatus, CLLocationManager};
use objc2_foundation::{NSEdgeInsets, NSPoint, NSRect, NSSize, NSString, ns_string};
use serde_json::{Value, json};

const DEFAULT_RADIUS: f64 = 150.0; // meters
const ACTIONS: [(&str, &str); 3] = [
    ("record", "Auto record"),
    ("meetings", "Record meetings only"),
    ("off", "Auto off"),
];

fn file() -> std::path::PathBuf {
    cli::dir().join("places.json")
}

/// places.json; older builds kept places in the app's defaults (moved to the file on the next save).
pub fn load() -> Vec<Value> {
    let from_file = std::fs::read(file()).ok();
    let legacy = || {
        objc2_foundation::NSUserDefaults::standardUserDefaults()
            .dataForKey(ns_string!("places"))
            .map(|d| d.to_vec())
    };
    from_file
        .or_else(legacy)
        .and_then(|b| serde_json::from_slice::<Vec<crdt::Row>>(&b).ok())
        .map(|raw| {
            crdt::live_rows(&raw)
                .into_iter()
                .map(Value::Object)
                .collect()
        })
        .unwrap_or_else(|| {
            vec![
                json!({"label": "Home", "action": "off"}),
                json!({"label": "Work", "action": "record"}),
            ]
        })
}

/// Sorted keys, as src/places.rs writes it; versions and tombstones kept (src/crdt.rs).
fn save(places: &[Value]) {
    let path = file();
    let tmp = path.with_extension("json.tmp");
    let raw: Vec<crdt::Row> = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let live: Vec<crdt::Row> = places
        .iter()
        .filter_map(|p| p.as_object().cloned())
        .collect();
    let text =
        serde_json::to_string_pretty(&crdt::edit_rows(&raw, &live)).unwrap_or_default() + "\n";
    if std::fs::write(&tmp, text).is_ok() && std::fs::rename(&tmp, &path).is_ok() {
        objc2_foundation::NSUserDefaults::standardUserDefaults()
            .removeObjectForKey(ns_string!("places")); // migrated
    }
}

pub fn located(places: &[Value]) -> bool {
    places.iter().any(|p| p["lat"].is_number())
}

fn tagged(v: &NSView, i: usize) {
    // SAFETY: the row index rides in the control's tag.
    unsafe { objc2::msg_send![v, setTag: i as isize] }
}

pub fn row_of(sender: &AnyObject) -> usize {
    // SAFETY: every Places control carries its row in -tag.
    let t: isize = unsafe { objc2::msg_send![sender, tag] };
    t.max(0) as usize
}

impl App {
    pub fn show_places(&self) {
        let mtm = self.mtm();
        let iv = self.ivars();
        if iv.places_window.borrow().is_none() {
            // SAFETY: a plain window we keep (not released on close).
            let w = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 240.0)),
                    NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            w.setTitle(ns_string!("Ozen Places"));
            unsafe { w.setReleasedWhenClosed(false) };
            let stack = iv.places_stack.get_or_init(|| NSStackView::new(mtm));
            stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            stack.setAlignment(NSLayoutAttribute::Leading);
            stack.setEdgeInsets(NSEdgeInsets {
                top: 12.0,
                left: 12.0,
                bottom: 12.0,
                right: 12.0,
            });
            let note = iv
                .places_note
                .get_or_init(|| NSTextField::wrappingLabelWithString(ns_string!(""), mtm));
            note.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            note.setTextColor(Some(&NSColor::secondaryLabelColor()));
            w.setContentView(Some(stack));
            *iv.places_window.borrow_mut() = Some(w);
        }
        self.build_places(true);
        if let Some(w) = iv.places_window.borrow().as_ref() {
            w.center();
            w.makeKeyAndOrderFront(None);
        }
        NSApplication::sharedApplication(mtm).activate();
    }

    fn note(&self, s: &str) {
        if let Some(n) = self.ivars().places_note.get() {
            n.setStringValue(&NSString::from_str(s));
        }
    }

    fn small(&self, title: &str, action: objc2::runtime::Sel, i: usize) -> Retained<NSButton> {
        // SAFETY: the target is the app delegate, alive for the process.
        let b = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(title),
                Some(self),
                Some(action),
                self.mtm(),
            )
        };
        b.setControlSize(NSControlSize::Small);
        tagged(&b, i);
        b
    }

    fn field(
        &self,
        value: &str,
        hint: &str,
        action: objc2::runtime::Sel,
        i: usize,
        width: f64,
    ) -> Retained<NSTextField> {
        let f = NSTextField::textFieldWithString(&NSString::from_str(value), self.mtm());
        f.setPlaceholderString(Some(&NSString::from_str(hint)));
        tagged(&f, i);
        // SAFETY: the target is the app delegate, alive for the process.
        unsafe {
            f.setTarget(Some(self));
            f.setAction(Some(action));
            if let Some(cell) = f.cell() {
                cell.setSendsActionOnEndEditing(true); // clicking away saves too, not only Return
            }
        }
        f.widthAnchor()
            .constraintEqualToConstant(width)
            .setActive(true);
        f
    }

    /// One row per place: label, action, where it is, and buttons; the row index rides in each control's tag.
    pub fn build_places(&self, fit: bool) {
        let mtm = self.mtm();
        let iv = self.ivars();
        let Some(stack) = iv.places_stack.get() else {
            return;
        };
        iv.rebuilding.set(true);
        for v in &stack.arrangedSubviews() {
            v.removeFromSuperview();
        }
        iv.rebuilding.set(false);
        let intro = NSTextField::wrappingLabelWithString(
            ns_string!(
                "While you're within a place's radius, its setting replaces Always/Meetings. Type coordinates, use where you are now, pick a spot on the map, or drag a pin."
            ),
            mtm,
        );
        intro.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        stack.addArrangedSubview(&intro);
        let map = self.places_map();
        stack.addArrangedSubview(map);
        let label = |s: &str| NSTextField::labelWithString(&NSString::from_str(s), mtm);
        for (i, p) in load().iter().enumerate() {
            let name = self.field(
                p["label"].as_str().unwrap_or(""),
                "Label",
                sel!(renamePlace:),
                i,
                160.0,
            );
            let action = NSPopUpButton::initWithFrame_pullsDown(
                NSPopUpButton::alloc(mtm),
                NSRect::ZERO,
                false,
            );
            for (_, title) in ACTIONS {
                action.addItemWithTitle(&NSString::from_str(title));
            }
            let chosen = ACTIONS
                .iter()
                .position(|(a, _)| Some(*a) == p["action"].as_str())
                .unwrap_or(0);
            action.selectItemAtIndex(chosen as isize);
            tagged(&action, i);
            // SAFETY: the target is the app delegate, alive for the process.
            unsafe {
                action.setTarget(Some(self));
                action.setAction(Some(sel!(placeActionChanged:)));
            }
            let remove = self.small("Remove", sel!(removePlace:), i);
            let spacer = NSView::new(mtm);
            let top: [&NSView; 4] = [&name, &action, &spacer, &remove];
            stack.addArrangedSubview(&NSStackView::stackViewWithViews(
                &objc2_foundation::NSArray::from_slice(&top),
                mtm,
            ));
            // Every value is editable by hand; empty latitude or longitude means "not set".
            let num = |k: &str| {
                p[k].as_f64().map_or(String::new(), |v| {
                    if k == "radius" {
                        (v as i64).to_string()
                    } else {
                        format!("{v:.6}")
                    }
                })
            };
            let fields: Vec<Retained<NSTextField>> = [
                ("lat", "Latitude", 104.0),
                ("lon", "Longitude", 104.0),
                ("radius", "150", 56.0),
            ]
            .iter()
            .map(|(k, hint, w)| {
                let f = self.field(&num(k), hint, sel!(placeValueChanged:), i, *w);
                f.setIdentifier(Some(&NSString::from_str(k)));
                f
            })
            .collect();
            let status = if iv.setting_place.get() == Some(i) {
                "Locating…"
            } else if iv.picking_place.get() == Some(i) {
                "Click the map…"
            } else {
                ""
            };
            let (l1, l2, l3, l4, l5) = (
                label("Lat"),
                label("Lon"),
                label("Radius"),
                label("m"),
                label(status),
            );
            let here = self.small("Use current location", sel!(setPlaceHere:), i);
            let pick = self.small("Pick on map", sel!(pickOnMap:), i);
            let parts: [&NSView; 10] = [
                &l1, &fields[0], &l2, &fields[1], &l3, &fields[2], &l4, &here, &pick, &l5,
            ];
            let row = NSStackView::stackViewWithViews(
                &objc2_foundation::NSArray::from_slice(&parts),
                mtm,
            );
            row.setEdgeInsets(NSEdgeInsets {
                top: 0.0,
                left: 8.0,
                bottom: 6.0,
                right: 0.0,
            });
            stack.addArrangedSubview(&row);
        }
        // SAFETY: the target is the app delegate, alive for the process.
        let add = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Add place"),
                Some(self),
                Some(sel!(addPlace:)),
                mtm,
            )
        };
        stack.addArrangedSubview(&add);
        if let Some(n) = iv.places_note.get() {
            stack.addArrangedSubview(n);
        }
        let row_width = stack
            .arrangedSubviews()
            .iter()
            .skip(2)
            .map(|v| v.fittingSize().width)
            .fold(f64::NAN, f64::max);
        let row_width = if row_width.is_nan() { 536.0 } else { row_width };
        for c in &map.constraints() {
            if c.firstAttribute() == NSLayoutAttribute::Width {
                map.removeConstraint(&c);
            }
        }
        map.widthAnchor()
            .constraintEqualToConstant(row_width)
            .setActive(true);
        self.show_places_on_map(fit);
        intro.setPreferredMaxLayoutWidth(row_width); // wrap the intro to the rows, so it never squeezes them
        if let Some(w) = iv.places_window.borrow().as_ref() {
            w.setContentSize(stack.fittingSize());
        }
    }

    /// Draw each located place with its radius on the map; red records, gray turns it off.
    pub fn show_places_on_map(&self, fit: bool) {
        self.places_map_parts()
            .show(&load(), DEFAULT_RADIUS, self.ivars().here.get(), fit);
    }

    /// From the map (map.rs): {type: "move", index, lat, lon} when a pin is dragged, {type: "click", lat, lon} for a map click.
    pub fn map_message(&self, m: &Value) {
        let (Some(lat), Some(lon)) = (m["lat"].as_f64(), m["lon"].as_f64()) else {
            return;
        };
        let i = if m["type"] == "move" {
            m["index"].as_u64().map(|i| i as usize)
        } else {
            self.ivars().picking_place.get()
        };
        let Some(i) = i else { return };
        self.commit_edits();
        self.ivars().picking_place.set(None);
        self.edit_places(
            |p| {
                if let Some(p) = p.get_mut(i) {
                    p["lat"] = json!(lat);
                    p["lon"] = json!(lon);
                }
            },
            false, // the map stays where you put it
        );
        self.build_places(false);
    }

    pub fn edit_places(&self, change: impl FnOnce(&mut Vec<Value>), fit: bool) {
        let mut places = load();
        change(&mut places);
        save(&places);
        self.ask_location();
        self.ivars().auto.borrow_mut().last_wanted = None; // a changed place applies right away
        self.auto_control();
        self.show_places_on_map(fit);
    }

    /// Save a label still being typed while row indexes are valid; a field removed mid-edit would rename the wrong row.
    pub fn commit_edits(&self) {
        if let Some(w) = self.ivars().places_window.borrow().as_ref() {
            w.makeFirstResponder(None);
        }
    }

    pub fn rename_place(&self, sender: &NSTextField) {
        if self.ivars().rebuilding.get() {
            return;
        }
        let label = sender.stringValue().to_string().trim().to_string();
        let i = row_of(sender);
        self.edit_places(
            |p| {
                if !label.is_empty()
                    && let Some(p) = p.get_mut(i)
                {
                    p["label"] = json!(label);
                }
            },
            true,
        );
    }

    pub fn place_action_changed(&self, sender: &NSPopUpButton) {
        let (i, a) = (row_of(sender), sender.indexOfSelectedItem().max(0) as usize);
        self.edit_places(
            |p| {
                if let (Some(p), Some((action, _))) = (p.get_mut(i), ACTIONS.get(a)) {
                    p["action"] = json!(action);
                }
            },
            true,
        );
    }

    /// A typed latitude, longitude or radius; anything out of range is refused and the row shows the saved value again.
    pub fn place_value_changed(&self, sender: &NSTextField) {
        if self.ivars().rebuilding.get() {
            return;
        }
        let text = sender.stringValue().to_string().trim().to_string();
        let key = sender
            .identifier()
            .map(|k| k.to_string())
            .unwrap_or_default();
        let v: Option<f64> = text.parse().ok();
        let valid = text.is_empty()
            || v.is_some_and(|v| match key.as_str() {
                "lat" => v.abs() <= 90.0,
                "lon" => v.abs() <= 180.0,
                _ => v > 0.0,
            });
        if !valid {
            let what = match key.as_str() {
                "lat" => "latitude (−90…90)",
                "lon" => "longitude (−180…180)",
                _ => "radius in meters",
            };
            self.note(&format!("{text} isn't a valid {what}."));
            self.build_places(false);
            return;
        }
        self.note("");
        let i = row_of(sender);
        let old = load();
        let Some(p) = old.get(i) else { return };
        if p[&key].as_f64() == v {
            return; // end-editing fires on every focus change: skip saves that change nothing
        }
        self.edit_places(
            |p| {
                if let Some(p) = p.get_mut(i).and_then(Value::as_object_mut) {
                    match v {
                        Some(v) => p.insert(key.clone(), json!(v)),
                        None => p.remove(&key),
                    };
                }
            },
            key != "radius",
        );
    }

    pub fn set_place_here(&self, i: usize) {
        self.commit_edits();
        self.note("");
        self.ask_location();
        self.ivars().setting_place.set(Some(i));
        self.build_places(true);
        cli::run(&["places", "here", &i.to_string()], |_, err, code| {
            // Rust gets the fix and saves it (src/places.rs)
            crate::APP.with(|a| {
                let app = a.get().unwrap();
                app.ivars().setting_place.set(None);
                if code != 0 {
                    app.note(&err);
                }
                app.ivars().auto.borrow_mut().last_wanted = None; // a changed place applies right away
                app.build_places(true);
                app.refresh_state();
            })
        });
    }

    pub fn pick_on_map(&self, i: usize) {
        self.commit_edits();
        self.note("");
        self.ivars().picking_place.set(Some(i));
        self.build_places(false);
    }

    pub fn remove_place(&self, i: usize) {
        self.commit_edits();
        self.edit_places(
            |p| {
                if i < p.len() {
                    p.remove(i);
                }
            },
            true,
        );
        self.build_places(true);
    }

    pub fn add_place(&self) {
        self.commit_edits();
        self.edit_places(
            |p| p.push(json!({"label": format!("Place {}", p.len() + 1), "action": "record"})),
            true,
        );
        self.build_places(true);
    }

    pub fn location(&self) -> &Retained<CLLocationManager> {
        self.ivars().location.get_or_init(|| {
            // SAFETY: a location manager whose delegate is the app delegate, alive for the process.
            unsafe {
                let m = CLLocationManager::new();
                m.setDelegate(Some(ProtocolObject::from_ref(self)));
                m
            }
        })
    }

    /// Only the app can show the location prompt (the locate binary uses the answer), so ask here once some place
    /// has coordinates; an unused feature never asks. macOS shows the prompt when updates start, not on the request
    /// alone, so start them until the user answers.
    pub fn ask_location(&self) {
        let m = self.location();
        // SAFETY: plain CoreLocation calls on the main thread.
        unsafe {
            if located(&load()) && m.authorizationStatus() == CLAuthorizationStatus::NotDetermined {
                m.requestAlwaysAuthorization();
                m.startUpdatingLocation();
            }
        }
    }

    /// Fallback: the locate binary shares the app's location permission through the app's identity. If it can't get
    /// a location (it writes here.json.error) while the app itself is allowed, the app writes here.json instead, so
    /// place switching keeps working; `ozen place` still decides which place that is. The locate binary retries each
    /// minute and clears the error on its first fix, which hands the job back.
    pub fn supply_location_if_needed(&self) {
        let blocked = cli::dir().join("here.json.error").exists();
        // SAFETY: reading the authorization status.
        let allowed = unsafe { self.location().authorizationStatus() }
            == CLAuthorizationStatus::AuthorizedAlways;
        let want = blocked && allowed && located(&load());
        if want {
            // heartbeat for `ozen health`: the app has the location covered, so don't warn
            let _ = std::fs::write(cli::dir().join("here.json.app"), "");
        }
        if want == self.ivars().supplying_location.get() {
            return;
        }
        self.ivars().supplying_location.set(want);
        // SAFETY: plain CoreLocation calls on the main thread.
        unsafe {
            if want {
                self.location().startUpdatingLocation()
            } else {
                self.location().stopUpdatingLocation()
            }
        }
    }

    pub fn location_update(&self, lat: f64, lon: f64, t: f64) {
        if !self.ivars().supplying_location.get() {
            return;
        }
        let path = cli::dir().join("here.json");
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, format!("{{\"lat\":{lat},\"lon\":{lon},\"t\":{t}}}")).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    pub fn authorization_changed(&self) {
        let m = self.location();
        // SAFETY: reading and stopping CoreLocation on the main thread.
        let status = unsafe { m.authorizationStatus() };
        if status != CLAuthorizationStatus::NotDetermined && !self.ivars().supplying_location.get()
        {
            unsafe { m.stopUpdatingLocation() }; // answered: locate takes over
        }
        if status == CLAuthorizationStatus::Denied || status == CLAuthorizationStatus::Restricted {
            self.note("Location access is off. Turn on Ozen in System Settings → Privacy & Security → Location Services.");
        }
    }
}
