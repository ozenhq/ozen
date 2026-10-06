//! The Sync section of Advanced settings (OFE-90): set up sync, pair or join a Mac, LAN only, and a status
//! line. What it shows is decided from `ozen sync status` by `view` (tested); the buttons run `ozen sync …`.
use crate::cli;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject};
use objc2::{MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAlert, NSApplication, NSButton, NSColor, NSControlStateValueOff, NSControlStateValueOn,
    NSFont, NSLayoutAttribute, NSSecureTextField, NSStackView, NSSwitch, NSTextField,
    NSUserInterfaceLayoutOrientation, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, ns_string,
};
use serde_json::Value;
use std::cell::RefCell;

/// What the section shows for one `ozen sync status`.
#[derive(Debug, PartialEq)]
pub struct View {
    pub line: String,
    /// "Set up sync" and "Join…": while this Mac isn't syncing.
    pub set_up: bool,
    /// "Pair a Mac": while it is.
    pub pair: bool,
    /// The LAN only switch, while syncing.
    pub lan_only: Option<bool>,
    /// Paused by Undo: setting up again keeps the relay or LAN only choice.
    pub paused: bool,
}

/// What "Set up sync" runs for relay `url` (empty: none given).
pub fn set_up_args(url: &str, paused: bool) -> Vec<String> {
    let args: &[&str] = match url {
        "" if paused => &["sync", "init"],
        "" => &["sync", "init", "--lan-only"],
        u => &["sync", "init", "--server", u],
    };
    args.iter().map(ToString::to_string).collect()
}

/// Days since a sync, as people say it.
fn ago(days: u64) -> String {
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        n => format!("{n} days ago"),
    }
}

pub fn view(s: &Value) -> View {
    let sync = s["sync"].as_str().unwrap_or("off");
    let lan = s["lan_only"] == true;
    let on = sync == "on";
    let head = match (sync, s["error"].as_str()) {
        ("paused", _) => "Sync is paused (after Undo). Set up sync turns it back on.".into(),
        ("on", Some(e)) => format!("Sync isn't working: {e}"),
        ("on", None) if s["running"] != true => "Sync is starting…".into(),
        ("on", None) if lan => "Sync is on, with Macs on this network only.".into(),
        ("on", None) => match s["other_macs_online"].as_u64() {
            Some(0) | None => "Sync is on. No other Mac is online now.".into(),
            Some(1) => "Sync is on. 1 other Mac is online now.".into(),
            Some(n) => format!("Sync is on. {n} other Macs are online now."),
        },
        _ => "Sync is off. Set it up on your first Mac, then Join from the others.".into(),
    };
    let macs: Vec<String> = s["macs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let (name, days) = (m["name"].as_str()?, m["days_ago"].as_u64()?);
            let late = if days >= 7 {
                " (Macs sync only while both are on and online at the same time)"
            } else {
                ""
            };
            Some(format!("Last synced with {name} {}{late}", ago(days)))
        })
        .collect();
    let mut lines = vec![head];
    if on && macs.is_empty() {
        lines.push("No other Mac has synced with this one yet: Pair a Mac to add one.".into());
    }
    lines.extend(macs);
    View {
        line: lines.join("\n"),
        set_up: !on,
        pair: on,
        lan_only: on.then_some(lan),
        paused: sync == "paused",
    }
}

define_class!(
    /// The buttons' target: runs `ozen sync …`, then shows the new state.
    // SAFETY: a plain NSObject with action methods; no ivars, no Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    struct Target;

    unsafe impl NSObjectProtocol for Target {}

    impl Target {
        #[unsafe(method(syncSetUp:))]
        fn set_up(&self, _s: Option<&AnyObject>) {
            let Some(url) = ask_text(
                "Set up sync",
                "A relay URL (wss://…) syncs with your Macs anywhere; leave it empty to sync only with Macs on this network.",
                false,
            ) else {
                return;
            };
            let paused = UI.with_borrow(|ui| ui.as_ref().is_some_and(|u| u.paused.get()));
            let args = set_up_args(&url, paused);
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            cli::run(&args, |out, err, code| done("Sync is set up", &out, &err, code));
        }

        #[unsafe(method(syncPair:))]
        fn pair(&self, _s: Option<&AnyObject>) {
            cli::run(&["sync", "pair"], |out, err, code| done("Pair a Mac", &out, &err, code));
        }

        #[unsafe(method(syncJoin:))]
        fn join(&self, _s: Option<&AnyObject>) {
            let Some(code) = ask_text(
                "Join your other Macs",
                "On a Mac that already syncs, open Advanced and press Pair a Mac, then paste its code here (or type it).",
                true,
            ) else {
                return;
            };
            // on stdin, never in argv where other processes can read it
            cli::run_input(&["sync", "join"], code + "\n", |out, err, code| {
                done("Joined", &out, &err, code)
            });
        }

        #[unsafe(method(syncLanOnly:))]
        fn lan_only(&self, sender: &NSSwitch) {
            let lan = sender.state() == NSControlStateValueOn;
            let arg = if lan { "--lan-only" } else { "--relay" };
            cli::run(&["sync", "init", arg], |_, err, code| {
                if code != 0 {
                    crate::fail("Sync", &err);
                }
                refresh();
            });
        }
    }
);

struct Ui {
    _target: Retained<Target>,
    line: Retained<NSTextField>,
    set_up: Retained<NSButton>,
    join: Retained<NSButton>,
    pair: Retained<NSButton>,
    lan_row: Retained<NSStackView>,
    lan: Retained<NSSwitch>,
    paused: std::cell::Cell<bool>,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

/// After a command: its output (or error) in an alert, then the new state.
fn done(title: &str, out: &str, err: &str, code: i32) {
    if code == 0 {
        crate::ask(title, out, &[]);
    } else {
        crate::fail("Sync", if err.is_empty() { out } else { err });
    }
    refresh();
}

/// A modal alert with a one-line field (`secret`: dots, as for a password); None on Cancel.
fn ask_text(title: &str, info: &str, secret: bool) -> Option<String> {
    let mtm = MainThreadMarker::new().expect("main thread");
    let alert = NSAlert::new(mtm);
    alert.setMessageText(&NSString::from_str(title));
    alert.setInformativeText(&NSString::from_str(info));
    alert.addButtonWithTitle(ns_string!("OK"));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(320.0, 24.0));
    let field: Retained<NSTextField> = if secret {
        // SAFETY: NSView's designated initializer
        let f: Retained<NSSecureTextField> =
            unsafe { msg_send![NSSecureTextField::alloc(mtm), initWithFrame: frame] };
        f.into_super()
    } else {
        // SAFETY: as above
        unsafe { msg_send![NSTextField::alloc(mtm), initWithFrame: frame] }
    };
    alert.setAccessoryView(Some(&field));
    alert.window().setInitialFirstResponder(Some(&field));
    NSApplication::sharedApplication(mtm).activate();
    (alert.runModal() == 1000) // NSAlertFirstButtonReturn
        .then(|| field.stringValue().to_string().trim().to_string())
}

fn button(title: &str, target: &Target, action: objc2::runtime::Sel) -> Retained<NSButton> {
    let mtm = MainThreadMarker::new().expect("main thread");
    // SAFETY: the target is kept in UI for the process's life
    unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(title),
            Some(target),
            Some(action),
            mtm,
        )
    }
}

/// The section, filled from `ozen sync status`.
pub fn section(mtm: MainThreadMarker) -> Retained<NSStackView> {
    // SAFETY: NSObject's initializer
    let target: Retained<Target> = unsafe { msg_send![Target::alloc(mtm), init] };
    let header = NSTextField::labelWithString(ns_string!("Sync"), mtm);
    header.setFont(Some(&NSFont::boldSystemFontOfSize(12.0)));
    let line = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
    line.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    line.setTextColor(Some(&NSColor::secondaryLabelColor()));
    line.setPreferredMaxLayoutWidth(420.0);
    let set_up = button("Set up sync…", &target, sel!(syncSetUp:));
    let join = button("Join…", &target, sel!(syncJoin:));
    let pair = button("Pair a Mac", &target, sel!(syncPair:));
    let buttons = NSStackView::stackViewWithViews(
        &NSArray::from_slice(&[&*set_up as &NSView, &join, &pair]),
        mtm,
    );
    let lan = NSSwitch::new(mtm);
    // SAFETY: as in `button`
    unsafe {
        lan.setTarget(Some(&target));
        lan.setAction(Some(sel!(syncLanOnly:)));
    }
    let label = NSTextField::labelWithString(ns_string!("LAN only (never use the relay)"), mtm);
    let lan_row =
        NSStackView::stackViewWithViews(&NSArray::from_slice(&[&*label as &NSView, &lan]), mtm);
    lan_row.setSpacing(12.0);
    let stack = NSStackView::stackViewWithViews(
        &NSArray::from_slice(&[&*header as &NSView, &line, &buttons, &lan_row]),
        mtm,
    );
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setAlignment(NSLayoutAttribute::Leading);
    stack.setSpacing(8.0);
    UI.with_borrow_mut(|ui| {
        *ui = Some(Ui {
            _target: target,
            line,
            set_up,
            join,
            pair,
            lan_row,
            lan,
            paused: Default::default(),
        })
    });
    refresh();
    stack
}

/// Shows the state `ozen sync status` reports now.
pub fn refresh() {
    let v = view(&cli::json(&["sync", "status"]));
    UI.with_borrow(|ui| {
        let Some(ui) = ui else { return };
        ui.line.setStringValue(&NSString::from_str(&v.line));
        ui.set_up.setHidden(!v.set_up);
        ui.join.setHidden(!v.set_up);
        ui.pair.setHidden(!v.pair);
        ui.lan_row.setHidden(v.lan_only.is_none());
        ui.paused.set(v.paused);
        ui.lan.setState(if v.lan_only == Some(true) {
            NSControlStateValueOn
        } else {
            NSControlStateValueOff
        });
    });
}

#[cfg(test)]
#[path = "sync_section_tests.rs"]
mod tests;
