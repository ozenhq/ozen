//! The menu bar's menus and popover pieces: the right-click menu's header, the status line, the payload of a menu
//! item, and the small buttons.
use crate::App;
use objc2::rc::Retained;
use objc2_app_kit::{NSButton, NSControlSize, NSMenuItem};
use objc2_foundation::{MainThreadMarker, NSString};
use serde_json::Value;

/// A menu item's payload (JSON in its represented object).
pub(super) fn represented(item: &NSMenuItem) -> Option<Value> {
    let o = item.representedObject()?;
    serde_json::from_str(&o.downcast::<NSString>().ok()?.to_string()).ok()
}

/// The right-click menu's first line: the panel's status line, or the raw state before the first refresh.
pub(super) fn menu_header<'a>(status: &'a str, state: &'a str) -> &'a str {
    if status.is_empty() { state } else { status }
}

/// The status line while not recording: what ozen is doing instead, and the place it's at.
pub(super) fn idle_status(state: &str, queued: i64, meetings_only: bool, place: &str) -> String {
    match state {
        "paused" => "Paused".into(),
        "stopping" => "Finishing transcription…".into(),
        "processing" => format!(
            "Transcribing {queued} chunk{}…",
            if queued == 1 { "" } else { "s" }
        ),
        _ if meetings_only => format!("Waiting for a meeting{place}"),
        _ => format!("Not recording{place}"),
    }
}

pub(super) fn small_button(
    title: &NSString,
    target: &App,
    action: objc2::runtime::Sel,
    mtm: MainThreadMarker,
) -> Retained<NSButton> {
    // SAFETY: target outlives the button (the app delegate lives for the whole process).
    let b =
        unsafe { NSButton::buttonWithTitle_target_action(title, Some(target), Some(action), mtm) };
    b.setControlSize(NSControlSize::Small);
    b
}
