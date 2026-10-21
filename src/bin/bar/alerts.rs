//! Modal alerts: a yes/no confirm, a button pick, and a one-button message.
use crate::main_thread::mtm;
use objc2_app_kit::NSApplication;
use objc2_foundation::{NSString, ns_string};

/// Ask before a change: `action` or Cancel.
pub(super) fn confirm(message: &str, info: &str, action: &str) -> bool {
    let alert = objc2_app_kit::NSAlert::new(mtm());
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(info));
    alert.addButtonWithTitle(&NSString::from_str(action));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    alert.runModal() == 1000 // NSAlertFirstButtonReturn
}

/// A modal alert with `buttons`; returns the index of the one pressed.
pub(super) fn ask(title: &str, info: &str, buttons: &[&str]) -> usize {
    let mtm = mtm();
    let alert = objc2_app_kit::NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(info));
    for b in buttons {
        alert.addButtonWithTitle(&NSString::from_str(b));
    }
    NSApplication::sharedApplication(mtm).activate();
    (alert.runModal() - 1000).max(0) as usize // NSAlertFirstButtonReturn = 1000
}

pub(super) fn fail(title: &str, detail: &str) {
    ask(title, detail, &[]);
}
