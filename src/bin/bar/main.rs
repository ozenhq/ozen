//! Ozen's menu bar app, built into ~/Applications/Ozen.app by `ozen app`.
//!
//! The ear icon: left-click shows the live transcript with Start/Pause/Stop controls, right-click the same controls.
//! Everything it shows is decided by the ozen CLI (src/panel.rs and friends); this draws it and forwards clicks.
//!
//!     bar [DIR] [--open] [--dump FILE]
//!
//! DIR is the ozen checkout (default ~/ozen). --open shows the panel at launch. --dump FILE shows the panel, writes
//! what it drew (every text run with its attributes, the footer, Review queue, timeline, windows) to FILE and quits:
//! a render check (it matched the Swift app it replaced).
mod auto;
mod cli;
mod install;
mod places;
mod timebar;
mod timeline;
mod transcript;
mod voices;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSButton, NSColor,
    NSControlSize, NSEventMask, NSEventType, NSFont, NSImage, NSMenu, NSMenuItem, NSPopover,
    NSPopoverBehavior, NSScrollView, NSStackView, NSStandardKeyBindingResponding, NSStatusBar,
    NSStatusItem, NSTableViewDataSource, NSTextDelegate, NSTextField, NSTextView,
    NSTextViewDelegate, NSUserInterfaceItemIdentification, NSUserInterfaceLayoutOrientation,
    NSView, NSViewController,
};
use objc2_core_location::CLLocationManagerDelegate;
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSEdgeInsets, NSNotification, NSObject, NSObjectProtocol,
    NSPoint, NSRect, NSRectEdge, NSSize, NSString, NSTimer, ns_string,
};
use objc2_web_kit::{WKNavigationDelegate, WKScriptMessageHandler};
use serde_json::Value;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Default)]
struct Ivars {
    item: OnceCell<Retained<NSStatusItem>>,
    popover: OnceCell<Retained<NSPopover>>,
    scroll: OnceCell<Retained<NSScrollView>>,
    footer: OnceCell<Retained<NSTextField>>,
    warning: OnceCell<Retained<NSTextField>>,
    status: OnceCell<Retained<NSTextField>>,
    buttons: OnceCell<Buttons>,
    signature: RefCell<String>,
    pending: RefCell<BTreeMap<String, String>>, // tags shown right away while `ozen tag` retrains
    pending_fixes: RefCell<BTreeMap<String, String>>, // same for text fixes
    state: RefCell<String>, // `ozen status`: recording | paused | stopping | processing | stopped
    problems: RefCell<Vec<String>>, // `ozen health`
    view: RefCell<transcript::Shown>, // the last transcript drawn
    review: RefCell<Vec<String>>, // the Review queue minus lines too old to remember
    queued: Cell<i64>,      // chunks waiting to be transcribed, from `ozen controls`
    controls_asked: Cell<u64>, // only the newest `ozen controls` answer is drawn
    timeline: OnceCell<Retained<timeline::TimelineView>>,
    timeline_scroll: OnceCell<Retained<NSScrollView>>,
    view_control: OnceCell<Retained<objc2_app_kit::NSSegmentedControl>>,
    view_buttons: OnceCell<ViewButtons>,
    meetings_table: OnceCell<Retained<objc2_app_kit::NSTableView>>,
    meetings_scroll: OnceCell<Retained<NSScrollView>>,
    meetings: RefCell<Vec<Vec<String>>>, // `ozen meetings` rows: id, start, minutes, lines, first words
    auto: RefCell<auto::Memory>,
    place_now: RefCell<Option<auto::Place>>, // the place you're in, from `ozen place`
    located: Cell<bool>,                     // `ozen place` has a fix
    heard: Cell<(bool, bool)>, // the first `ozen status` and `ozen place` answers are in
    launched: OnceCell<std::time::Instant>,
    mode_control: OnceCell<Retained<objc2_app_kit::NSSegmentedControl>>,
    extra: OnceCell<Extra>,
    advanced_window: RefCell<Option<Retained<objc2_app_kit::NSWindow>>>,
    voices_window: RefCell<Option<Retained<objc2_app_kit::NSWindow>>>,
    voices_stack: OnceCell<Retained<NSStackView>>,
    voices: RefCell<Vec<Value>>, // `ozen voices`: people, this run's unnamed speakers, ignored
    timebar_window: RefCell<Option<Retained<objc2_app_kit::NSWindow>>>,
    timebar_view: OnceCell<Retained<timebar::TimebarView>>,
    timebar_timer: RefCell<Option<Retained<NSTimer>>>,
    places_window: RefCell<Option<Retained<objc2_app_kit::NSWindow>>>,
    places_stack: OnceCell<Retained<NSStackView>>,
    places_map: OnceCell<Retained<objc2_web_kit::WKWebView>>,
    places_note: OnceCell<Retained<NSTextField>>,
    setting_place: Cell<Option<usize>>, // row waiting for a location fix after "Use current location"
    picking_place: Cell<Option<usize>>, // row waiting for a map click after "Pick on map"
    rebuilding: Cell<bool>, // removing a focused field fires its action; ignore those echoes
    location: OnceCell<Retained<objc2_core_location::CLLocationManager>>,
    supplying_location: Cell<bool>, // the app writes here.json because the locate binary can't
    here: Cell<Option<(f64, f64)>>, // from `ozen place`; None until the first fix
}

struct Extra {
    ask: Retained<NSButton>,
    places: Retained<NSButton>,
    voices: Retained<NSButton>,
    timebar: Retained<NSButton>,
    advanced: Retained<NSButton>,
    quit: Retained<NSButton>,
}

struct ViewButtons {
    zoom_out: Retained<NSButton>,
    zoom_in: Retained<NSButton>,
    gather: Retained<NSButton>,
    kev: Retained<NSButton>,
}

struct Buttons {
    start: Retained<NSButton>,
    pause: Retained<NSButton>,
    stop: Retained<NSButton>,
    process: Retained<NSButton>,
    review: Retained<NSButton>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and App doesn't implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    struct App;

    unsafe impl NSObjectProtocol for App {}

    unsafe impl NSApplicationDelegate for App {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _n: &NSNotification) {
            self.setup();
        }
    }

    impl App {
        #[unsafe(method(clicked:))]
        fn clicked(&self, _sender: Option<&AnyObject>) {
            self.toggle();
        }

        #[unsafe(method(tick:))]
        fn tick(&self, _t: Option<&AnyObject>) {
            self.reload();
            self.refresh_review();
            self.refresh_state();
        }

        #[unsafe(method(startCapture:))]
        fn start_capture(&self, _s: Option<&AnyObject>) {
            self.start();
        }

        #[unsafe(method(pauseCapture:))]
        fn pause_capture(&self, _s: Option<&AnyObject>) {
            self.control("pause", "paused");
        }

        #[unsafe(method(stopCapture:))]
        fn stop_capture(&self, _s: Option<&AnyObject>) {
            self.stop();
        }

        #[unsafe(method(processQueue:))]
        fn process_queue(&self, _s: Option<&AnyObject>) {
            let stop = cli::dir().join(".processing").exists();
            cli::run(if stop { &["process", "stop"] } else { &["process"] }, |_, _, _| later(1.0, App::refresh_state));
        }

        #[unsafe(method(reviewNext:))]
        fn review_next(&self, _s: Option<&AnyObject>) {
            self.refresh_review();
            let first = self.ivars().review.borrow().first().cloned();
            let range = first.clone().and_then(|id| self.ivars().view.borrow().headers.get(&id).copied());
            if let Some((loc, len)) = range {
                let text = self.text();
                let r = objc2_foundation::NSRange::new(loc, len);
                text.scrollRangeToVisible(r);
                text.showFindIndicatorForRange(r);
                // SAFETY: layout objects of our own text view.
                let (lm, tc) = unsafe { (text.layoutManager(), text.textContainer()) };
                if let (Some(lm), Some(tc), Some(id)) = (lm, tc, first) {
                    let glyphs = unsafe { lm.glyphRangeForCharacterRange_actualCharacterRange(r, std::ptr::null_mut()) };
                    let mut rect = lm.boundingRectForGlyphRange_inTextContainer(glyphs, &tc);
                    let origin = text.textContainerOrigin();
                    rect.origin.x += origin.x;
                    rect.origin.y += origin.y + rect.size.height;
                    self.tag_menu(&id).popUpMenuPositioningItem_atLocation_inView(None, rect.origin, Some(&text));
                }
            }
        }

        #[unsafe(method(newPerson:))]
        fn new_person(&self, sender: &NSMenuItem) {
            let Some(id) = represented(sender).and_then(|v| v.as_str().map(String::from)) else { return };
            let mtm = self.mtm();
            let alert = objc2_app_kit::NSAlert::new(mtm);
            alert.setMessageText(ns_string!("Who said this line?"));
            alert.addButtonWithTitle(ns_string!("Tag"));
            alert.addButtonWithTitle(ns_string!("Cancel"));
            let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(240.0, 24.0)));
            field.setPlaceholderString(Some(ns_string!("Full name")));
            alert.setAccessoryView(Some(&field));
            alert.window().setInitialFirstResponder(Some(&field));
            NSApplication::sharedApplication(mtm).activate();
            let name = field.stringValue().to_string();
            if alert.runModal() == 1000 && !field.stringValue().to_string().trim().is_empty() {
                let _ = name;
                self.tag(vec![id], field.stringValue().to_string().trim(), None);
            }
        }

        #[unsafe(method(pick:))]
        fn pick(&self, sender: &NSMenuItem) {
            let Some(v) = represented(sender) else { return };
            if let (Some(id), Some(name)) = (v[0].as_str(), v[1].as_str()) {
                self.tag(vec![id.to_string()], name, None);
            }
        }

        #[unsafe(method(ignoreAll:))]
        fn ignore_all(&self, sender: &NSMenuItem) {
            let Some(v) = represented(sender) else { return };
            let ids: Vec<String> = v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect();
            self.tag(ids, "Ignored", Some("cd \"$OZEN_DIR\" && target/release/ozen ignore ${=OZEN_ID}"));
        }

        #[unsafe(method(modeChanged:))]
        fn mode_changed(&self, sender: Option<&AnyObject>) {
            let from_menu = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()).and_then(represented);
            let m = match from_menu.as_ref().and_then(|v| v.as_str()) {
                Some(m) => m.to_string(),
                None => if self.ivars().mode_control.get().unwrap().selectedSegment() == 1 { "meetings".into() } else { "always".into() },
            };
            // SAFETY: storing a string in the app's defaults.
            unsafe { objc2_foundation::NSUserDefaults::standardUserDefaults().setObject_forKey(Some(&NSString::from_str(&m)), ns_string!("mode")) };
            self.ivars().mode_control.get().unwrap().setSelectedSegment(if m == "meetings" { 1 } else { 0 });
            self.ivars().auto.borrow_mut().last_wanted = None; // apply the new mode right away
            self.auto_control();
        }

        #[unsafe(method(askMenu:))]
        fn ask_menu(&self, sender: &NSButton) {
            let mtm = self.mtm();
            let menu = NSMenu::new(mtm);
            for (title, tool) in [("Claude Code", "claude"), ("Hermes", "hermes")] {
                // SAFETY: the target is the app delegate, alive for the process.
                let mi = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &NSString::from_str(title), Some(sel!(askNow:)), ns_string!("")) };
                unsafe {
                    mi.setTarget(Some(self));
                    mi.setRepresentedObject(Some(&NSString::from_str(&serde_json::json!(tool).to_string())));
                }
                menu.addItem(&mi);
            }
            menu.popUpMenuPositioningItem_atLocation_inView(None, NSPoint::new(0.0, sender.bounds().size.height + 4.0), Some(sender));
        }

        #[unsafe(method(askNow:))]
        fn ask_now(&self, sender: &NSMenuItem) {
            let Some(tool) = represented(sender).and_then(|v| v.as_str().map(String::from)) else { return };
            let title = sender.title().to_string();
            self.ivars().extra.get().unwrap().ask.setEnabled(false);
            cli::run(&["live", "--open", &tool], move |_, err, code| {
                APP.with(|a| a.get().unwrap().ivars().extra.get().unwrap().ask.setEnabled(true));
                if code != 0 {
                    fail(&format!("Couldn't start {title} on this meeting"), &err);
                }
            });
        }

        // Stop returns at once (the drain runs detached), so the last words still reach the transcript after we exit.
        // Processing without a recording finishes by itself, so quitting leaves it running.
        #[unsafe(method(quitOzen:))]
        fn quit_ozen(&self, _s: Option<&AnyObject>) {
            let state = self.ivars().state.borrow().clone();
            if state == "stopped" || state == "processing" {
                NSApplication::sharedApplication(self.mtm()).terminate(None);
            } else {
                cli::run(&["stop"], |_, _, _| NSApplication::sharedApplication(mtm()).terminate(None));
            }
        }

        #[unsafe(method(didWake:))]
        fn did_wake(&self, _n: &NSNotification) {
            // The Mac may have moved while asleep: restarting the watcher sends a fresh fix within seconds; the old
            // place holds until it lands, rather than dropping to the global mode.
            cli::run(&["place", "--restart"], |out, _, _| APP.with(|a| a.get().unwrap().apply_place(&out)));
        }

        #[unsafe(method(showVoices:))]
        fn show_voices_action(&self, _s: Option<&AnyObject>) {
            self.show_voices();
        }

        #[unsafe(method(renameVoice:))]
        fn rename_voice_action(&self, sender: &NSButton) {
            if let Some(n) = sender.identifier() {
                self.rename_voice(&n.to_string());
            }
        }

        #[unsafe(method(ignoreVoice:))]
        fn ignore_voice_action(&self, sender: &NSButton) {
            if let Some(n) = sender.identifier() {
                self.ignore_voice(&n.to_string());
            }
        }

        #[unsafe(method(forgetVoice:))]
        fn forget_voice_action(&self, sender: &NSButton) {
            if let Some(n) = sender.identifier() {
                self.forget_voice(&n.to_string());
            }
        }

        #[unsafe(method(showVoiceLine:))]
        fn show_voice_line_action(&self, sender: &NSButton) {
            if let Some(id) = sender.identifier() {
                self.show_voice_line(&id.to_string());
            }
        }

        #[unsafe(method(showTimebar:))]
        fn show_timebar_action(&self, _s: Option<&AnyObject>) {
            self.show_timebar();
        }

        #[unsafe(method(timebarTick:))]
        fn timebar_tick_action(&self, _t: Option<&AnyObject>) {
            self.timebar_tick();
        }

        #[unsafe(method(showPlaces:))]
        fn show_places_action(&self, _s: Option<&AnyObject>) {
            self.show_places();
        }

        #[unsafe(method(renamePlace:))]
        fn rename_place_action(&self, sender: &NSTextField) {
            self.rename_place(sender);
        }

        #[unsafe(method(placeActionChanged:))]
        fn place_action_action(&self, sender: &objc2_app_kit::NSPopUpButton) {
            self.place_action_changed(sender);
        }

        #[unsafe(method(placeValueChanged:))]
        fn place_value_action(&self, sender: &NSTextField) {
            self.place_value_changed(sender);
        }

        #[unsafe(method(setPlaceHere:))]
        fn set_place_here_action(&self, sender: &AnyObject) {
            self.set_place_here(places::row_of(sender));
        }

        #[unsafe(method(pickOnMap:))]
        fn pick_on_map_action(&self, sender: &AnyObject) {
            self.pick_on_map(places::row_of(sender));
        }

        #[unsafe(method(removePlace:))]
        fn remove_place_action(&self, sender: &AnyObject) {
            self.remove_place(places::row_of(sender));
        }

        #[unsafe(method(addPlace:))]
        fn add_place_action(&self, _s: Option<&AnyObject>) {
            self.add_place();
        }

        #[unsafe(method(showAdvanced:))]
        fn show_advanced(&self, _s: Option<&AnyObject>) {
            self.advanced();
        }

        #[unsafe(method(splitChanged:))]
        fn split_changed(&self, sender: &NSButton) {
            objc2_foundation::NSUserDefaults::standardUserDefaults().setBool_forKey(sender.state() == objc2_app_kit::NSControlStateValueOn, ns_string!("split"));
            // A recording in progress switches now. Turning split on keeps the live transcriber going as processing
            // (Stop processing ends it), so nothing already heard waits.
            let state = self.ivars().state.borrow().clone();
            if state == "recording" {
                cli::run(&[if split() { "record" } else { "start" }], |_, _, _| APP.with(|a| a.get().unwrap().refresh_state()));
            }
            self.show_state(&state);
        }

        #[unsafe(method(switchView:))]
        fn switch_view(&self, _s: Option<&AnyObject>) {
            self.view_switched();
        }

        #[unsafe(method(zoom:))]
        fn zoom(&self, sender: &NSButton) {
            let iv = self.ivars();
            let (tl, scroll) = (iv.timeline.get().unwrap(), iv.timeline_scroll.get().unwrap());
            let clip = scroll.contentView();
            let anchor = (clip.bounds().origin.x + clip.bounds().size.width / 2.0) / tl.bounds().size.width.max(1.0); // keep the view centered
            let zoom_in = std::ptr::eq(sender, &*iv.view_buttons.get().unwrap().zoom_in);
            tl.set_px((tl.px() * if zoom_in { 2.0 } else { 0.5 }).clamp(0.25, 32.0));
            objc2_foundation::NSUserDefaults::standardUserDefaults().setDouble_forKey(tl.px(), ns_string!("pxPerSec"));
            let w = clip.bounds().size.width;
            clip.scrollToPoint(NSPoint::new((anchor * tl.bounds().size.width - w / 2.0).max(0.0), 0.0));
            scroll.reflectScrolledClipView(&clip);
        }

        #[unsafe(method(boundsChanged:))]
        fn bounds_changed(&self, _n: &NSNotification) {
            self.ivars().timeline.get().unwrap().setNeedsDisplay(true); // repin names
        }

        #[unsafe(method(gather:))]
        fn gather(&self, sender: Option<&AnyObject>) {
            let iv = self.ivars();
            let table = iv.meetings_table.get().unwrap();
            let meetings = iv.meetings.borrow();
            let rows = table.selectedRowIndexes();
            let ids: Vec<String> = (0..meetings.len()).filter(|&i| rows.containsIndex(i)).map(|i| meetings[i][0].clone()).collect();
            if ids.is_empty() {
                return;
            }
            let b = iv.view_buttons.get().unwrap();
            let kev = sender.is_some_and(|s| std::ptr::eq(s, &**b.kev as &AnyObject));
            b.kev.setEnabled(false);
            b.gather.setEnabled(false);
            if kev {
                b.kev.setTitle(ns_string!("Asking Kev…"));
            }
            let mut args = vec!["gather".to_string()];
            if kev {
                args.push("--kev".into());
            }
            args.extend(ids);
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            cli::run(&refs, move |out, err, code| {
                APP.with(|a| {
                    let app = a.get().unwrap();
                    let b = app.ivars().view_buttons.get().unwrap();
                    b.kev.setEnabled(true);
                    b.gather.setEnabled(true);
                    b.kev.setTitle(ns_string!("Auto add with Kev"));
                    let lines: Vec<&str> = out.lines().collect(); // files written, then the folder
                    let Some((folder, files)) = lines.split_last().filter(|_| code == 0) else {
                        return fail("Couldn't gather the transcripts", &err);
                    };
                    let mut info = files.join("\n");
                    if kev {
                        info += &format!("\n\nKev's scores:\n{err}");
                    }
                    info += &format!("\n\n{folder}");
                    let n = files.len();
                    let what = ask(&format!("{n} transcript{} ready", if n == 1 { "" } else { "s" }), &info, &["Claude Code", "Hermes", "Show in Finder"]);
                    let what = ["claude", "hermes", "finder"][what.min(2)];
                    let folder = folder.to_string();
                    cli::run(&["open", &folder, what], move |_, err, code| {
                        if code != 0 {
                            fail(&format!("Couldn't open {what}"), &err);
                        }
                    });
                })
            });
        }
    }

    unsafe impl NSTextDelegate for App {}

    unsafe impl NSTextViewDelegate for App {
        #[unsafe(method(textView:clickedOnLink:atIndex:))]
        fn clicked_on_link(&self, view: &NSTextView, link: &AnyObject, _index: usize) -> objc2::runtime::Bool {
            let Ok(url) = link.downcast_ref::<objc2_foundation::NSURL>().ok_or(()) else { return objc2::runtime::Bool::NO };
            let (host, last) = (url.host().map(|h| h.to_string()), url.lastPathComponent().map(|c| c.to_string()).unwrap_or_default());
            match host.as_deref() {
                Some("fix") => {
                    self.fix_text(&last);
                    objc2::runtime::Bool::YES
                }
                Some("tag") => {
                    if let Some(event) = NSApplication::sharedApplication(self.mtm()).currentEvent() {
                        NSMenu::popUpContextMenu_withEvent_forView(&self.tag_menu(&last), &event, view);
                    }
                    objc2::runtime::Bool::YES
                }
                _ => objc2::runtime::Bool::NO,
            }
        }
    }

    unsafe impl WKNavigationDelegate for App {
        #[unsafe(method(webView:didFinishNavigation:))]
        fn did_finish(&self, _view: &objc2_web_kit::WKWebView, _n: Option<&objc2_web_kit::WKNavigation>) {
            self.show_places_on_map(true);
        }
    }

    unsafe impl WKScriptMessageHandler for App {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn did_receive(&self, _c: &objc2_web_kit::WKUserContentController, message: &objc2_web_kit::WKScriptMessage) {
            // SAFETY: the message body is a plain JSON-compatible object from map.html.
            let body = unsafe { message.body() };
            let json = unsafe { objc2_foundation::NSJSONSerialization::dataWithJSONObject_options_error(&body, objc2_foundation::NSJSONWritingOptions::empty()) };
            if let Some(v) = json.ok().and_then(|d| serde_json::from_slice::<Value>(&d.to_vec()).ok()) {
                self.map_message(&v);
            }
        }
    }

    unsafe impl CLLocationManagerDelegate for App {
        #[unsafe(method(locationManager:didUpdateLocations:))]
        fn did_update(&self, _m: &objc2_core_location::CLLocationManager, locations: &objc2_foundation::NSArray<objc2_core_location::CLLocation>) {
            if let Some(fix) = locations.lastObject() {
                // SAFETY: reading a delivered location.
                let (c, t) = unsafe { (fix.coordinate(), fix.timestamp().timeIntervalSince1970()) };
                self.location_update(c.latitude, c.longitude, t);
            }
        }

        #[unsafe(method(locationManagerDidChangeAuthorization:))]
        fn did_change_authorization(&self, _m: &objc2_core_location::CLLocationManager) {
            self.authorization_changed();
        }
    }

    unsafe impl NSTableViewDataSource for App {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn number_of_rows(&self, _t: &objc2_app_kit::NSTableView) -> isize {
            self.ivars().meetings.borrow().len() as isize
        }

        #[unsafe(method_id(tableView:objectValueForTableColumn:row:))]
        fn value(&self, _t: &objc2_app_kit::NSTableView, col: Option<&objc2_app_kit::NSTableColumn>, row: isize) -> Option<Retained<AnyObject>> {
            let id = col.map(|c| c.identifier().to_string()).unwrap_or_default();
            let cell = meeting_cell(&self.ivars().meetings.borrow(), &id, row as usize);
            Some(Retained::into_super(Retained::into_super(NSString::from_str(&cell))))
        }
    }
);

/// A menu item's payload (JSON in its represented object).
fn represented(item: &NSMenuItem) -> Option<Value> {
    let o = item.representedObject()?;
    serde_json::from_str(&o.downcast::<NSString>().ok()?.to_string()).ok()
}

/// The Meetings table's cell in column `col` of `row`: When, Min, Lines, else the first words.
fn meeting_cell(meetings: &[Vec<String>], col: &str, row: usize) -> String {
    let i = match col {
        "When" => 1,
        "Min" => 2,
        "Lines" => 3,
        _ => 4,
    };
    meetings
        .get(row)
        .and_then(|m| m.get(i))
        .cloned()
        .unwrap_or_default()
}

/// Ask before a change: `action` or Cancel.
fn confirm(message: &str, info: &str, action: &str) -> bool {
    let alert = objc2_app_kit::NSAlert::new(mtm());
    alert.setMessageText(&NSString::from_str(message));
    alert.setInformativeText(&NSString::from_str(info));
    alert.addButtonWithTitle(&NSString::from_str(action));
    alert.addButtonWithTitle(ns_string!("Cancel"));
    alert.runModal() == 1000 // NSAlertFirstButtonReturn
}

/// A modal alert with `buttons`; returns the index of the one pressed.
fn ask(title: &str, info: &str, buttons: &[&str]) -> usize {
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

fn fail(title: &str, detail: &str) {
    ask(title, detail, &[]);
}

fn mode() -> String {
    objc2_foundation::NSUserDefaults::standardUserDefaults()
        .stringForKey(ns_string!("mode"))
        .map_or("always".into(), |m| m.to_string()) // "always" | "meetings"
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

fn split() -> bool {
    objc2_foundation::NSUserDefaults::standardUserDefaults().boolForKey(ns_string!("split"))
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("on the main thread")
}

thread_local! {
    static APP: OnceCell<Retained<App>> = const { OnceCell::new() };
}

/// Run `f` on the app, on the main thread, after `secs`.
fn later(secs: f64, f: impl FnOnce(&App) + Send + 'static) {
    let when = dispatch2::DispatchTime::try_from(std::time::Duration::from_secs_f64(secs))
        .unwrap_or(dispatch2::DispatchTime::NOW);
    let _ = dispatch2::DispatchQueue::main().after(when, move || APP.with(|a| f(a.get().unwrap())));
}

fn small_button(
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

impl App {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars::default());
        // SAFETY: NSObject's init.
        unsafe { msg_send![super(this), init] }
    }

    fn text(&self) -> Retained<NSTextView> {
        let doc = self.ivars().scroll.get().unwrap().documentView().unwrap();
        doc.downcast::<NSTextView>().unwrap()
    }

    fn setup(&self) {
        let mtm = self.mtm();
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(-2.0); // NSSquareStatusItemLength
        let button = item.button(mtm).unwrap();
        button.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("ear"),
                Some(ns_string!("ozen transcript")),
            )
            .as_deref(),
        );
        // SAFETY: the target is the app delegate, alive for the process.
        unsafe {
            button.setTarget(Some(self));
            button.setAction(Some(sel!(clicked:)));
        }
        button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);

        let scroll = NSTextView::scrollableTextView(mtm);
        let text = scroll
            .documentView()
            .unwrap()
            .downcast::<NSTextView>()
            .unwrap();
        text.setEditable(false);
        text.setTextContainerInset(NSSize::new(10.0, 10.0));
        text.setDelegate(Some(ProtocolObject::from_ref(self)));
        // links keep their own colors (names, unsure, text): only the cursor says they're clickable
        // SAFETY: AppKit's cursor attribute key.
        let cursor = objc2_app_kit::NSCursor::pointingHandCursor();
        let link_attrs = NSDictionary::from_slices(
            &[unsafe { objc2_app_kit::NSCursorAttributeName }],
            &[&*cursor as &AnyObject],
        );
        unsafe { text.setLinkTextAttributes(Some(&link_attrs)) };
        let footer = NSTextField::labelWithString(ns_string!(""), mtm);
        footer.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        footer.setTextColor(Some(&NSColor::secondaryLabelColor()));
        let status = NSTextField::labelWithString(ns_string!(""), mtm);
        status.setFont(Some(&NSFont::boldSystemFontOfSize(12.0)));
        let warning = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
        warning.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        warning.setTextColor(Some(&NSColor::systemOrangeColor()));
        let buttons = Buttons {
            start: small_button(ns_string!("Start"), self, sel!(startCapture:), mtm),
            pause: small_button(ns_string!("Pause"), self, sel!(pauseCapture:), mtm),
            stop: small_button(ns_string!("Stop"), self, sel!(stopCapture:), mtm),
            process: small_button(ns_string!("Process"), self, sel!(processQueue:), mtm),
            review: small_button(ns_string!("Review"), self, sel!(reviewNext:), mtm),
        };
        buttons.process.setToolTip(Some(ns_string!(
            "Transcribe the recorded audio that's waiting, then stop"
        )));
        let spacer = NSView::new(mtm);
        let mode_labels = objc2_foundation::NSArray::from_retained_slice(&[
            NSString::from_str("Always"),
            NSString::from_str("Meetings"),
        ]);
        // SAFETY: the target is the app delegate, alive for the process.
        let mode_control = unsafe {
            objc2_app_kit::NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &mode_labels,
                objc2_app_kit::NSSegmentSwitchTracking::SelectOne,
                Some(self),
                Some(sel!(modeChanged:)),
                mtm,
            )
        };
        mode_control.setControlSize(NSControlSize::Small);
        mode_control.setSelectedSegment(if mode() == "meetings" { 1 } else { 0 });
        mode_control.setToolTip(Some(ns_string!("Always: record until you stop. Meetings: start and stop automatically with Zoom/Meet/Teams/Slack/FaceTime calls.")));
        let extra = Extra {
            ask: small_button(ns_string!("Ask AI"), self, sel!(askMenu:), mtm),
            places: small_button(ns_string!("Places…"), self, sel!(showPlaces:), mtm),
            voices: small_button(ns_string!("Voices…"), self, sel!(showVoices:), mtm),
            timebar: small_button(ns_string!("Timebar…"), self, sel!(showTimebar:), mtm),
            advanced: small_button(ns_string!(""), self, sel!(showAdvanced:), mtm),
            quit: small_button(ns_string!("Quit"), self, sel!(quitOzen:), mtm),
        };
        extra.ask.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("sparkles"),
                Some(ns_string!("AI")),
            )
            .as_deref(),
        ); // the usual mark for AI features
        extra
            .ask
            .setImagePosition(objc2_app_kit::NSCellImagePosition::ImageLeading);
        extra.ask.setToolTip(Some(ns_string!(
            "Start Claude Code or Hermes on the meeting happening now (ozen live)"
        )));
        extra.advanced.setImage(
            NSImage::imageWithSystemSymbolName_accessibilityDescription(
                ns_string!("gearshape"),
                Some(ns_string!("Advanced settings")),
            )
            .as_deref(),
        );
        extra
            .advanced
            .setImagePosition(objc2_app_kit::NSCellImagePosition::ImageOnly);
        extra
            .advanced
            .setToolTip(Some(ns_string!("Advanced settings")));
        let row: [&NSView; 14] = [
            &status,
            &spacer,
            &extra.ask,
            &mode_control,
            &extra.places,
            &extra.voices,
            &extra.timebar,
            &buttons.review,
            &buttons.start,
            &buttons.pause,
            &buttons.stop,
            &buttons.process,
            &extra.advanced,
            &extra.quit,
        ];
        let controls =
            NSStackView::stackViewWithViews(&objc2_foundation::NSArray::from_slice(&row), mtm);
        controls.setEdgeInsets(NSEdgeInsets {
            top: 8.0,
            left: 12.0,
            bottom: 0.0,
            right: 12.0,
        });
        let warning_row = NSStackView::stackViewWithViews(
            &objc2_foundation::NSArray::from_slice(&[&*warning as &NSView]),
            mtm,
        );
        warning_row.setEdgeInsets(NSEdgeInsets {
            top: 0.0,
            left: 12.0,
            bottom: 0.0,
            right: 12.0,
        });
        let defaults = objc2_foundation::NSUserDefaults::standardUserDefaults();
        let labels = objc2_foundation::NSArray::from_retained_slice(&[
            NSString::from_str("Transcript"),
            NSString::from_str("Timeline"),
            NSString::from_str("Meetings"),
        ]);
        // SAFETY: the target is the app delegate, alive for the process.
        let view_control = unsafe {
            objc2_app_kit::NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &labels,
                objc2_app_kit::NSSegmentSwitchTracking::SelectOne,
                Some(self),
                Some(sel!(switchView:)),
                mtm,
            )
        };
        view_control.setControlSize(NSControlSize::Small);
        view_control.setSelectedSegment(defaults.integerForKey(ns_string!("view")));
        let vb = ViewButtons {
            zoom_out: small_button(ns_string!("−"), self, sel!(zoom:), mtm),
            zoom_in: small_button(ns_string!("+"), self, sel!(zoom:), mtm),
            gather: small_button(ns_string!("Open"), self, sel!(gather:), mtm),
            kev: small_button(ns_string!("Auto add with Kev"), self, sel!(gather:), mtm),
        };
        vb.gather.setToolTip(Some(ns_string!("Put the selected meetings' transcripts in a folder and start Claude Code or Hermes there")));
        vb.kev.setToolTip(Some(ns_string!(
            "Same, plus every other meeting Kev (localhost:8009) judges related"
        )));
        let tl = timeline::TimelineView::new(mtm);
        let px = defaults.doubleForKey(ns_string!("pxPerSec"));
        tl.set_px(if px > 0.0 { px } else { 4.0 });
        tl.set_on_select(|id| {
            let id = id.to_string();
            later(0.0, move |app| app.jump(&id));
        });
        let timeline_scroll = NSScrollView::new(mtm);
        timeline_scroll.setDocumentView(Some(&tl));
        timeline_scroll.setHasHorizontalScroller(true);
        timeline_scroll.setHasVerticalScroller(true);
        timeline_scroll.setAutohidesScrollers(true);
        let clip = timeline_scroll.contentView();
        clip.setPostsBoundsChangedNotifications(true);
        // SAFETY: the observer is the app delegate, alive for the process.
        unsafe {
            objc2_foundation::NSNotificationCenter::defaultCenter()
                .addObserver_selector_name_object(
                    self,
                    sel!(boundsChanged:),
                    Some(objc2_app_kit::NSViewBoundsDidChangeNotification),
                    Some(&clip),
                );
        }
        let table = objc2_app_kit::NSTableView::new(mtm);
        for (title, width) in [
            ("When", 120.0),
            ("Min", 40.0),
            ("Lines", 44.0),
            ("Starts with", 380.0),
        ] {
            let col = objc2_app_kit::NSTableColumn::initWithIdentifier(
                objc2_app_kit::NSTableColumn::alloc(mtm),
                &NSString::from_str(title),
            );
            col.setTitle(&NSString::from_str(title));
            col.setWidth(width);
            table.addTableColumn(&col);
        }
        table.setAllowsMultipleSelection(true);
        table.setUsesAlternatingRowBackgroundColors(true);
        // SAFETY: the data source and target are the app delegate, alive for the process.
        unsafe {
            table.setDataSource(Some(ProtocolObject::from_ref(self)));
            table.setTarget(Some(self));
            table.setDoubleAction(Some(sel!(gather:)));
        }
        let meetings_scroll = NSScrollView::new(mtm);
        meetings_scroll.setDocumentView(Some(&table));
        meetings_scroll.setHasVerticalScroller(true);
        let spacer2 = NSView::new(mtm);
        let view_row_parts: [&NSView; 6] = [
            &view_control,
            &spacer2,
            &vb.zoom_out,
            &vb.zoom_in,
            &vb.kev,
            &vb.gather,
        ];
        let view_row = NSStackView::stackViewWithViews(
            &objc2_foundation::NSArray::from_slice(&view_row_parts),
            mtm,
        );
        view_row.setEdgeInsets(NSEdgeInsets {
            top: 0.0,
            left: 12.0,
            bottom: 0.0,
            right: 12.0,
        });
        let parts: [&NSView; 7] = [
            &controls,
            &warning_row,
            &view_row,
            &scroll,
            &timeline_scroll,
            &meetings_scroll,
            &footer,
        ];
        let stack =
            NSStackView::stackViewWithViews(&objc2_foundation::NSArray::from_slice(&parts), mtm);
        stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        stack.setEdgeInsets(NSEdgeInsets {
            top: 0.0,
            left: 0.0,
            bottom: 8.0,
            right: 0.0,
        });
        stack.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(640.0, 680.0),
        ));
        let vc = NSViewController::new(mtm);
        vc.setView(&stack);
        let popover = NSPopover::new(mtm);
        popover.setContentViewController(Some(&vc));
        popover.setBehavior(NSPopoverBehavior::Transient);

        let iv = self.ivars();
        let _ = iv.mode_control.set(mode_control);
        let _ = iv.extra.set(extra);
        let _ = iv.launched.set(std::time::Instant::now());
        // SAFETY: the observer is the app delegate, alive for the process.
        unsafe {
            objc2_app_kit::NSWorkspace::sharedWorkspace()
                .notificationCenter()
                .addObserver_selector_name_object(
                    self,
                    sel!(didWake:),
                    Some(objc2_app_kit::NSWorkspaceDidWakeNotification),
                    None,
                );
        }
        let _ = iv.timeline.set(tl);
        let _ = iv.timeline_scroll.set(timeline_scroll);
        let _ = iv.view_control.set(view_control);
        let _ = iv.view_buttons.set(vb);
        let _ = iv.meetings_table.set(table);
        let _ = iv.meetings_scroll.set(meetings_scroll);
        let _ = iv.item.set(item);
        let _ = iv.popover.set(popover);
        let _ = iv.scroll.set(scroll);
        let _ = iv.footer.set(footer);
        let _ = iv.status.set(status);
        let _ = iv.warning.set(warning);
        let _ = iv.buttons.set(buttons);
        *iv.state.borrow_mut() = "stopped".into();
        self.ask_location();
        self.apply_view();
        // SAFETY: the target is the app delegate, alive for the process.
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                2.0,
                self,
                sel!(tick:),
                None,
                true,
            );
        }
        self.refresh_state();
        let args: Vec<String> = std::env::args().collect();
        if args.iter().any(|a| a == "--open") {
            later(1.0, App::toggle);
        }
        if let Some(file) = args
            .iter()
            .position(|a| a == "--dump")
            .and_then(|i| args.get(i + 1))
            .cloned()
        {
            later(2.0, move |app| {
                app.toggle();
                let mut out = app.dump(&file);
                // then the Meetings tab, once `ozen meetings` has answered
                app.ivars()
                    .view_control
                    .get()
                    .unwrap()
                    .setSelectedSegment(2);
                app.apply_view();
                later(3.0, move |app| {
                    out += &app.dump_meetings();
                    out += &app.dump_pending();
                    app.show_voices();
                    later(3.0, move |app| {
                        out += &app.dump_window(
                            app.ivars().voices_window.borrow().as_ref(),
                            &format!("{file}.voices.png"),
                        );
                        app.show_timebar();
                        later(4.0, move |app| {
                            let out = format!(
                                "{out}\n\n== timebar\n{}",
                                app.timebar_view().dump(&format!("{file}.timebar.png"))
                            );
                            later(0.0, move |app| app.dump_places(file, out));
                        });
                    });
                });
            });
        }
    }

    /// Left click: show or hide the panel. Right click: the controls as a menu.
    fn toggle(&self) {
        let mtm = self.mtm();
        let iv = self.ivars();
        let (item, popover) = (iv.item.get().unwrap(), iv.popover.get().unwrap());
        let right = NSApplication::sharedApplication(mtm)
            .currentEvent()
            .is_some_and(|e| e.r#type() == NSEventType::RightMouseUp);
        if right {
            let menu = self.right_menu();
            item.setMenu(Some(&menu));
            if let Some(b) = item.button(mtm) {
                // SAFETY: shows the menu, as a click would.
                unsafe { b.performClick(None) };
            }
            item.setMenu(None); // keep left-click for the panel
            return;
        }
        if popover.isShown() {
            // SAFETY: closes the panel; nothing depends on it staying open.
            unsafe { popover.performClose(None) };
        } else {
            iv.signature.borrow_mut().clear();
            let b = item.button(mtm).unwrap();
            popover.showRelativeToRect_ofView_preferredEdge(b.bounds(), &b, NSRectEdge::MinY);
            NSApplication::sharedApplication(mtm).activate();
            self.reload();
            // SAFETY: scrolls the app's own text view.
            unsafe { self.text().scrollToEndOfDocument(None) };
        }
    }

    fn right_menu(&self) -> Retained<NSMenu> {
        let mtm = self.mtm();
        let menu = NSMenu::new(mtm);
        let add = |title: &str, action: Option<objc2::runtime::Sel>, key: &str| {
            // SAFETY: plain menu item; the target (when set) is the app delegate, alive for the process.
            let mi = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    action,
                    &NSString::from_str(key),
                )
            };
            if action.is_some() {
                unsafe { mi.setTarget(Some(self)) };
            }
            menu.addItem(&mi);
            mi
        };
        add(&format!("ozen: {}", self.ivars().state.borrow()), None, "");
        for p in self.ivars().problems.borrow().iter() {
            add(&format!("⚠︎ {p}"), None, "");
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let b = self.ivars().buttons.get().unwrap();
        for (btn, sel) in [
            (&b.start, sel!(startCapture:)),
            (&b.pause, sel!(pauseCapture:)),
            (&b.stop, sel!(stopCapture:)),
            (&b.process, sel!(processQueue:)),
        ] {
            if !btn.isHidden() && btn.isEnabled() {
                add(&btn.title().to_string(), Some(sel), "");
            }
        }
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        let mode = mode();
        for (title, m) in [
            ("Record always", "always"),
            ("Record only meetings", "meetings"),
        ] {
            let mi = add(title, Some(sel!(modeChanged:)), "");
            // SAFETY: payload for modeChanged:.
            unsafe {
                mi.setRepresentedObject(Some(&NSString::from_str(
                    &serde_json::json!(m).to_string(),
                )))
            };
            mi.setState(if mode == m {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
        }
        add("Places…", Some(sel!(showPlaces:)), "");
        add("Voices…", Some(sel!(showVoices:)), "");
        add("Timebar…", Some(sel!(showTimebar:)), "");
        add("Advanced…", Some(sel!(showAdvanced:)), "");
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        add("Quit ozen", Some(sel!(quitOzen:)), "q");
        let quit_bar = add("Quit bar, keep recording", Some(sel!(terminate:)), "");
        // SAFETY: terminate: goes to the application, not the delegate.
        unsafe { quit_bar.setTarget(Some(&NSApplication::sharedApplication(mtm))) };
        menu
    }

    fn reload(&self) {
        let iv = self.ivars();
        if !iv.popover.get().unwrap().isShown() {
            return;
        }
        let files = [
            "lines.jsonl",
            "tags.json",
            "labels.json",
            "stats.json",
            "fixes.json",
            "junk.json",
        ];
        let sig: String = files
            .iter()
            .map(|f| {
                let m = std::fs::metadata(cli::dir().join(f)).ok();
                format!(
                    "{}-{:?}",
                    m.as_ref().map_or(0, std::fs::Metadata::len),
                    m.and_then(|m| m.modified().ok())
                )
            })
            .collect::<Vec<_>>()
            .join("|")
            + &self.pending_json();
        if *iv.signature.borrow() == sig {
            return;
        }
        *iv.signature.borrow_mut() = sig;
        let scroll = iv.scroll.get().unwrap();
        let at_bottom = scroll
            .verticalScroller()
            .is_none_or(|s| s.floatValue() > 0.98);
        let view = cli::json(&["transcript", &self.pending_json()]);
        let (text, shown) = transcript::render(&view, self.mtm());
        let t = self.text();
        // SAFETY: the text view's storage exists once it's set up.
        unsafe { t.textStorage().unwrap().setAttributedString(&text) };
        if at_bottom {
            // SAFETY: scrolls the app's own text view.
            unsafe { t.scrollToEndOfDocument(None) };
        }
        iv.footer
            .get()
            .unwrap()
            .setStringValue(&NSString::from_str(&shown.footer));
        let tl_scroll = iv.timeline_scroll.get().unwrap();
        let tl = iv.timeline.get().unwrap();
        let clip = tl_scroll.contentView().bounds();
        let at_end = clip.origin.x + clip.size.width >= tl.bounds().size.width - 20.0;
        tl.set_segments(shown.segments.clone());
        if at_end {
            self.scroll_timeline_to_end();
        }
        *iv.view.borrow_mut() = shown;
        self.refresh_review();
    }

    /// Only ask about recent lines: after 10 minutes nobody remembers who said what.
    fn refresh_review(&self) {
        let iv = self.ivars();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs_f64();
        let review: Vec<String> = iv
            .view
            .borrow()
            .review
            .iter()
            .filter(|(_, until)| *until >= now)
            .map(|(id, _)| id.clone())
            .collect();
        let b = &iv.buttons.get().unwrap().review;
        b.setTitle(&NSString::from_str(&if review.is_empty() {
            "Review".into()
        } else {
            format!("Review {}", review.len())
        }));
        b.setEnabled(!review.is_empty());
        *iv.review.borrow_mut() = review;
    }

    fn refresh_state(&self) {
        cli::run(&["place"], |out, _, _| {
            APP.with(|a| a.get().unwrap().apply_place(&out))
        });
        cli::run(&["status"], |out, _, _| {
            APP.with(|a| {
                let app = a.get().unwrap();
                let (_, place) = app.ivars().heard.get();
                app.ivars().heard.set((true, place));
                app.show_state(&out);
                app.auto_control();
            })
        });
        cli::run(&["health"], |out, _, _| {
            APP.with(|a| {
                let app = a.get().unwrap();
                *app.ivars().problems.borrow_mut() = out
                    .lines()
                    .map(String::from)
                    .filter(|l| !l.is_empty())
                    .collect();
                let s = app.ivars().state.borrow().clone();
                app.show_state(&s);
            })
        });
    }

    fn show_state(&self, s: &str) {
        let iv = self.ivars();
        *iv.state.borrow_mut() = s.to_string();
        let problems = iv.problems.borrow().clone();
        let icon = if s == "recording" && !problems.is_empty() {
            "ear.trianglebadge.exclamationmark"
        } else {
            match s {
                "recording" => "ear.fill",
                "paused" => "pause.circle",
                "stopping" | "processing" => "hourglass",
                _ => "ear",
            }
        };
        let warning = iv.warning.get().unwrap();
        warning.setStringValue(&NSString::from_str(
            &problems
                .iter()
                .map(|p| format!("⚠︎ {p}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        // SAFETY: the warning label sits in its own row view.
        if let Some(row) = unsafe { warning.superview() } {
            row.setHidden(problems.is_empty());
        }
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
            &NSString::from_str(icon),
            Some(&NSString::from_str(&format!("ozen {s}"))),
        );
        let item = iv.item.get().unwrap();
        let button = item.button(self.mtm()).unwrap();
        // Only recording is red and filled; every other state is the plain monochrome menu bar icon.
        let image = if s == "recording" {
            image.and_then(|i| {
                let red = objc2_app_kit::NSImageSymbolConfiguration::configurationWithPaletteColors(
                    &objc2_foundation::NSArray::from_slice(&[&*NSColor::systemRedColor()]),
                );
                i.imageWithSymbolConfiguration(&red)
                    .inspect(|i| i.setTemplate(false))
            })
        } else {
            image
        };
        button.setImage(image.as_deref());
        let processing = cli::dir().join(".processing").exists();
        let record_only = cli::dir().join(".record-only").exists();
        let queued = iv.queued.get();
        let place = iv.place_now.borrow().clone();
        let meetings_only = place
            .as_ref()
            .map_or(mode() == "meetings", |p| p.action == "meetings");
        let meeting_now = auto::in_meeting(&iv.auto.borrow(), now());
        let place_suffix = place
            .as_ref()
            .map_or(String::new(), |p| format!(" · {}", p.label));
        let meeting = if meetings_only && meeting_now {
            format!(
                " · {}",
                iv.auto
                    .borrow()
                    .meeting_name
                    .clone()
                    .unwrap_or_else(|| "meeting".into())
            )
        } else {
            String::new()
        } + &place_suffix;
        let line = if s == "recording" {
            format!(
                "● Recording{meeting}{}",
                if record_only && !processing {
                    " · not transcribing"
                } else {
                    ""
                }
            )
        } else {
            let what = match s {
                "paused" => "paused".to_string(),
                "stopping" => "finishing transcription…".to_string(),
                "processing" => format!("processing {queued} chunks…"),
                _ => format!(
                    "{}{place_suffix}",
                    if meetings_only {
                        "waiting for a meeting"
                    } else {
                        "stopped"
                    }
                ),
            };
            format!("Not recording · {what}")
        };
        let status = iv.status.get().unwrap();
        status.setStringValue(&NSString::from_str(&line));
        button.setToolTip(Some(&NSString::from_str(&format!("Ozen: {line}"))));
        status.setTextColor(Some(&*if s == "recording" {
            NSColor::systemRedColor()
        } else {
            NSColor::secondaryLabelColor()
        }));
        iv.controls_asked.set(iv.controls_asked.get() + 1);
        let asked = iv.controls_asked.get();
        let state = s.to_string();
        let args = ["controls", s, if split() { "split" } else { "" }];
        cli::run(&args, move |out, _, _| {
            APP.with(|a| {
                let app = a.get().unwrap();
                if asked != app.ivars().controls_asked.get() {
                    return;
                }
                let Ok(c) = serde_json::from_str::<Value>(&out) else {
                    return;
                };
                let b = app.ivars().buttons.get().unwrap();
                for (btn, name) in [
                    (&b.start, "start"),
                    (&b.pause, "pause"),
                    (&b.stop, "stop"),
                    (&b.process, "process"),
                ] {
                    let c = &c[name];
                    if let Some(t) = c["title"].as_str() {
                        btn.setTitle(&NSString::from_str(t));
                    }
                    btn.setEnabled(c["enabled"].as_bool().unwrap_or(false));
                    btn.setHidden(c["hidden"].as_bool().unwrap_or(false));
                }
                let q = c["queued"].as_i64().unwrap_or(0);
                if q != app.ivars().queued.get() {
                    app.ivars().queued.set(q);
                    app.show_state(&state); // the status line counts them
                }
            })
        });
    }

    /// {"tags": ..., "fixes": ...} the panel shows before `ozen tag` / `ozen fix` write them.
    fn pending_json(&self) -> String {
        serde_json::json!({"tags": *self.ivars().pending.borrow(), "fixes": *self.ivars().pending_fixes.borrow()}).to_string()
    }

    /// Who said line `id`: `ozen tag-menu` (src/panel.rs) decides the entries.
    fn tag_menu(&self, id: &str) -> Retained<NSMenu> {
        let mtm = self.mtm();
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Who said this?"));
        for e in cli::json(&["tag-menu", id])
            .as_array()
            .into_iter()
            .flatten()
        {
            let title = e["title"].as_str().unwrap_or("");
            let (action, object) = match e["action"].as_str() {
                Some("separator") => {
                    menu.addItem(&NSMenuItem::separatorItem(mtm));
                    continue;
                }
                Some("new") => (sel!(newPerson:), serde_json::json!(id)),
                Some("ignore") => (
                    sel!(ignoreAll:),
                    if e["arg"].is_array() {
                        e["arg"].clone()
                    } else {
                        serde_json::json!([id])
                    },
                ),
                _ => (
                    sel!(pick:),
                    serde_json::json!([id, e["arg"].as_str().unwrap_or("")]),
                ),
            };
            // SAFETY: the target is the app delegate, alive for the process.
            let mi = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    NSMenuItem::alloc(mtm),
                    &NSString::from_str(title),
                    Some(action),
                    ns_string!(""),
                )
            };
            unsafe {
                mi.setTarget(Some(self));
                mi.setRepresentedObject(Some(&NSString::from_str(&object.to_string())));
            }
            menu.addItem(&mi);
        }
        menu
    }

    /// Tag lines (what the panel shows at once, retrained in the background). Values go through the environment,
    /// never into the command string; line ids have no spaces.
    fn tag(&self, ids: Vec<String>, name: &str, command: Option<&str>) {
        for id in &ids {
            self.ivars()
                .pending
                .borrow_mut()
                .insert(id.clone(), name.to_string());
        }
        self.reload();
        let command = command
            .unwrap_or("cd \"$OZEN_DIR\" && target/release/ozen tag \"$OZEN_ID\" \"$OZEN_NAME\"")
            .to_string();
        let (dir, joined, name) = (
            cli::dir().display().to_string(),
            ids.join(" "),
            name.to_string(),
        );
        std::thread::spawn(move || {
            let _ = std::process::Command::new("/bin/zsh")
                .args(["-lc", &command])
                .env("OZEN_DIR", dir)
                .env("OZEN_ID", joined)
                .env("OZEN_NAME", name)
                .status();
            dispatch2::DispatchQueue::main().exec_async(move || {
                APP.with(|a| {
                    let app = a.get().unwrap();
                    for id in &ids {
                        app.ivars().pending.borrow_mut().remove(id);
                    }
                    app.ivars().signature.borrow_mut().clear();
                    app.reload();
                })
            });
        });
    }

    /// Clicking a line's text: correct what was said. Empty restores what ozen heard.
    fn fix_text(&self, id: &str) {
        let Some(original) = self.ivars().view.borrow().heard.get(id).cloned() else {
            return;
        };
        let fixes = std::fs::read(cli::dir().join("fixes.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let current = self
            .ivars()
            .pending_fixes
            .borrow()
            .get(id)
            .cloned()
            .or_else(|| {
                fixes
                    .as_ref()
                    .and_then(|f| f[id].as_str().map(String::from))
            })
            .unwrap_or_else(|| original.clone());
        let mtm = self.mtm();
        let alert = objc2_app_kit::NSAlert::new(mtm);
        alert.setMessageText(ns_string!("What was really said?"));
        alert.setInformativeText(&NSString::from_str(&format!("Heard: {original}\nozen learns the words you add, and applies a correction you make twice.")));
        alert.addButtonWithTitle(ns_string!("Fix"));
        alert.addButtonWithTitle(ns_string!("Cancel"));
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(420.0, 90.0));
        let field = NSTextView::initWithFrame(NSTextView::alloc(mtm), frame);
        field.setString(&NSString::from_str(&current));
        field.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        field.setRichText(false);
        if current
            .chars()
            .any(|c| ('\u{0590}'..='\u{05FF}').contains(&c))
        {
            field.setBaseWritingDirection(objc2_app_kit::NSWritingDirection::RightToLeft);
            field.setAlignment(objc2_app_kit::NSTextAlignment::Right);
        }
        let bx = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
        bx.setDocumentView(Some(&field));
        bx.setHasVerticalScroller(true);
        bx.setBorderType(objc2_app_kit::NSBorderType::BezelBorder);
        alert.setAccessoryView(Some(&bx));
        alert.window().setInitialFirstResponder(Some(&field));
        NSApplication::sharedApplication(mtm).activate();
        if alert.runModal() != 1000 {
            return;
        }
        let fixed = field.string().to_string().trim().to_string();
        self.ivars()
            .pending_fixes
            .borrow_mut()
            .insert(id.to_string(), fixed.clone());
        self.reload();
        let id = id.to_string();
        cli::run(&["fix", &id.clone(), &fixed], move |_, _, _| {
            APP.with(|a| {
                let app = a.get().unwrap();
                app.ivars().pending_fixes.borrow_mut().remove(&id);
                app.ivars().signature.borrow_mut().clear();
                app.reload();
            })
        });
    }

    /// `ozen place`: {"here": {lat, lon, age} | null, "place": {label, action} | null}. Where you are and which place
    /// that is are decided in Rust (src/places.rs, src/bin/locate.rs); this keeps the answer.
    fn apply_place(&self, out: &str) {
        let Ok(r) = serde_json::from_str::<Value>(out) else {
            return;
        };
        let iv = self.ivars();
        let (status, first) = iv.heard.get();
        iv.heard.set((status, true));
        iv.located
            .set(r["here"]["lat"].is_f64() && r["here"]["lon"].is_f64());
        iv.here
            .set(r["here"]["lat"].as_f64().zip(r["here"]["lon"].as_f64()));
        let place = r["place"]["label"].as_str().map(|l| auto::Place {
            label: l.into(),
            action: r["place"]["action"].as_str().unwrap_or("off").into(),
        });
        if place != *iv.place_now.borrow() || !first {
            *iv.place_now.borrow_mut() = place;
            self.auto_control();
            if iv
                .places_window
                .borrow()
                .as_ref()
                .is_some_and(|w| w.isVisible())
            {
                self.show_places_on_map(false);
            }
        }
        self.supply_location_if_needed();
    }

    fn auto_control(&self) {
        let iv = self.ivars();
        // Decide only once the recorder's state and the place are both known: deciding on the mode alone and then
        // on the place a moment later would count as a flip and stop a recording that a relaunch must keep.
        if iv.heard.get() != (true, true) {
            return;
        }
        let mic = cli::json(&["mic"]);
        let place = iv.place_now.borrow().clone();
        let places_set = places::located(&places::load());
        let state = iv.state.borrow().clone();
        let mode = mode();
        let facts = auto::Facts {
            now: now(),
            since_launch: iv.launched.get().map_or(0.0, |t| t.elapsed().as_secs_f64()),
            mic_app: mic["app"].as_str(),
            place: place.as_ref(),
            located: iv.located.get(),
            places_set,
            mode: &mode,
            state: &state,
        };
        let act = auto::decide(&facts, &mut iv.auto.borrow_mut());
        self.show_state(&state);
        match act {
            auto::Act::Start => self.start(),
            auto::Act::Stop => self.stop(),
            auto::Act::None => {}
        }
    }

    /// Advanced settings: record and process separately.
    fn advanced(&self) {
        let mtm = self.mtm();
        if self.ivars().advanced_window.borrow().is_none() {
            use objc2_app_kit::{NSBackingStoreType, NSWindow, NSWindowStyleMask};
            // SAFETY: a plain titled window we keep (not released on close).
            let w = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(460.0, 170.0)),
                    NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            w.setTitle(ns_string!("Ozen Advanced Settings"));
            unsafe { w.setReleasedWhenClosed(false) };
            let header = NSTextField::labelWithString(ns_string!("Recording and processing"), mtm);
            header.setFont(Some(&NSFont::boldSystemFontOfSize(12.0)));
            // SAFETY: the target is the app delegate, alive for the process.
            let bx = unsafe {
                NSButton::checkboxWithTitle_target_action(
                    ns_string!("Split recording and processing"),
                    Some(self),
                    Some(sel!(splitChanged:)),
                    mtm,
                )
            };
            bx.setState(if split() {
                objc2_app_kit::NSControlStateValueOn
            } else {
                objc2_app_kit::NSControlStateValueOff
            });
            let note = NSTextField::wrappingLabelWithString(
                &NSString::from_str(concat!(
                    "Record then only records: nothing is transcribed while it runs, and the audio ",
                    "waits in the chunks folder (it takes disk space until processed). Process transcribes the waiting audio, ",
                    "with or without a recording going on, and stops once it's done. Off: Start records and transcribes together."
                )),
                mtm,
            );
            note.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            note.setTextColor(Some(&NSColor::secondaryLabelColor()));
            note.setPreferredMaxLayoutWidth(420.0);
            let views: [&NSView; 3] = [&header, &bx, &note];
            let stack = NSStackView::stackViewWithViews(
                &objc2_foundation::NSArray::from_slice(&views),
                mtm,
            );
            stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            stack.setAlignment(objc2_app_kit::NSLayoutAttribute::Leading);
            stack.setSpacing(8.0);
            stack.setEdgeInsets(NSEdgeInsets {
                top: 16.0,
                left: 16.0,
                bottom: 16.0,
                right: 16.0,
            });
            w.setContentView(Some(&stack));
            w.center();
            *self.ivars().advanced_window.borrow_mut() = Some(w);
        }
        NSApplication::sharedApplication(mtm).activate();
        if let Some(w) = self.ivars().advanced_window.borrow().as_ref() {
            w.makeKeyAndOrderFront(None);
        }
    }

    fn view_switched(&self) {
        let seg = self.ivars().view_control.get().unwrap().selectedSegment();
        objc2_foundation::NSUserDefaults::standardUserDefaults()
            .setInteger_forKey(seg, ns_string!("view"));
        self.apply_view();
    }

    fn apply_view(&self) {
        let iv = self.ivars();
        let seg = iv.view_control.get().unwrap().selectedSegment();
        let (show_timeline, show_meetings) = (seg == 1, seg == 2);
        iv.scroll
            .get()
            .unwrap()
            .setHidden(show_timeline || show_meetings);
        iv.timeline_scroll.get().unwrap().setHidden(!show_timeline);
        iv.meetings_scroll.get().unwrap().setHidden(!show_meetings);
        let b = iv.view_buttons.get().unwrap();
        b.zoom_in.setHidden(!show_timeline);
        b.zoom_out.setHidden(!show_timeline);
        b.gather.setHidden(!show_meetings);
        b.kev.setHidden(!show_meetings);
        if show_timeline {
            self.scroll_timeline_to_end();
        }
        if show_meetings {
            self.load_meetings();
        }
    }

    fn scroll_timeline_to_end(&self) {
        let iv = self.ivars();
        let (tl, scroll) = (
            iv.timeline.get().unwrap(),
            iv.timeline_scroll.get().unwrap(),
        );
        let clip = scroll.contentView();
        let x = (tl.bounds().size.width - clip.bounds().size.width).max(0.0);
        clip.scrollToPoint(NSPoint::new(x, 0.0));
        scroll.reflectScrolledClipView(&clip);
    }

    /// Timeline bar clicked: show that line in the transcript.
    fn jump(&self, id: &str) {
        self.ivars()
            .view_control
            .get()
            .unwrap()
            .setSelectedSegment(0);
        self.view_switched();
        if let Some(&(loc, len)) = self.ivars().view.borrow().headers.get(id) {
            let r = objc2_foundation::NSRange::new(loc, len);
            let text = self.text();
            text.scrollRangeToVisible(r);
            text.showFindIndicatorForRange(r);
        }
    }

    fn load_meetings(&self) {
        cli::run(&["meetings"], |out, _, _| {
            APP.with(|a| {
                let app = a.get().unwrap();
                let iv = app.ivars();
                let table = iv.meetings_table.get().unwrap();
                let picked: std::collections::HashSet<String> = {
                    let m = iv.meetings.borrow();
                    let rows = table.selectedRowIndexes();
                    (0..m.len())
                        .filter(|&i| rows.containsIndex(i))
                        .map(|i| m[i][0].clone())
                        .collect()
                };
                let rows: Vec<Vec<String>> = out
                    .lines()
                    .map(|l| l.split('\t').map(String::from).collect::<Vec<_>>())
                    .filter(|r| r.len() == 5)
                    .collect();
                let keep = objc2_foundation::NSMutableIndexSet::new();
                for (i, r) in rows.iter().enumerate() {
                    if picked.contains(&r[0]) {
                        keep.addIndex(i);
                    }
                }
                *iv.meetings.borrow_mut() = rows;
                table.reloadData();
                table.selectRowIndexes_byExtendingSelection(&keep, false);
            })
        });
    }

    /// The transcript again with tags and fixes pending (as between a click and `ozen tag`/`fix` finishing), and
    /// the tag menu of the first line: the render check's last part.
    fn dump_pending(&self) -> String {
        let iv = self.ivars();
        iv.view_control.get().unwrap().setSelectedSegment(0);
        self.apply_view();
        let mut ids = iv.view.borrow().ids.clone();
        ids.sort();
        ids.dedup();
        if ids.len() > 3 {
            iv.pending
                .borrow_mut()
                .insert(ids[0].clone(), "Dana Levi".into());
            iv.pending
                .borrow_mut()
                .insert(ids[1].clone(), String::new());
            iv.pending_fixes
                .borrow_mut()
                .insert(ids[2].clone(), "fixed text".into());
            iv.pending_fixes
                .borrow_mut()
                .insert(ids[3].clone(), String::new());
        }
        iv.signature.borrow_mut().clear();
        self.reload();
        let mut out = vec![String::new(), String::new(), "== pending".to_string()];
        out.extend(transcript::dump_runs(&self.text()));
        if let Some(id) = ids.first() {
            let menu = self.tag_menu(id);
            let items: Vec<String> = menu
                .itemArray()
                .iter()
                .map(|mi| {
                    let action = mi
                        .action()
                        .map_or(String::new(), |a| a.name().to_string_lossy().into_owned());
                    format!("{}>{action}", mi.title())
                })
                .collect();
            out.push(format!("TAGMENU {id}: {}", items.join(" | ")));
        }
        out.join("\n")
    }

    /// A window's content as the render check prints it: every text shown, top to bottom, and a PNG.
    fn dump_window(&self, w: Option<&Retained<objc2_app_kit::NSWindow>>, png: &str) -> String {
        let Some(view) = w.and_then(|w| w.contentView()) else {
            return String::new();
        };
        view.layoutSubtreeIfNeeded();
        let mut out = String::from("\n\n== window");
        fn walk(v: &NSView, out: &mut String) {
            if let Some(t) = v.downcast_ref::<NSTextField>() {
                *out += &format!("\nLABEL {}", t.stringValue());
            } else if let Some(b) = v.downcast_ref::<NSButton>() {
                *out += &format!("\nBUTTON {}", b.title());
            }
            for sub in v.subviews().iter() {
                walk(&sub, out);
            }
        }
        walk(&view, &mut out);
        if let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(view.bounds()) {
            view.cacheDisplayInRect_toBitmapImageRep(view.bounds(), &rep);
            // SAFETY: PNG encoding of our own bitmap.
            if let Some(d) = unsafe {
                rep.representationUsingType_properties(
                    objc2_app_kit::NSBitmapImageFileType::PNG,
                    &objc2_foundation::NSDictionary::new(),
                )
            } {
                let _ = std::fs::write(png, d.to_vec());
            }
        }
        out
    }

    /// The render check's Places part: open the window, then the same scripted edits as the Swift harness (a map
    /// click while picking, a pin drag, Add place, a typed latitude, an invalid radius), dumping the window and
    /// saving places.json after each. Writes the dump and quits.
    fn dump_places(&self, file: String, out: String) {
        self.show_places();
        let js = |app: &App, js: &str| unsafe {
            app.places_map()
                .evaluateJavaScript_completionHandler(&NSString::from_str(js), None)
        };
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
        };
        later(4.0, move |app| {
            let mut out = out;
            snap(app, 1, &mut out);
            app.pick_on_map(0);
            js(
                app,
                "window.webkit.messageHandlers.ozen.postMessage({type: 'click', lat: 32.1, lon: 34.9})",
            );
            later(1.5, move |app| {
                snap(app, 2, &mut out);
                js(
                    app,
                    "window.webkit.messageHandlers.ozen.postMessage({type: 'move', index: 1, lat: 31.5, lon: 35.25})",
                );
                later(1.5, move |app| {
                    snap(app, 3, &mut out);
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
                            std::fs::write(&file2, out).expect("write dump");
                            std::process::exit(0);
                        });
                    });
                });
            });
        });
    }

    /// The Meetings table's cells, row by row, as the render check prints them.
    fn dump_meetings(&self) -> String {
        let table = self.ivars().meetings_table.get().unwrap();
        let cols = table.tableColumns();
        let mut out = String::new();
        let meetings = self.ivars().meetings.borrow();
        out += &format!("\nROWS {}", table.numberOfRows());
        for row in 0..meetings.len() {
            let cells: Vec<String> = cols
                .iter()
                .map(|c| meeting_cell(&meetings, &c.identifier().to_string(), row))
                .collect();
            out += &format!("\nMEETING {}", cells.join("\t"));
        }
        out
    }

    fn start(&self) {
        let paused = *self.ivars().state.borrow() == "paused";
        self.control(
            if paused {
                "resume"
            } else if split() {
                "record"
            } else {
                "start"
            },
            "recording",
        );
    }

    fn stop(&self) {
        self.control("stop", "stopping");
    }

    fn control(&self, cmd: &str, optimistic: &str) {
        self.show_state(optimistic);
        cli::run(&[cmd], |_, _, _| later(1.0, App::refresh_state));
    }

    /// What the panel drew, in the Swift render check's format.
    fn dump(&self, file: &str) -> String {
        self.ivars().signature.borrow_mut().clear();
        self.reload();
        // the whole panel as shown, the transcript laid out in full (TextKit 2 lays out lazily)
        if let Some(tlm) = self.text().textLayoutManager() {
            tlm.ensureLayoutForRange(&objc2_app_kit::NSTextSelectionDataSource::documentRange(
                &*tlm,
            ));
            // SAFETY: scrolls the app's own text view.
            unsafe { self.text().scrollToEndOfDocument(None) };
        }
        if let Some(view) = self
            .ivars()
            .popover
            .get()
            .unwrap()
            .contentViewController()
            .map(|c| c.view())
        {
            view.layoutSubtreeIfNeeded();
            if let Some(rep) = view.bitmapImageRepForCachingDisplayInRect(view.bounds()) {
                view.cacheDisplayInRect_toBitmapImageRep(view.bounds(), &rep);
                // SAFETY: PNG encoding of our own bitmap.
                if let Some(d) = unsafe {
                    rep.representationUsingType_properties(
                        objc2_app_kit::NSBitmapImageFileType::PNG,
                        &objc2_foundation::NSDictionary::new(),
                    )
                } {
                    let _ = std::fs::write(format!("{file}.panel.png"), d.to_vec());
                }
            }
        }
        let mut out = vec!["== plain".to_string()];
        out.extend(transcript::dump_runs(&self.text()));
        let iv = self.ivars();
        let view = iv.view.borrow();
        let headers: BTreeMap<&String, &(usize, usize)> = view.headers.iter().collect();
        out.push(format!(
            "HEADERS {}",
            headers
                .iter()
                .map(|(k, (l, n))| format!("{k}={l},{n}"))
                .collect::<Vec<_>>()
                .join(" ")
        ));
        out.push(format!("FOOTER {}", iv.footer.get().unwrap().stringValue()));
        out.push(format!(
            "REVIEWQ {}",
            view.review
                .iter()
                .map(|(id, u)| format!("{id}@{}", transcript::swift_double(*u)))
                .collect::<Vec<_>>()
                .join(",")
        ));
        out.push(format!("REVIEW {}", iv.review.borrow().join(",")));
        out.push(format!(
            "SHOWN {}",
            view.ids.iter().collect::<BTreeSet<_>>().len()
        ));
        // the Timeline view as its tab shows it
        iv.view_control.get().unwrap().setSelectedSegment(1);
        self.apply_view();
        out.extend(
            iv.timeline
                .get()
                .unwrap()
                .dump(&std::path::PathBuf::from(format!("{file}.png"))),
        );
        for g in iv.timeline.get().unwrap().segments() {
            let f = transcript::swift_double;
            out.push(format!(
                "SEG {} {} {} {} {} {}",
                g.id,
                f(g.t),
                f(g.d),
                g.speaker,
                g.unsure,
                g.text
            ));
        }
        out.join("\n")
    }
}

fn main() {
    let mtm = mtm();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut rest = args.iter().filter(|a| !a.starts_with("--"));
    let dir = match args.iter().position(|a| a == "--dump") {
        Some(i) => args
            .iter()
            .enumerate()
            .find(|(j, a)| !a.starts_with("--") && *j != i + 1)
            .map(|(_, a)| PathBuf::from(a)),
        None => rest.next().map(PathBuf::from),
    };
    // Launched as Ozen.app (Finder/Spotlight) there are no args: use the standard checkout, installing the copy the
    // app carries when it's newer.
    let launched_as_app = dir.is_none();
    cli::set_dir(
        dir.unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("ozen")
        }),
    );
    if launched_as_app && let Some(res) = objc2_foundation::NSBundle::mainBundle().resourcePath() {
        install::bundled(&PathBuf::from(res.to_string()).join("ozen"), cli::dir());
    }
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory); // menu bar only, no Dock icon
    let delegate = App::new(mtm);
    APP.with(|a| {
        let _ = a.set(delegate.clone());
    });
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
