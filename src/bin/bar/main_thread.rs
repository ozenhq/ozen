//! Main-thread and clock helpers: the wall-clock time, the main-thread marker, and running code on the app later.
use crate::{APP, App};
use objc2_foundation::MainThreadMarker;

pub(super) fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

pub(super) fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("on the main thread")
}

/// Run `f` on the app, on the main thread, after `secs`.
pub(super) fn later(secs: f64, f: impl FnOnce(&App) + Send + 'static) {
    let when = dispatch2::DispatchTime::try_from(std::time::Duration::from_secs_f64(secs))
        .unwrap_or(dispatch2::DispatchTime::NOW);
    let _ = dispatch2::DispatchQueue::main().after(when, move || APP.with(|a| f(a.get().unwrap())));
}
