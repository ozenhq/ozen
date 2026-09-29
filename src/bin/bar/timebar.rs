//! The Timebar window: every recorded chunk on a local-time bar, done or still waiting, and the transcriber's pace.
//! The page is chunks.html; `ozen timebar` (src/timebar.rs) supplies its data every 5s while the window is open.
use crate::{App, cli};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{DefinedClass, MainThreadOnly, sel};
use objc2_app_kit::{NSApplication, NSBackingStoreType, NSWindow, NSWindowStyleMask};
use objc2_foundation::{NSBundle, NSPoint, NSRect, NSSize, NSString, NSTimer, NSURL, ns_string};
use objc2_web_kit::WKWebView;

/// A page shipped in Ozen.app (by `ozen app`), else the checkout's copy (a checkout run: `Ozen [dir]`).
pub fn page(name: &str) -> Retained<NSURL> {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
    NSBundle::mainBundle()
        .URLForResource_withExtension(
            Some(&NSString::from_str(stem)),
            Some(&NSString::from_str(ext)),
        )
        .unwrap_or_else(|| {
            NSURL::fileURLWithPath(&NSString::from_str(
                &cli::dir().join(name).display().to_string(),
            ))
        })
}

pub fn load(view: &WKWebView, name: &str) {
    let url = page(name);
    if let Some(dir) = url.URLByDeletingLastPathComponent() {
        // SAFETY: loading a local file with read access to its folder.
        unsafe { view.loadFileURL_allowingReadAccessToURL(&url, &dir) };
    }
}

impl App {
    pub fn timebar_view(&self) -> &Retained<WKWebView> {
        self.ivars().timebar_view.get_or_init(|| {
            // SAFETY: a default web view on the main thread.
            let v = unsafe { WKWebView::new(self.mtm()) };
            // SAFETY: the delegate is the app delegate, alive for the process.
            unsafe { v.setNavigationDelegate(Some(ProtocolObject::from_ref(self))) };
            v
        })
    }

    pub fn show_timebar(&self) {
        let mtm = self.mtm();
        let iv = self.ivars();
        if iv.timebar_window.borrow().is_none() {
            // SAFETY: a plain window we keep (not released on close).
            let w = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(900.0, 420.0)),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            w.setTitle(ns_string!("Ozen Timebar"));
            unsafe { w.setReleasedWhenClosed(false) };
            let view = self.timebar_view();
            w.setContentView(Some(view));
            load(view, "chunks.html");
            w.center();
            *iv.timebar_window.borrow_mut() = Some(w);
        }
        if let Some(w) = iv.timebar_window.borrow().as_ref() {
            w.makeKeyAndOrderFront(None);
        }
        NSApplication::sharedApplication(mtm).activate();
        self.refresh_timebar();
        if let Some(t) = iv.timebar_timer.borrow_mut().take() {
            t.invalidate();
        }
        // SAFETY: the target is the app delegate, alive for the process.
        let t = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                5.0,
                self,
                sel!(timebarTick:),
                None,
                true,
            )
        };
        *iv.timebar_timer.borrow_mut() = Some(t);
    }

    pub fn timebar_tick(&self) {
        let iv = self.ivars();
        let open = iv
            .timebar_window
            .borrow()
            .as_ref()
            .is_some_and(|w| w.isVisible());
        if !open {
            // closed: stop polling
            if let Some(t) = iv.timebar_timer.borrow_mut().take() {
                t.invalidate();
            }
            return;
        }
        self.refresh_timebar();
    }

    pub fn refresh_timebar(&self) {
        cli::run(&["timebar"], |out, _, _| {
            crate::APP.with(|a| {
                let view = a.get().unwrap().timebar_view();
                // before the page loads this is a no-op
                // SAFETY: calling the page's own show() with ozen's JSON.
                unsafe {
                    view.evaluateJavaScript_completionHandler(
                        &NSString::from_str(&format!("show({out})")),
                        None,
                    )
                };
            })
        });
    }
}
