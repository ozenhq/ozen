//! The record mode and the Transcribe split switch, as the app saved them in its user defaults.
use objc2_foundation::ns_string;

pub(super) fn mode() -> String {
    objc2_foundation::NSUserDefaults::standardUserDefaults()
        .stringForKey(ns_string!("mode"))
        .map_or("always".into(), |m| m.to_string()) // "always" | "meetings"
}

pub(super) fn split() -> bool {
    objc2_foundation::NSUserDefaults::standardUserDefaults().boolForKey(ns_string!("split"))
}
