//! The render check's Places part (`bar --dump`): scripted edits in the Places window, dumped after each.
use crate::{App, cli, later, places};
use objc2::DefinedClass;
use objc2::rc::Retained;
use objc2_app_kit::{NSTextField, NSUserInterfaceItemIdentification, NSView};
use objc2_foundation::{NSPoint, ns_string};

impl App {
    /// The render check's Places part: open the window, then scripted edits (a map click while picking, a pin move,
    /// a real pin drag with Add place, a typed latitude, an invalid radius), dumping the window and the map and
    /// saving places.json after each. Writes the dump, puts places.json back as it was and quits: the edits are a
    /// test, never the user's places.
    pub fn dump_places(&self, file: String, out: String) {
        let places = cli::dir().join("places.json");
        let saved = std::fs::read(&places).ok();
        self.show_places();
        fn field(v: &NSView, key: &str, row: isize) -> Option<Retained<NSTextField>> {
            if let Some(t) = v.downcast_ref::<NSTextField>()
                && t.identifier().is_some_and(|k| k.to_string() == key)
                && places::row_of(t) as isize == row
            {
                return Some(objc2::Message::retain(t));
            }
            v.subviews().iter().find_map(|s| field(&s, key, row))
        }
        let file2 = file.clone();
        let snap = move |app: &App, step: usize, out: &mut String| {
            let _ = std::fs::copy(
                cli::dir().join("places.json"),
                format!("{file}.places{step}.json"),
            );
            *out += &app.dump_window(
                app.ivars().places_window.borrow().as_ref(),
                &format!("{file}.places{step}.png"),
            );
            *out += &format!("\n{}", app.places_map_parts().dump().join("\n"));
        };
        later(4.0, move |app| {
            let mut out = out;
            snap(app, 1, &mut out);
            app.pick_on_map(0);
            // a click on the map, posted as mouse events: picks that spot for Home
            let map = app.places_map_parts();
            let b = map.view.bounds();
            let p = NSPoint::new(b.size.width * 0.3, b.size.height * 0.6);
            out += &format!("\nCLICK expect {:?}", map.coordinate_at(p));
            map.mouse(objc2_app_kit::NSEventType::LeftMouseDown, p);
            map.mouse(objc2_app_kit::NSEventType::LeftMouseUp, p);
            later(1.5, move |app| {
                snap(app, 2, &mut out);
                app.map_message(
                    &serde_json::json!({"type": "move", "index": 1, "lat": 31.5, "lon": 35.25}),
                );
                later(1.5, move |app| {
                    snap(app, 3, &mut out);
                    // Gym's pin dragged: its real view, and MapKit's drag-ended delegate call (MapKit ignores
                    // synthesized drag events)
                    let dragged = app.places_map_parts().drag(2, 32.105, 34.81);
                    out += &format!("\nDRAGGED Gym {dragged}");
                    app.add_place();
                    later(1.0, move |app| {
                        snap(app, 4, &mut out);
                        let stack = app.ivars().places_stack.get().unwrap().clone();
                        if let Some(f) = field(&stack, "lat", 2) {
                            f.setStringValue(ns_string!("33.5"));
                            app.place_value_changed(&f);
                        }
                        if let Some(f) = field(&stack, "radius", 0) {
                            f.setStringValue(ns_string!("abc"));
                            app.place_value_changed(&f);
                        }
                        later(1.0, move |app| {
                            snap(app, 5, &mut out);
                            match &saved {
                                Some(b) => std::fs::write(&places, b).expect("restore places.json"),
                                None => {
                                    let _ = std::fs::remove_file(&places);
                                }
                            }
                            std::fs::write(&file2, out).expect("write dump");
                            std::process::exit(0);
                        });
                    });
                });
            });
        });
    }
}
