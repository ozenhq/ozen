//! The Places map: Apple's map (MapKit) with each located place as a draggable pin inside its radius circle
//! (red records, orange records meetings, gray turns recording off) and a blue "You" pin at your location.
//! A pin drag and a map click go to `App::map_message` as {type: "move", index, lat, lon} and
//! {type: "click", lat, lon}.
use crate::App;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSClickGestureRecognizer, NSColor, NSEvent, NSEventModifierFlags, NSEventType,
    NSGestureRecognizer,
};
use objc2_core_location::CLLocationCoordinate2D;
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSString};
use objc2_map_kit::{
    MKAnnotation, MKAnnotationView, MKAnnotationViewDragState, MKCircle, MKCircleRenderer,
    MKMapPoint, MKMapRect, MKMapSize, MKMapView, MKMapViewDelegate, MKMarkerAnnotationView,
    MKMetersPerMapPointAtLatitude, MKOverlay, MKOverlayRenderer, MKPointAnnotation,
};
use serde_json::{Value, json};
use std::cell::RefCell;

/// Zoomed in no further than this many meters across when fitting, like the page's maxZoom.
const MIN_SPAN_M: f64 = 1000.0;

struct Pin {
    index: Option<usize>, // the place's row; None for "You"
    color: Retained<NSColor>,
    ann: Retained<MKPointAnnotation>,
    circle: Option<Retained<MKCircle>>,
}

#[derive(Default)]
pub struct Ivars {
    pins: RefCell<Vec<Pin>>,
}

define_class!(
    // SAFETY: a plain NSObject acting as the map's delegate and click target; no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct MapDelegate;

    unsafe impl NSObjectProtocol for MapDelegate {}

    unsafe impl MKMapViewDelegate for MapDelegate {
        #[unsafe(method_id(mapView:viewForAnnotation:))]
        fn view_for(
            &self,
            _map: &MKMapView,
            annotation: &ProtocolObject<dyn MKAnnotation>,
        ) -> Option<Retained<MKAnnotationView>> {
            self.annotation_view(annotation)
        }

        #[unsafe(method_id(mapView:rendererForOverlay:))]
        fn renderer_for(
            &self,
            _map: &MKMapView,
            overlay: &ProtocolObject<dyn MKOverlay>,
        ) -> Retained<MKOverlayRenderer> {
            let pins = self.ivars().pins.borrow();
            let pin = pins
                .iter()
                .find(|p| p.circle.as_ref().is_some_and(|c| same(&**c, overlay)));
            // SAFETY: a plain renderer for our own circle overlay.
            unsafe {
                let r = MKCircleRenderer::initWithOverlay(MKCircleRenderer::alloc(), overlay);
                if let Some(p) = pin {
                    r.setStrokeColor(Some(&p.color));
                    r.setFillColor(Some(&p.color.colorWithAlphaComponent(0.15)));
                    r.setLineWidth(1.5);
                }
                Retained::into_super(Retained::into_super(r))
            }
        }

        #[unsafe(method(mapView:annotationView:didChangeDragState:fromOldState:))]
        fn drag_state(
            &self,
            _map: &MKMapView,
            view: &MKAnnotationView,
            new: MKAnnotationViewDragState,
            _old: MKAnnotationViewDragState,
        ) {
            if new != MKAnnotationViewDragState::Ending {
                return;
            }
            // SAFETY: reading our own annotation's coordinate; ending the drag as MapKit expects.
            unsafe { view.setDragState(MKAnnotationViewDragState::None) };
            let Some(a) = (unsafe { view.annotation() }) else { return };
            let index = self
                .ivars()
                .pins
                .borrow()
                .iter()
                .find(|p| same(&*p.ann, &*a))
                .and_then(|p| p.index);
            if let Some(index) = index {
                let c = unsafe { a.coordinate() };
                send(json!({"type": "move", "index": index, "lat": c.latitude, "lon": c.longitude}));
            }
        }
    }

    impl MapDelegate {
        #[unsafe(method(clicked:))]
        fn clicked(&self, g: &NSGestureRecognizer) {
            if let Some(map) = g.view().and_then(|v| v.downcast::<MKMapView>().ok()) {
                click_at(&map, g.locationInView(Some(&map)));
            }
        }
    }
);

impl MapDelegate {
    /// A pin's colored marker, draggable unless it's "You".
    fn annotation_view(
        &self,
        annotation: &ProtocolObject<dyn MKAnnotation>,
    ) -> Option<Retained<MKAnnotationView>> {
        let pins = self.ivars().pins.borrow();
        let pin = pins.iter().find(|p| same(&*p.ann, annotation))?;
        // SAFETY: a marker view for our own annotation.
        let v = unsafe {
            MKMarkerAnnotationView::initWithAnnotation_reuseIdentifier(
                MKMarkerAnnotationView::alloc(self.mtm()),
                Some(annotation),
                None,
            )
        };
        unsafe {
            v.setMarkerTintColor(Some(&pin.color));
            v.setDraggable(pin.index.is_some());
        }
        Some(Retained::into_super(v))
    }
}

/// The same object, whatever the static type (pass the objects, not their `Retained`s).
fn same<A: ?Sized, B: ?Sized>(a: &A, b: &B) -> bool {
    std::ptr::addr_eq(a as *const A, b as *const B)
}

/// A click at `p` in the map: the spot under it.
fn click_at(map: &MKMapView, p: NSPoint) {
    // SAFETY: converting a point in the map view to a coordinate.
    let c = unsafe { map.convertPoint_toCoordinateFromView(p, Some(map)) };
    send(json!({"type": "click", "lat": c.latitude, "lon": c.longitude}));
}

/// After the current event: the app redraws the map in answer, which mustn't happen inside MapKit's callback.
fn send(m: Value) {
    dispatch2::DispatchQueue::main().exec_async(move || {
        crate::APP.with(|a| a.get().unwrap().map_message(&m));
    });
}

pub struct Map {
    pub view: Retained<MKMapView>,
    delegate: Retained<MapDelegate>, // the map holds its delegate weakly
}

impl Map {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let delegate: Retained<MapDelegate> = {
            let this = MapDelegate::alloc(mtm).set_ivars(Ivars::default());
            // SAFETY: NSObject's init.
            unsafe { msg_send![super(this), init] }
        };
        // SAFETY: a default map view on the main thread.
        let view = unsafe { MKMapView::new(mtm) };
        // SAFETY: the delegate lives as long as the map (both in `Map`); the recognizer targets it.
        unsafe {
            view.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            let click = NSClickGestureRecognizer::initWithTarget_action(
                NSClickGestureRecognizer::alloc(mtm),
                Some(&delegate as &AnyObject),
                Some(sel!(clicked:)),
            );
            click.setDelaysPrimaryMouseButtonEvents(false);
            view.addGestureRecognizer(&click);
        }
        Map { view, delegate }
    }

    /// Draw the places (each `{label, lat, lon, action, radius}`; a place without coordinates is skipped, a
    /// missing radius is `radius`) and `here`; with `fit`, zoom to them.
    pub fn show(&self, places: &[Value], radius: f64, here: Option<(f64, f64)>, fit: bool) {
        let v = &self.view;
        // SAFETY: MapKit calls on the main thread with our own annotations and overlays.
        // (Ours, not v.overlays(): that's nil on an empty map.)
        for p in self.delegate.ivars().pins.borrow_mut().drain(..) {
            unsafe {
                v.removeAnnotation(ProtocolObject::from_ref(&*p.ann));
                if let Some(c) = &p.circle {
                    v.removeOverlay(ProtocolObject::from_ref(&**c));
                }
            }
        }
        let mut pins = vec![];
        for (i, p) in places.iter().enumerate() {
            let (Some(lat), Some(lon)) = (p["lat"].as_f64(), p["lon"].as_f64()) else {
                continue;
            };
            let at = CLLocationCoordinate2D {
                latitude: lat,
                longitude: lon,
            };
            let r = p["radius"].as_f64().filter(|&r| r != 0.0).unwrap_or(radius);
            pins.push(Pin {
                index: Some(i),
                color: color(p["action"].as_str().unwrap_or("")),
                ann: annotation(at, p["label"].as_str().unwrap_or("")),
                circle: Some(unsafe { MKCircle::circleWithCenterCoordinate_radius(at, r) }),
            });
        }
        if let Some((lat, lon)) = here {
            let at = CLLocationCoordinate2D {
                latitude: lat,
                longitude: lon,
            };
            pins.push(Pin {
                index: None,
                color: NSColor::systemBlueColor(),
                ann: annotation(at, "You"),
                circle: None,
            });
        }
        // The page fit the places, or you when no place is set.
        let located = pins.iter().filter(|p| p.index.is_some()).count();
        let points: Vec<MKMapPoint> = pins
            .iter()
            .filter(|p| located == 0 || p.index.is_some())
            .map(|p| unsafe { MKMapPoint::for_coordinate(p.ann.coordinate()) })
            .collect();
        *self.delegate.ivars().pins.borrow_mut() = pins;
        let pins = self.delegate.ivars().pins.borrow();
        for p in pins.iter() {
            // SAFETY: as above.
            unsafe {
                if let Some(c) = &p.circle {
                    v.addOverlay(ProtocolObject::from_ref(&**c));
                }
                v.addAnnotation(ProtocolObject::from_ref(&*p.ann));
            }
        }
        if fit && let Some(r) = fit_rect(&points) {
            unsafe { v.setVisibleMapRect_animated(r, false) };
        }
    }

    /// For the render check: a mouse event at map point `p`, posted to the app's event queue so AppKit and MapKit
    /// handle it as they would the real mouse's.
    pub fn mouse(&self, kind: NSEventType, p: NSPoint) {
        let Some(w) = self.view.window() else { return };
        let at = self.view.convertPoint_toView(p, None);
        let time = objc2_foundation::NSProcessInfo::processInfo().systemUptime();
        let e = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
            kind,
            at,
            NSEventModifierFlags::empty(),
            time,
            w.windowNumber(),
            None,
            0,
            1,
            1.0,
        );
        if let Some(e) = e {
            NSApplication::sharedApplication(self.view.mtm()).postEvent_atStart(&e, false);
        }
    }

    /// For the render check: drag place `index`'s pin to (lat, lon) and let go, through MapKit's drag delegate
    /// call on the pin's real view. False when the pin has no view (off screen).
    pub fn drag(&self, index: usize, lat: f64, lon: f64) -> bool {
        let pins = self.delegate.ivars().pins.borrow();
        let Some(p) = pins.iter().find(|p| p.index == Some(index)) else {
            return false;
        };
        // SAFETY: our own annotation and its view; the delegate call MapKit makes when a drag ends.
        unsafe {
            let Some(v) = self
                .view
                .viewForAnnotation(ProtocolObject::from_ref(&*p.ann))
            else {
                return false;
            };
            p.ann.setCoordinate(CLLocationCoordinate2D {
                latitude: lat,
                longitude: lon,
            });
            drop(pins);
            let _: () = msg_send![&*self.delegate, mapView: &*self.view, annotationView: &*v, didChangeDragState: MKAnnotationViewDragState::Ending, fromOldState: MKAnnotationViewDragState::Dragging];
        }
        true
    }

    /// The coordinate under map point `p`.
    pub fn coordinate_at(&self, p: NSPoint) -> (f64, f64) {
        // SAFETY: converting a point in the map view to a coordinate.
        let c = unsafe {
            self.view
                .convertPoint_toCoordinateFromView(p, Some(&self.view))
        };
        (c.latitude, c.longitude)
    }

    /// What the map shows, for the render check.
    pub fn dump(&self) -> Vec<String> {
        let pins = self.delegate.ivars().pins.borrow();
        let mut out = vec![];
        for p in pins.iter() {
            // SAFETY: reading our own annotations.
            let (c, title) = unsafe { (p.ann.coordinate(), p.ann.title()) };
            out.push(format!(
                "PIN {:?} {} {:?} {:?} {}",
                p.index,
                title.map_or(String::new(), |t| t.to_string()),
                c.latitude,
                c.longitude,
                color_name(&p.color)
            ));
            // What MapKit actually draws, from our delegate; "-" while off screen (no view made yet).
            // SAFETY: reading the map's own views and renderers for our annotations and overlays.
            let shown = unsafe {
                self.view
                    .viewForAnnotation(ProtocolObject::from_ref(&*p.ann))
                    .and_then(|v| v.downcast::<MKMarkerAnnotationView>().ok())
                    .map_or("-".into(), |v| {
                        format!(
                            "{} draggable {}",
                            v.markerTintColor().map_or("none", |c| color_name(&c)),
                            v.isDraggable()
                        )
                    })
            };
            out.push(format!("  VIEW {shown}"));
            if let Some(ci) = &p.circle {
                let drawn = unsafe {
                    self.view
                        .rendererForOverlay(ProtocolObject::from_ref(&**ci))
                        .and_then(|r| r.downcast::<MKCircleRenderer>().ok())
                        .map_or("-", |r| r.strokeColor().map_or("none", |c| color_name(&c)))
                };
                out.push(format!("CIRCLE {:?} {drawn}", unsafe { ci.radius() }));
            }
        }
        // SAFETY: reading the view's region.
        let r = unsafe { self.view.region() };
        out.push(format!(
            "REGION {:.4} {:.4} span {:.4} {:.4}",
            r.center.latitude, r.center.longitude, r.span.latitudeDelta, r.span.longitudeDelta
        ));
        out
    }
}

fn annotation(at: CLLocationCoordinate2D, title: &str) -> Retained<MKPointAnnotation> {
    // SAFETY: a plain point annotation.
    unsafe {
        let a = MKPointAnnotation::new();
        a.setCoordinate(at);
        a.setTitle(Some(&NSString::from_str(title)));
        a
    }
}

fn color(action: &str) -> Retained<NSColor> {
    match action {
        "record" => NSColor::systemRedColor(),
        "meetings" => NSColor::systemOrangeColor(),
        _ => NSColor::systemGrayColor(),
    }
}

fn color_name(c: &NSColor) -> &'static str {
    ["record", "meetings", "off"]
        .into_iter()
        .find(|a| *c == *color(a))
        .unwrap_or("you")
}

/// The points' bounds padded by half their size on each side, at least MIN_SPAN_M across.
fn fit_rect(points: &[MKMapPoint]) -> Option<MKMapRect> {
    let first = points.first()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for p in points {
        (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
    }
    let (w, h) = (x1 - x0, y1 - y0);
    // map points per meter at the middle latitude (MapKit's projection)
    let mid = unsafe {
        objc2_map_kit::MKCoordinateForMapPoint(MKMapPoint {
            x: (x0 + x1) / 2.0,
            y: (y0 + y1) / 2.0,
        })
    };
    let min = MIN_SPAN_M / unsafe { MKMetersPerMapPointAtLatitude(mid.latitude) };
    let (w2, h2) = ((w * 2.0).max(min), (h * 2.0).max(min));
    Some(MKMapRect {
        origin: MKMapPoint {
            x: (x0 + x1) / 2.0 - w2 / 2.0,
            y: (y0 + y1) / 2.0 - h2 / 2.0,
        },
        size: MKMapSize {
            width: w2,
            height: h2,
        },
    })
}

impl App {
    pub fn places_map(&self) -> &Retained<MKMapView> {
        &self.places_map_parts().view
    }

    pub fn places_map_parts(&self) -> &Map {
        self.ivars().places_map.get_or_init(|| {
            let m = Map::new(self.mtm());
            // a fixed height; the width follows the rows (build_places)
            m.view
                .heightAnchor()
                .constraintEqualToConstant(280.0)
                .setActive(true);
            m
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_the_places_padded_and_not_too_close() {
        let p = |lat, lon| unsafe {
            MKMapPoint::for_coordinate(CLLocationCoordinate2D {
                latitude: lat,
                longitude: lon,
            })
        };
        assert!(fit_rect(&[]).is_none());
        let (a, b) = (p(32.0, 34.8), p(31.8, 35.2));
        let r = fit_rect(&[a, b]).unwrap();
        assert!((r.size.width - 2.0 * (b.x - a.x)).abs() < 1e-6); // half again on each side
        assert!((r.origin.x - (a.x - (b.x - a.x) / 2.0)).abs() < 1e-6);
        let one = fit_rect(&[a]).unwrap(); // a single place: MIN_SPAN_M across, centered on it
        let m = unsafe { MKMetersPerMapPointAtLatitude(32.0) };
        assert!((one.size.width * m - MIN_SPAN_M).abs() < 1e-6);
        assert!((one.origin.x + one.size.width / 2.0 - a.x).abs() < 1e-6);
    }
}
