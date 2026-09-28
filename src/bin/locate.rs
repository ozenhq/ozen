//! Where this Mac is, from CoreLocation. `ozen` runs it; it holds no logic about places.
//!   locate once         print `lat lon unix_time` for a current fix, then exit
//!   locate watch FILE   keep FILE (`{"lat","lon","t"}`) current until nobody reads it for 5 minutes
//! Embedded Info.plist + Ozen's signing certificate make macOS treat it as Ozen.app (see locate.plist);
//! only the app can show the permission prompt, so the app asks and this uses the answer.
use objc2::rc::Retained;
use objc2_core_location::{CLLocation, CLLocationManager, kCLLocationAccuracyHundredMeters};
use objc2_foundation::{NSDate, NSRunLoop};
use std::process::exit;

const FRESH: f64 = 120.0; // a fix this recent is where we are now
const WAIT: u32 = 30; // seconds to wait for one

fn spin(seconds: f64) {
    NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(seconds));
}

fn line(l: &CLLocation) -> (f64, f64, f64) {
    let c = unsafe { l.coordinate() };
    let t = unsafe { l.timestamp() }.timeIntervalSince1970();
    (c.latitude, c.longitude, t)
}

/// Denied (2) or restricted (1): say how to fix it instead of waiting for a fix that won't come.
fn check_allowed(m: &CLLocationManager) {
    let s = unsafe { m.authorizationStatus() }.0;
    if s == 1 || s == 2 {
        eprintln!(
            "Location access is off. Turn on Ozen in System Settings → Privacy & Security → Location Services."
        );
        exit(3);
    }
}

fn manager() -> Retained<CLLocationManager> {
    unsafe {
        let m = CLLocationManager::new();
        m.setDesiredAccuracy(kCLLocationAccuracyHundredMeters);
        check_allowed(&m);
        // Starting updates always delivers the current location, even to a Mac standing still.
        m.startUpdatingLocation();
        m
    }
}

fn once() {
    let m = manager();
    for _ in 0..WAIT {
        spin(1.0);
        check_allowed(&m);
        if let Some(l) = unsafe { m.location() }
            && -unsafe { l.timestamp() }.timeIntervalSinceNow() < FRESH
        {
            let (lat, lon, t) = line(&l);
            return println!("{lat} {lon} {t}");
        }
    }
    eprintln!(
        "Couldn't get your location in {WAIT} seconds. Check Wi-Fi is on, or use Pick on map."
    );
    exit(1);
}

fn watch(file: &str) {
    let m = manager();
    let mut last = 0.0;
    let mut tick = 0u32;
    loop {
        spin(1.0);
        if let Some(l) = unsafe { m.location() } {
            let (lat, lon, t) = line(&l);
            if t != last {
                last = t;
                let tmp = format!("{file}.tmp");
                let json = format!(r#"{{"lat":{lat},"lon":{lon},"t":{t}}}"#);
                if std::fs::write(&tmp, json)
                    .and_then(|_| std::fs::rename(&tmp, file))
                    .is_err()
                {
                    exit(1);
                }
            }
        }
        tick += 1;
        // `ozen place` touches FILE.asked each time it reads FILE; nobody asking for 5 minutes (app quit, or no
        // place has coordinates any more) means stop.
        let asked = std::fs::metadata(format!("{file}.asked")).and_then(|m| m.modified());
        if tick.is_multiple_of(60)
            && !asked.is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() < 300))
        {
            let _ = std::fs::remove_file(file);
            exit(0);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["once"] => once(),
        ["watch", file] => watch(file),
        _ => {
            eprintln!("usage: locate once | locate watch FILE");
            exit(2);
        }
    }
}
