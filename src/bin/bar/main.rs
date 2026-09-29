//! Ozen's menu bar app in Rust, replacing menubar.swift piece by piece. Not installed yet: `ozen app` still builds
//! the Swift app until this one does everything it does.
//!
//! The ear icon: left-click shows the live transcript with Start/Pause/Stop controls, right-click the same controls.
//! Everything it shows is decided by the ozen CLI (src/panel.rs and friends); this draws it and forwards clicks.
//!
//!     bar [DIR] [--open] [--dump FILE]
//!
//! DIR is the ozen checkout (default ~/ozen). --open shows the panel at launch. --dump FILE shows the panel, writes
//! what it drew (every text run with its attributes, the footer, Review queue and timeline bars) to FILE and quits:
//! the render check against the Swift app.
mod cli;
mod transcript;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSButton, NSColor,
    NSControlSize, NSEventMask, NSEventType, NSFont, NSImage, NSMenu, NSMenuItem, NSPopover,
    NSPopoverBehavior, NSScrollView, NSStackView, NSStandardKeyBindingResponding, NSStatusBar,
    NSStatusItem, NSTextField, NSTextView, NSUserInterfaceLayoutOrientation, NSView,
    NSViewController,
};
use objc2_foundation::{
    MainThreadMarker, NSEdgeInsets, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSRectEdge, NSSize, NSString, NSTimer, ns_string,
};
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
    state: RefCell<String>, // `ozen status`: recording | paused | stopping | processing | stopped
    problems: RefCell<Vec<String>>, // `ozen health`
    view: RefCell<transcript::Shown>, // the last transcript drawn
    review: RefCell<Vec<String>>, // the Review queue minus lines too old to remember
    queued: Cell<i64>,      // chunks waiting to be transcribed, from `ozen controls`
    controls_asked: Cell<u64>, // only the newest `ozen controls` answer is drawn
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
            let paused = *self.ivars().state.borrow() == "paused";
            self.control(if paused { "resume" } else if split() { "record" } else { "start" }, "recording");
        }

        #[unsafe(method(pauseCapture:))]
        fn pause_capture(&self, _s: Option<&AnyObject>) {
            self.control("pause", "paused");
        }

        #[unsafe(method(stopCapture:))]
        fn stop_capture(&self, _s: Option<&AnyObject>) {
            self.control("stop", "stopping");
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
            let range = first.and_then(|id| self.ivars().view.borrow().headers.get(&id).copied());
            if let Some((loc, len)) = range {
                let text = self.text();
                let r = objc2_foundation::NSRange::new(loc, len);
                text.scrollRangeToVisible(r);
                text.showFindIndicatorForRange(r);
                // the tag menu for that line comes with the link-click port
            }
        }
    }
);

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
        let row: [&NSView; 7] = [
            &status,
            &spacer,
            &buttons.review,
            &buttons.start,
            &buttons.pause,
            &buttons.stop,
            &buttons.process,
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
        let parts: [&NSView; 4] = [&controls, &warning_row, &scroll, &footer];
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
        let _ = iv.item.set(item);
        let _ = iv.popover.set(popover);
        let _ = iv.scroll.set(scroll);
        let _ = iv.footer.set(footer);
        let _ = iv.status.set(status);
        let _ = iv.warning.set(warning);
        let _ = iv.buttons.set(buttons);
        *iv.state.borrow_mut() = "stopped".into();
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
                let out = app.dump();
                std::fs::write(&file, out).expect("write dump");
                std::process::exit(0);
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
        add("Quit bar, keep recording", Some(sel!(terminate:)), "");
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
            .join("|");
        if *iv.signature.borrow() == sig {
            return;
        }
        *iv.signature.borrow_mut() = sig;
        let scroll = iv.scroll.get().unwrap();
        let at_bottom = scroll
            .verticalScroller()
            .is_none_or(|s| s.floatValue() > 0.98);
        let view = cli::json(&["transcript", "{}"]);
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
        cli::run(&["status"], |out, _, _| {
            APP.with(|a| a.get().unwrap().show_state(&out))
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
        let line = if s == "recording" {
            format!(
                "● Recording{}",
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
                _ => "stopped".to_string(), // meetings and places come with the auto-record port
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

    fn control(&self, cmd: &str, optimistic: &str) {
        self.show_state(optimistic);
        cli::run(&[cmd], |_, _, _| later(1.0, App::refresh_state));
    }

    /// What the panel drew, in the Swift render check's format.
    fn dump(&self) -> String {
        self.ivars().signature.borrow_mut().clear();
        self.reload();
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
        out.extend(view.segments.iter().cloned());
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
    cli::set_dir(
        dir.unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join("ozen")
        }),
    );
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory); // menu bar only, no Dock icon
    let delegate = App::new(mtm);
    APP.with(|a| {
        let _ = a.set(delegate.clone());
    });
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
