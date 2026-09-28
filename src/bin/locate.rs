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

const DENIED: &str = "Location access is off. Turn on Ozen in System Settings → Privacy & Security → Location Services.";
const NOT_ASKED: &str =
    "Ozen hasn't been allowed to use your location yet. Open Places… in Ozen to allow it.";
const NO_FIX: &str = "No location fix. Check Wi-Fi is on.";

/// Why no fix will come, or None while one may: denied (2) or restricted (1), or never answered (0).
fn problem(m: &CLLocationManager) -> Option<&'static str> {
    match unsafe { m.authorizationStatus() }.0 {
        1 | 2 => Some(DENIED),
        0 => Some(NOT_ASKED),
        _ => None,
    }
}

fn manager() -> Retained<CLLocationManager> {
    unsafe {
        let m = CLLocationManager::new();
        m.setDesiredAccuracy(kCLLocationAccuracyHundredMeters);
        // Starting updates always delivers the current location, even to a Mac standing still.
        m.startUpdatingLocation();
        m
    }
}

fn once() {
    let m = manager();
    for _ in 0..WAIT {
        spin(1.0);
        if let Some(l) = unsafe { m.location() }
            && -unsafe { l.timestamp() }.timeIntervalSinceNow() < FRESH
        {
            let (lat, lon, t) = line(&l);
            return println!("{lat} {lon} {t}");
        }
        if problem(&m) == Some(DENIED) {
            break;
        }
    }
    eprintln!("{}", problem(&m).unwrap_or(NO_FIX));
    exit(1);
}

/// FILE.error says why FILE isn't coming (`ozen health` shows it, `ozen place` stops respawning); a fix clears it.
fn watch(file: &str) {
    // One watcher: concurrent `ozen place` polls can each start one; all but the lock holder leave at once.
    let lock = std::fs::File::create(format!("{file}.lock"));
    if !lock.as_ref().is_ok_and(|l| l.try_lock().is_ok()) {
        exit(0);
    }
    let m = manager();
    let error = format!("{file}.error");
    let mut last = 0.0;
    let mut tick = 0u32;
    loop {
        spin(1.0);
        tick += 1;
        if last == 0.0 && tick >= WAIT || problem(&m) == Some(DENIED) {
            let why = problem(&m).unwrap_or(NO_FIX);
            let _ = std::fs::write(&error, why);
            if why != NO_FIX {
                exit(3); // nothing will change until the permission does; `ozen place` retries each minute
            }
        }
        if let Some(l) = unsafe { m.location() } {
            let (lat, lon, t) = line(&l);
            if t != last {
                last = t;
                let _ = std::fs::remove_file(&error);
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
