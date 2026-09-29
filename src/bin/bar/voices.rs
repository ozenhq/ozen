//! The Voices window: everyone ozen has heard (`ozen voices`): rename or merge people, name this run's unnamed
//! speakers, ignore voices that aren't in the meeting, forget one. Changes retrain the voiceprints.
use crate::{App, cli, confirm};
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSButton, NSColor, NSComboBox, NSControlSize, NSFont,
    NSLayoutAttribute, NSLineBreakMode, NSScrollView, NSStackView, NSTextField,
    NSUserInterfaceItemIdentification, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSEdgeInsets, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, ns_string,
};
use serde_json::Value;

define_class!(
    /// Scroll content that starts at the top.
    // SAFETY: NSView subclass that only flips its coordinates; no Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    pub struct FlippedView;

    unsafe impl NSObjectProtocol for FlippedView {}

    impl FlippedView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
    }
);

impl FlippedView {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        // SAFETY: NSView's designated initializer.
        unsafe { msg_send![Self::alloc(mtm), initWithFrame: NSRect::ZERO] }
    }
}

fn isolated(s: &str) -> String {
    format!("\u{2068}{s}\u{2069}") // a Hebrew name would otherwise reorder the whole row ("lines 1 · נתן")
}

impl App {
    pub fn show_voices(&self) {
        let mtm = self.mtm();
        let iv = self.ivars();
        if iv.voices_window.borrow().is_none() {
            // SAFETY: a plain window we keep (not released on close).
            let w = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(560.0, 520.0)),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            w.setTitle(ns_string!("Ozen Voices"));
            unsafe { w.setReleasedWhenClosed(false) };
            let stack = iv.voices_stack.get_or_init(|| NSStackView::new(mtm));
            stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
            stack.setAlignment(NSLayoutAttribute::Leading);
            stack.setSpacing(10.0);
            stack.setEdgeInsets(NSEdgeInsets {
                top: 12.0,
                left: 12.0,
                bottom: 12.0,
                right: 12.0,
            });
            let flipped = FlippedView::new(mtm);
            flipped.addSubview(stack);
            stack.setTranslatesAutoresizingMaskIntoConstraints(false);
            for (a, b) in [
                (stack.topAnchor(), flipped.topAnchor()),
                (stack.bottomAnchor(), flipped.bottomAnchor()),
            ] {
                a.constraintEqualToAnchor(&b).setActive(true);
            }
            for (a, b) in [
                (stack.leadingAnchor(), flipped.leadingAnchor()),
                (stack.trailingAnchor(), flipped.trailingAnchor()),
            ] {
                a.constraintEqualToAnchor(&b).setActive(true);
            }
            let scroll = NSScrollView::new(mtm);
            scroll.setHasVerticalScroller(true);
            scroll.setDocumentView(Some(&flipped));
            flipped.setTranslatesAutoresizingMaskIntoConstraints(false);
            flipped
                .widthAnchor()
                .constraintEqualToAnchor(&scroll.contentView().widthAnchor())
                .setActive(true);
            w.setContentView(Some(&scroll));
            w.center();
            *iv.voices_window.borrow_mut() = Some(w);
        }
        self.load_voices(None);
        NSApplication::sharedApplication(mtm).activate();
        if let Some(w) = iv.voices_window.borrow().as_ref() {
            w.makeKeyAndOrderFront(None);
        }
    }

    pub fn load_voices(&self, note: Option<String>) {
        cli::run(&["voices"], move |out, err, code| {
            crate::APP.with(|a| {
                let app = a.get().unwrap();
                *app.ivars().voices.borrow_mut() = serde_json::from_str::<Value>(&out)
                    .ok()
                    .and_then(|v| v.as_array().cloned())
                    .unwrap_or_default();
                app.build_voices(if code == 0 {
                    note
                } else {
                    Some(format!("Couldn't list voices: {err}"))
                });
            })
        });
    }

    pub fn build_voices(&self, note: Option<String>) {
        let mtm = self.mtm();
        let iv = self.ivars();
        let Some(stack) = iv.voices_stack.get() else {
            return;
        };
        for v in stack.arrangedSubviews().iter() {
            v.removeFromSuperview();
        }
        let intro = NSTextField::wrappingLabelWithString(
            &NSString::from_str(&note.unwrap_or_else(|| {
                "Rename or merge people, name this run's unnamed speakers, and ignore voices that aren't in the meeting. Changes retrain the voiceprints.".into()
            })),
            mtm,
        );
        intro.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        intro.setTextColor(Some(&NSColor::secondaryLabelColor()));
        stack.addArrangedSubview(&intro);
        let voices = iv.voices.borrow();
        if voices.is_empty() {
            stack.addArrangedSubview(&NSTextField::labelWithString(
                ns_string!("No voices yet."),
                mtm,
            ));
        }
        for v in voices.iter() {
            let name = v["name"].as_str().unwrap_or("?");
            let kind = v["kind"].as_str().unwrap_or("");
            let n = v["lines"].as_i64().unwrap_or(0);
            let hint = match kind {
                "unnamed" => " · unnamed, this run",
                "ignored" => " · not transcribed",
                _ => "",
            };
            let title = NSTextField::labelWithString(
                &NSString::from_str(&format!(
                    "{} · {n} line{}{hint}",
                    isolated(name),
                    if n == 1 { "" } else { "s" }
                )),
                mtm,
            );
            title.setFont(Some(&NSFont::boldSystemFontOfSize(13.0)));
            let actions: &[(&str, objc2::runtime::Sel)] = match kind {
                "person" => &[
                    ("Rename…", sel!(renameVoice:)),
                    ("Ignore…", sel!(ignoreVoice:)),
                    ("Forget…", sel!(forgetVoice:)),
                ],
                "unnamed" => &[
                    ("Name…", sel!(renameVoice:)),
                    ("Ignore", sel!(ignoreVoice:)),
                ],
                _ => &[("Stop ignoring…", sel!(forgetVoice:))],
            };
            let spacer = NSView::new(mtm);
            let mut row: Vec<Retained<NSView>> =
                vec![Retained::into_super(Retained::into_super(title)), spacer];
            for (t, sel) in actions {
                // SAFETY: the target is the app delegate, alive for the process.
                let b = unsafe {
                    NSButton::buttonWithTitle_target_action(
                        &NSString::from_str(t),
                        Some(self),
                        Some(*sel),
                        mtm,
                    )
                };
                b.setControlSize(NSControlSize::Small);
                b.setIdentifier(Some(&NSString::from_str(name)));
                row.push(Retained::into_super(Retained::into_super(b)));
            }
            let refs: Vec<&NSView> = row.iter().map(|v| &**v).collect();
            stack.addArrangedSubview(&NSStackView::stackViewWithViews(
                &objc2_foundation::NSArray::from_slice(&refs),
                mtm,
            ));
            for line in v["recent"].as_array().into_iter().flatten() {
                // Click a line to see it in the transcript.
                let text = format!("  “{}”", isolated(line["text"].as_str().unwrap_or("")));
                // SAFETY: the target is the app delegate, alive for the process.
                let b = unsafe {
                    NSButton::buttonWithTitle_target_action(
                        &NSString::from_str(&text),
                        Some(self),
                        Some(sel!(showVoiceLine:)),
                        mtm,
                    )
                };
                b.setBordered(false);
                b.setFont(Some(&NSFont::systemFontOfSize(11.0)));
                b.setContentTintColor(Some(&NSColor::secondaryLabelColor()));
                b.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
                // truncate, don't widen the window
                b.setContentCompressionResistancePriority_forOrientation(
                    250.0,
                    objc2_app_kit::NSLayoutConstraintOrientation::Horizontal,
                );
                b.setIdentifier(Some(&NSString::from_str(line["id"].as_str().unwrap_or(""))));
                stack.addArrangedSubview(&b);
                b.widthAnchor()
                    .constraintLessThanOrEqualToAnchor_constant(&stack.widthAnchor(), -24.0)
                    .setActive(true);
            }
        }
        if let Some(w) = iv.voices_window.borrow().as_ref()
            && let Some(c) = w.contentView()
        {
            c.setNeedsLayout(true);
        }
    }

    fn voice(&self, name: &str) -> Option<Value> {
        self.ivars()
            .voices
            .borrow()
            .iter()
            .find(|v| v["name"].as_str() == Some(name))
            .cloned()
    }

    /// Runs a voices command, then refreshes this window and the transcript.
    fn change_voices(&self, args: Vec<String>) {
        self.build_voices(Some("Retraining…".into()));
        if let Some(stack) = self.ivars().voices_stack.get() {
            for row in stack.arrangedSubviews().iter() {
                if let Ok(row) = row.downcast::<NSStackView>() {
                    for b in row.views().iter() {
                        if let Ok(b) = b.downcast::<NSButton>() {
                            b.setEnabled(false);
                        }
                    }
                }
            }
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        cli::run(&refs, |_, err, code| {
            crate::APP.with(|a| {
                let app = a.get().unwrap();
                app.ivars().signature.borrow_mut().clear();
                app.reload();
                app.load_voices(if code == 0 {
                    None
                } else {
                    Some(format!("That didn't work: {}", err.trim()))
                });
            })
        });
    }

    pub fn rename_voice(&self, name: &str) {
        let Some(v) = self.voice(name) else { return };
        let mtm = self.mtm();
        let unnamed = v["kind"] == "unnamed";
        let alert = objc2_app_kit::NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(&if unnamed {
            format!("Who is {name}?")
        } else {
            format!("Rename {name}")
        }));
        alert.setInformativeText(ns_string!("Use an existing name to merge the two voices."));
        alert.addButtonWithTitle(if unnamed {
            ns_string!("Name")
        } else {
            ns_string!("Rename")
        });
        alert.addButtonWithTitle(ns_string!("Cancel"));
        let field = NSComboBox::initWithFrame(
            NSComboBox::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(260.0, 26.0)),
        );
        let people: Vec<String> = self
            .ivars()
            .voices
            .borrow()
            .iter()
            .filter(|p| p["kind"] == "person")
            .filter_map(|p| p["name"].as_str().map(String::from))
            .filter(|p| p != name)
            .collect();
        let items: Vec<Retained<AnyObject>> = people
            .iter()
            .map(|p| Retained::into_super(Retained::into_super(NSString::from_str(p))))
            .collect();
        // SAFETY: string object values for the combo box.
        unsafe {
            field.addItemsWithObjectValues(&objc2_foundation::NSArray::from_retained_slice(&items))
        };
        field.setPlaceholderString(Some(ns_string!("Full name")));
        alert.setAccessoryView(Some(&field));
        alert.window().setInitialFirstResponder(Some(&field));
        if alert.runModal() != 1000 {
            return;
        }
        let to = field.stringValue().to_string().trim().to_string();
        if to.is_empty() || to == name || is_ignored(&to) {
            return;
        }
        if people.contains(&to)
            && !confirm(
                &format!("Merge {name} into {to}?"),
                &format!(
                    "All of {name}'s lines become {to}'s, and one voiceprint is built from both."
                ),
                "Merge",
            )
        {
            return;
        }
        let args = if unnamed {
            let mut a = vec!["name".to_string(), to];
            a.extend(
                v["ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(String::from)),
            );
            a
        } else {
            vec!["rename".into(), name.into(), to]
        };
        self.change_voices(args);
    }

    pub fn ignore_voice(&self, name: &str) {
        let Some(v) = self.voice(name) else { return };
        if v["kind"] == "unnamed" {
            let mut a = vec!["ignore".to_string()];
            a.extend(
                v["ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|x| x.as_str().map(String::from)),
            );
            return self.change_voices(a);
        }
        if confirm(
            &format!("Ignore {name}?"),
            "Their lines stop being a person and their voice stops being transcribed.",
            "Ignore",
        ) {
            self.change_voices(vec!["rename".into(), name.into(), "Ignored".into()]);
        }
    }

    pub fn forget_voice(&self, name: &str) {
        if self.voice(name).is_none() {
            return;
        }
        let ignored = is_ignored(name);
        let (message, info, action) = if ignored {
            (
                format!("Stop ignoring {name}?"),
                "Their speech is transcribed again from now on.",
                "Stop ignoring",
            )
        } else {
            (
                format!("Forget {name} on this Mac?"),
                "Clears every tag of {name} here. Other Macs keep theirs.",
                "Forget",
            )
        };
        if confirm(&message, &info.replace("{name}", name), action) {
            self.change_voices(vec!["forget".into(), name.into()]);
        }
    }

    pub fn show_voice_line(&self, id: &str) {
        if !self.ivars().popover.get().unwrap().isShown() {
            self.toggle();
        }
        self.jump(id);
    }
}

/// "Ignored" or one of its numbered voices ("Ignored 2"), as src/ignore.rs is_ignored.
pub fn is_ignored(name: &str) -> bool {
    name == "Ignored"
        || name
            .strip_prefix("Ignored ")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}
