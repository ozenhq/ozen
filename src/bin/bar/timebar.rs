//! The Timebar window: every recorded 15s chunk on a local-time axis, colored by whether the transcriber has done
//! it, with how far behind it ran and how fast it's going. Scroll or drag to move through history, pinch or
//! ⌘-scroll to zoom, Now to follow live again. `ozen timebar` (src/timebar.rs) supplies the data every 5s while the
//! window is open: {now, chunks: [{t, tag, state, sec?, done?, took?, lines?, error?}]}, state done | waiting |
//! error | old (done before the transcriber logged when).
//!
//! `scene` works out what to draw as plain shapes and text (testable, and what the render check prints);
//! `TimebarView` paints it and handles the mouse.
use crate::{App, cli};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBackingStoreType, NSBezierPath, NSButton, NSColor, NSCompositingOperation,
    NSControlSize, NSCursor, NSEvent, NSEventModifierFlags, NSFont, NSFontAttributeName,
    NSFontWeightRegular, NSFontWeightSemibold, NSForegroundColorAttributeName,
    NSRectFillUsingOperation, NSStringDrawing, NSStringDrawingOptions,
    NSStringNSExtendedStringDrawing, NSTrackingArea, NSTrackingAreaOptions, NSView, NSWindow,
    NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedStringKey, NSDate, NSDateFormatter, NSDictionary,
    NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSTimer, ns_string,
};
use serde::Deserialize;
use std::cell::{Cell, OnceCell, RefCell};

const LANES: [(&str, &str); 3] = [("call", "call"), ("mic", "room"), ("local", "computer")];
const GUTTER: f64 = 64.0;
const CHUNK: f64 = 15.0;
const LANES_H: f64 = 96.0;
const PACE_H: f64 = 150.0;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Chunk {
    pub t: f64,
    pub tag: String,
    pub state: String,
    pub sec: Option<f64>,
    pub done: Option<f64>,
    pub took: Option<f64>,
    pub lines: Option<i64>,
    pub error: Option<String>,
}

impl Chunk {
    fn len(&self) -> f64 {
        self.sec.filter(|&s| s != 0.0).unwrap_or(CHUNK)
    }
    /// How long after recording it was done.
    fn behind(&self) -> Option<f64> {
        self.done.map(|d| d - (self.t + self.len()))
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Data {
    pub now: f64,
    pub chunks: Vec<Chunk>,
}

/// What's in view: `span` seconds ending at `end`, or at now while following live.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Nav {
    pub span: f64,
    pub end: Option<f64>,
}

impl Default for Nav {
    fn default() -> Self {
        Nav {
            span: 30.0 * 60.0,
            end: None,
        }
    }
}

impl Nav {
    pub fn range(&self, now: f64) -> (f64, f64) {
        let e = self.end.unwrap_or(now);
        (e - self.span, e)
    }
    fn set_end(&mut self, e: f64, now: f64) {
        self.end = if e >= now { None } else { Some(e) };
    }
    /// Zoom by `f`, keeping the time `at` under the pointer (else the right edge) in place.
    pub fn zoom(&mut self, f: f64, at: Option<f64>, now: f64) {
        let (a, b) = self.range(now);
        let s = (self.span * f).clamp(120.0, 14.0 * 86400.0);
        let p = at.map_or(1.0, |t| (t - a) / (b - a));
        let e = at.unwrap_or(b) + (1.0 - p) * s;
        self.span = s;
        self.set_end(e, now);
    }
    pub fn pan(&mut self, dt: f64, now: f64) {
        self.set_end(self.end.unwrap_or(now) + dt, now);
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Col {
    Dim,
    Grid,
    Done,
    Old,
    Waiting,
    Error,
    Line,
    Now,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Align {
    Left,
    Center,
    Right,
}

/// A canvas-style drawing step; text is placed by its baseline.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Fill(Col, f64, f64, f64, f64),
    Text(Col, Align, f64, f64, String),
    Stroke(Col, f64, Vec<(f64, f64)>),
    Dot(Col, f64, f64),
}

/// Local time formatting (the app uses the system's locale).
pub trait Clock {
    fn time(&self, t: f64, secs: bool) -> String;
    fn date(&self, t: f64) -> String;
    fn day_time(&self, t: f64) -> String;
    /// Seconds to add to local time to get UTC, so ticks land on local clock boundaries.
    fn utc_minus_local(&self) -> f64;
}

pub fn fmt_dur(s: Option<f64>) -> String {
    match s {
        Some(s) if s.is_finite() => {
            if s < 90.0 {
                format!("{}s", js_round(s))
            } else if s < 5400.0 {
                format!("{}m", js_round(s / 60.0))
            } else {
                format!("{}h", fixed1(s / 3600.0))
            }
        }
        _ => "–".into(),
    }
}

/// JavaScript's Math.round: halves go up.
fn js_round(x: f64) -> i64 {
    (x + 0.5).floor() as i64
}

/// JavaScript's toFixed(1): an exact tie goes up, where Rust would round it to even.
fn fixed1(x: f64) -> String {
    let exact = format!("{x:.60}"); // every digit of the double
    let tail = &exact[exact.find('.').map_or(exact.len(), |i| i + 2)..];
    if tail.starts_with('5') && tail[1..].bytes().all(|b| b == b'0') {
        return format!("{:.1}", x + 0.05);
    }
    format!("{x:.1}")
}

fn median(mut a: Vec<f64>) -> Option<f64> {
    a.sort_by(f64::total_cmp);
    a.get(a.len() >> 1).copied()
}

/// The eight numbers across the top: (name, value, what it means).
pub fn stats(d: &Data, a: f64, b: f64, clock: &dyn Clock) -> Vec<[String; 3]> {
    let vis: Vec<&Chunk> = d
        .chunks
        .iter()
        .filter(|c| c.t + c.len() >= a && c.t <= b)
        .collect();
    let waiting: Vec<&Chunk> = d.chunks.iter().filter(|c| c.state == "waiting").collect();
    let timed: Vec<&Chunk> = vis.iter().copied().filter(|c| c.done.is_some()).collect();
    let took: f64 = timed.iter().map(|c| c.took.unwrap_or(0.0)).sum();
    let audio: f64 = timed.iter().map(|c| c.len()).sum();
    // Pace right now: audio finished in the last 10 minutes of wall time, per second.
    let rate = d
        .chunks
        .iter()
        .filter(|c| c.done.is_some_and(|t| t > d.now - 600.0))
        .map(Chunk::len)
        .sum::<f64>()
        / 600.0;
    let backlog: f64 = waiting.iter().map(|c| c.len()).sum();
    let oldest = waiting.iter().map(|c| c.t).reduce(f64::min);
    let count = |f: &dyn Fn(&Chunk) -> bool| vis.iter().filter(|c| f(c)).count().to_string();
    let row = |k: &str, v: String, s: String| [k.to_string(), v, s];
    vec![
        row("recorded", vis.len().to_string(), "chunks in view".into()),
        row(
            "done",
            count(&|c| c.state == "done" || c.state == "old"),
            "chunks in view".into(),
        ),
        row(
            "waiting",
            waiting.len().to_string(),
            format!("{} audio", fmt_dur(Some(backlog))),
        ),
        row(
            "skipped",
            count(&|c| c.state == "error"),
            "errors in view".into(),
        ),
        row(
            "behind",
            fmt_dur(median(timed.iter().filter_map(|c| c.behind()).collect())),
            "median in view".into(),
        ),
        row(
            "speed",
            if took != 0.0 {
                format!("{}×", fixed1(audio / took))
            } else {
                "–".into()
            },
            "faster than real time".into(),
        ),
        row(
            "pace",
            if rate != 0.0 {
                format!("{}/min", fixed1(rate * 60.0 / CHUNK))
            } else {
                "–".into()
            },
            "done, last 10m".into(),
        ),
        row(
            "clear in",
            if backlog == 0.0 {
                "caught up".into()
            } else if rate != 0.0 {
                fmt_dur(Some(backlog / rate))
            } else {
                "stalled".into()
            },
            oldest.map_or("nothing waiting".into(), |t| {
                format!("oldest {}", clock.time(t, false))
            }),
        ),
    ]
}

pub fn range_label(nav: &Nav, now: f64, clock: &dyn Clock) -> String {
    let (a, b) = nav.range(now);
    format!(
        "{} – {}{}",
        clock.day_time(a),
        clock.time(b, false),
        if nav.end.is_none() { " · live" } else { "" }
    )
}

/// Grid lines and time labels under both charts, and the red now line.
fn ticks(ops: &mut Vec<Op>, w: f64, h: f64, a: f64, b: f64, now: f64, clock: &dyn Clock) {
    let x = |t: f64| GUTTER + (t - a) / (b - a) * (w - GUTTER);
    let step = [
        60.0, 300.0, 600.0, 900.0, 1800.0, 3600.0, 7200.0, 21600.0, 43200.0, 86400.0,
    ]
    .into_iter()
    .find(|s| (w - GUTTER) / ((b - a) / s) > 70.0)
    .unwrap_or(86400.0);
    let off = clock.utc_minus_local();
    let mut t = ((a - off) / step).ceil() * step + off;
    while t <= b {
        ops.push(Op::Stroke(
            Col::Grid,
            1.0,
            vec![(x(t), 0.0), (x(t), h - 14.0)],
        ));
        let label = if step >= 86400.0 {
            clock.date(t)
        } else {
            clock.time(t, false)
        };
        ops.push(Op::Text(Col::Dim, Align::Center, x(t), h - 3.0, label));
        t += step;
    }
    let n = x(now);
    if n <= w {
        ops.push(Op::Stroke(Col::Now, 2.0, vec![(n, 0.0), (n, h - 14.0)]));
    }
}

/// A chunk's rect for hovering: (x0, x1, y0, y1, chunk index).
pub type Hit = (f64, f64, f64, f64, usize);

/// The lanes chart (call, room, computer), and where each chunk is.
pub fn lanes(d: &Data, a: f64, b: f64, w: f64, clock: &dyn Clock) -> (Vec<Op>, Vec<Hit>) {
    let h = LANES_H;
    let mut ops = vec![];
    ticks(&mut ops, w, h, a, b, d.now, clock);
    let x = |t: f64| GUTTER + (t - a) / (b - a) * (w - GUTTER);
    let lh = (h - 18.0) / LANES.len() as f64;
    for (i, (_, name)) in LANES.iter().enumerate() {
        let y = i as f64 * lh + lh / 2.0 + 4.0;
        ops.push(Op::Text(Col::Dim, Align::Left, 0.0, y, name.to_string()));
    }
    let mut hits = vec![];
    for (k, c) in d.chunks.iter().enumerate() {
        if c.t + c.len() < a || c.t > b {
            continue;
        }
        let Some(i) = LANES.iter().position(|(tag, _)| *tag == c.tag) else {
            continue;
        };
        let x0 = GUTTER.max(x(c.t));
        let x1 = (x0 + 1.0).max(x(c.t + c.len()) - 0.5);
        let (y0, y1) = (i as f64 * lh + 3.0, (i + 1) as f64 * lh - 3.0);
        let col = match c.state.as_str() {
            "done" => Col::Done,
            "old" => Col::Old,
            "waiting" => Col::Waiting,
            "error" => Col::Error,
            _ => Col::Dim,
        };
        ops.push(Op::Fill(col, x0, y0, x1 - x0, y1 - y0));
        hits.push((x0, x1, y0, y1, k));
    }
    (ops, hits)
}

/// The pace chart: how long after recording each chunk was done (dots), chunks waiting (line).
pub fn pace(d: &Data, a: f64, b: f64, w: f64, clock: &dyn Clock) -> Vec<Op> {
    let h = PACE_H;
    let mut ops = vec![];
    ticks(&mut ops, w, h, a, b, d.now, clock);
    let x = |t: f64| GUTTER + (t - a) / (b - a) * (w - GUTTER);
    let (top, bottom) = (8.0, h - 18.0);
    let vis: Vec<&Chunk> = d
        .chunks
        .iter()
        .filter(|c| c.t >= a - 3600.0 && c.t <= b)
        .collect();
    let lags: Vec<(&Chunk, f64)> = vis
        .iter()
        .filter(|c| c.t >= a)
        .filter_map(|c| Some((*c, c.behind()?)))
        .collect();
    // Chunks waiting at time s: recorded by then, not done by then (the waiting ones still aren't).
    let n = 160;
    let mut queue = vec![];
    for k in 0..=n {
        let s = a + (b - a) * k as f64 / n as f64;
        if s > d.now {
            break;
        }
        let q = vis
            .iter()
            .filter(|c| {
                c.t + c.len() <= s && (c.state == "waiting" || c.done.is_some_and(|t| t > s))
            })
            .count();
        queue.push((s, q));
    }
    let max_lag = lags.iter().map(|&(_, l)| l).fold(60.0, f64::max);
    let max_q = queue.iter().map(|&(_, q)| q).fold(3, usize::max);
    let y = |l: f64| bottom - l / max_lag * (bottom - top);
    let yq = |q: usize| bottom - q as f64 / max_q as f64 * (bottom - top);
    ops.push(Op::Text(
        Col::Dim,
        Align::Right,
        GUTTER - 6.0,
        top + 8.0,
        fmt_dur(Some(max_lag)),
    ));
    ops.push(Op::Text(
        Col::Dim,
        Align::Right,
        GUTTER - 6.0,
        bottom,
        "0".into(),
    ));
    ops.push(Op::Text(
        Col::Line,
        Align::Left,
        GUTTER + 4.0,
        top + 8.0,
        format!("{max_q} waiting"),
    ));
    ops.push(Op::Stroke(
        Col::Line,
        1.0,
        queue.iter().map(|&(s, q)| (x(s), yq(q))).collect(),
    ));
    for (c, l) in lags {
        let col = if c.state == "error" {
            Col::Error
        } else {
            Col::Done
        };
        ops.push(Op::Dot(col, x(c.t + c.len()), y(l.max(0.0))));
    }
    ops
}

/// The hover tip for a chunk.
pub fn tip(c: &Chunk, clock: &dyn Clock) -> String {
    let name = LANES.iter().find(|(t, _)| *t == c.tag).map_or("", |l| l.1);
    let mut out = vec![format!(
        "{name} {}–{}",
        clock.time(c.t, true),
        clock.time(c.t + c.len(), true)
    )];
    let state = match c.state.as_str() {
        "done" => "done",
        "old" => "done (time not logged)",
        "waiting" => "waiting",
        "error" => "skipped",
        _ => "",
    };
    out.push(state.into());
    if let Some(d) = c.done {
        out.push(format!(
            "done {}, {} after recording",
            clock.time(d, true),
            fmt_dur(c.behind())
        ));
    }
    if let Some(t) = c.took {
        out.push(format!("took {}s", fixed1(t)));
    }
    if let Some(n) = c.lines {
        out.push(format!("{n} line{}", if n == 1 { "" } else { "s" }));
    }
    out.extend(c.error.clone());
    out.retain(|l| !l.is_empty());
    out.join("\n")
}

/// The system's locale and time zone.
pub struct Formats([Retained<NSDateFormatter>; 4]);

impl Formats {
    fn new() -> Self {
        Formats(["jjmm", "jjmmss", "yMd", "EEEjjmm"].map(|tpl| {
            let f = NSDateFormatter::new();
            f.setLocalizedDateFormatFromTemplate(&NSString::from_str(tpl));
            f
        }))
    }
    fn fmt(&self, i: usize, t: f64) -> String {
        self.0[i]
            .stringFromDate(&NSDate::dateWithTimeIntervalSince1970(t))
            .to_string()
    }
}

impl Clock for Formats {
    fn time(&self, t: f64, secs: bool) -> String {
        self.fmt(if secs { 1 } else { 0 }, t)
    }
    fn date(&self, t: f64) -> String {
        self.fmt(2, t)
    }
    fn day_time(&self, t: f64) -> String {
        self.fmt(3, t)
    }
    fn utc_minus_local(&self) -> f64 {
        -(chrono::Local::now().offset().local_minus_utc() as f64)
    }
}

fn color(c: Col) -> Retained<NSColor> {
    match c {
        Col::Dim => NSColor::secondaryLabelColor(),
        Col::Grid => NSColor::separatorColor(),
        Col::Done => NSColor::systemGreenColor(),
        Col::Old => NSColor::systemGreenColor().colorWithAlphaComponent(0.4),
        Col::Waiting => NSColor::systemOrangeColor(),
        Col::Error | Col::Now => NSColor::systemRedColor(),
        Col::Line => NSColor::systemBlueColor(),
    }
}

fn attrs(font: &NSFont, c: &NSColor) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    // SAFETY: AppKit's attribute key statics.
    let (kf, kc) = unsafe { (NSFontAttributeName, NSForegroundColorAttributeName) };
    NSDictionary::from_slices(&[kf, kc], &[font as &AnyObject, c as &AnyObject])
}

/// Draws `s` with its top-left, center-top or top-right at (x, y) per `align`; returns its size.
fn text(s: &str, x: f64, y: f64, align: Align, font: &NSFont, c: &NSColor) -> NSSize {
    let a = attrs(font, c);
    let s = NSString::from_str(s);
    // SAFETY: string drawing on the main thread, inside drawRect.
    let size = unsafe { s.sizeWithAttributes(Some(&a)) };
    let x = match align {
        Align::Left => x,
        Align::Center => x - size.width / 2.0,
        Align::Right => x - size.width,
    };
    unsafe { s.drawAtPoint_withAttributes(NSPoint::new(x, y), Some(&a)) };
    size
}

fn fill(r: NSRect) {
    NSRectFillUsingOperation(r, NSCompositingOperation::SourceOver);
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn contains(r: NSRect, p: NSPoint) -> bool {
    p.x >= r.origin.x
        && p.x <= r.origin.x + r.size.width
        && p.y >= r.origin.y
        && p.y <= r.origin.y + r.size.height
}

/// Where things go in a view `w` wide.
struct Frames {
    cols: usize,
    stat_w: f64,
    bar_y: f64,
    lanes: NSRect,
    heading_y: f64,
    pace: NSRect,
}

const PAD_X: f64 = 12.0;
const STAT_H: f64 = 64.0; // name, value, and a meaning that may wrap to two lines

fn frames(w: f64) -> Frames {
    let cw = w - 2.0 * PAD_X;
    let cols = (((cw + 12.0) / 108.0).floor() as usize).max(1);
    let rows = 8usize.div_ceil(cols);
    let stat_w = (cw - (cols - 1) as f64 * 12.0) / cols as f64;
    let bar_y = 10.0 + rows as f64 * STAT_H + (rows - 1) as f64 * 6.0 + 12.0;
    let lanes = rect(PAD_X, bar_y + 26.0, cw, LANES_H);
    let heading_y = lanes.origin.y + LANES_H + 10.0;
    let pace = rect(PAD_X, heading_y + 17.0, cw, PACE_H);
    Frames {
        cols,
        stat_w,
        bar_y,
        lanes,
        heading_y,
        pace,
    }
}

pub struct Ivars {
    data: RefCell<Data>,
    nav: Cell<Nav>,
    hits: RefCell<Vec<(NSRect, usize)>>, // chunk rects in view coordinates, from the last draw
    tip: Cell<Option<(NSPoint, usize)>>,
    drag: Cell<bool>,
    buttons: OnceCell<[Retained<NSButton>; 3]>,
    formats: Formats,
}

define_class!(
    // SAFETY: NSView subclassing: we only override drawing, layout, mouse and button actions; no Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct TimebarView;

    unsafe impl NSObjectProtocol for TimebarView {}

    impl TimebarView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            self.draw();
        }

        #[unsafe(method(resizeSubviewsWithOldSize:))]
        fn resize_subviews(&self, _old: NSSize) {
            self.place_buttons();
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let f = frames(self.bounds().size.width);
            let hand = NSCursor::openHandCursor();
            self.addCursorRect_cursor(f.lanes, &hand);
            self.addCursorRect_cursor(f.pace, &hand);
        }

        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, e: &NSEvent) {
            let Some(w) = self.chart_at(e) else { return };
            let k = if e.hasPreciseScrollingDeltas() { 1.0 } else { 10.0 };
            let (dx, dy) = (-e.scrollingDeltaX() * k, -e.scrollingDeltaY() * k); // the web's wheel deltas
            if e.modifierFlags().contains(NSEventModifierFlags::Command) {
                self.zoom((dy / 100.0).exp(), Some(self.time_at(e, w))); // pinch-less zoom, as the page did
            } else {
                let d = if dx != 0.0 { dx } else { dy };
                self.pan(d / (w - GUTTER));
            }
        }

        #[unsafe(method(magnifyWithEvent:))]
        fn magnify(&self, e: &NSEvent) {
            if let Some(w) = self.chart_at(e) {
                self.zoom(1.0 / (1.0 + e.magnification()).max(0.1), Some(self.time_at(e, w)));
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &NSEvent) {
            if self.chart_at(e).is_some() {
                self.ivars().drag.set(true);
                NSCursor::closedHandCursor().push();
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, e: &NSEvent) {
            if self.ivars().drag.get() {
                let w = frames(self.bounds().size.width).lanes.size.width;
                self.pan(-e.deltaX() / (w - GUTTER));
            }
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _e: &NSEvent) {
            if self.ivars().drag.replace(false) {
                NSCursor::pop_class();
            }
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, e: &NSEvent) {
            let p = self.convertPoint_fromView(e.locationInWindow(), None);
            let hit = self
                .ivars()
                .hits
                .borrow()
                .iter()
                .find(|(r, _)| contains(rect(r.origin.x - 1.0, r.origin.y, r.size.width + 2.0, r.size.height), p))
                .map(|&(_, k)| (p, k));
            if hit.is_some() || self.ivars().tip.get().is_some() {
                self.ivars().tip.set(hit);
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _e: &NSEvent) {
            if self.ivars().tip.take().is_some() {
                self.setNeedsDisplay(true);
            }
        }

        #[unsafe(method(zoomIn:))]
        fn zoom_in(&self, _s: Option<&AnyObject>) {
            self.zoom(0.5, None);
        }

        #[unsafe(method(zoomOut:))]
        fn zoom_out(&self, _s: Option<&AnyObject>) {
            self.zoom(2.0, None);
        }

        #[unsafe(method(followNow:))]
        fn follow_now(&self, _s: Option<&AnyObject>) {
            let mut nav = self.ivars().nav.get();
            nav.end = None;
            self.ivars().nav.set(nav);
            self.setNeedsDisplay(true);
        }
    }
);

impl TimebarView {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            data: RefCell::new(Data::default()),
            nav: Cell::new(Nav::default()),
            hits: RefCell::new(vec![]),
            tip: Cell::new(None),
            drag: Cell::new(false),
            buttons: OnceCell::new(),
            formats: Formats::new(),
        });
        // SAFETY: NSView's designated initializer with a zero frame.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] };
        let buttons = [
            ("−", sel!(zoomOut:)),
            ("+", sel!(zoomIn:)),
            ("Now", sel!(followNow:)),
        ]
        .map(|(title, action)| {
            // SAFETY: the view is the target and owns the button.
            let b = unsafe {
                NSButton::buttonWithTitle_target_action(
                    &NSString::from_str(title),
                    Some(&this),
                    Some(action),
                    mtm,
                )
            };
            b.setControlSize(NSControlSize::Small);
            this.addSubview(&b);
            b
        });
        let _ = this.ivars().buttons.set(buttons);
        // SAFETY: a tracking area owned by this view, following its visible rect.
        let area = unsafe {
            NSTrackingArea::initWithRect_options_owner_userInfo(
                NSTrackingArea::alloc(),
                NSRect::ZERO,
                NSTrackingAreaOptions::MouseMoved
                    | NSTrackingAreaOptions::MouseEnteredAndExited
                    | NSTrackingAreaOptions::ActiveAlways
                    | NSTrackingAreaOptions::InVisibleRect,
                Some(&this),
                None,
            )
        };
        this.addTrackingArea(&area);
        this
    }

    pub fn set_data(&self, d: Data) {
        *self.ivars().data.borrow_mut() = d;
        self.setNeedsDisplay(true);
    }

    fn now(&self) -> f64 {
        self.ivars().data.borrow().now
    }

    fn zoom(&self, f: f64, at: Option<f64>) {
        let mut nav = self.ivars().nav.get();
        nav.zoom(f, at, self.now());
        self.ivars().nav.set(nav);
        self.setNeedsDisplay(true);
    }

    /// Move by `frac` of the view's span.
    fn pan(&self, frac: f64) {
        let mut nav = self.ivars().nav.get();
        nav.pan(frac * nav.span, self.now());
        self.ivars().nav.set(nav);
        self.setNeedsDisplay(true);
    }

    /// The chart under the event, as its width.
    fn chart_at(&self, e: &NSEvent) -> Option<f64> {
        let p = self.convertPoint_fromView(e.locationInWindow(), None);
        let f = frames(self.bounds().size.width);
        (contains(f.lanes, p) || contains(f.pace, p)).then_some(f.lanes.size.width)
    }

    fn time_at(&self, e: &NSEvent, w: f64) -> f64 {
        let p = self.convertPoint_fromView(e.locationInWindow(), None);
        let (a, b) = self.ivars().nav.get().range(self.now());
        a + (p.x - PAD_X - GUTTER) / (w - GUTTER) * (b - a)
    }

    fn place_buttons(&self) {
        let Some(buttons) = self.ivars().buttons.get() else {
            return;
        };
        let f = frames(self.bounds().size.width);
        let mut x = self.bounds().size.width - PAD_X;
        for b in buttons.iter().rev() {
            let size = b.fittingSize();
            x -= size.width;
            b.setFrame(rect(
                x,
                f.bar_y + (22.0 - size.height) / 2.0,
                size.width,
                size.height,
            ));
            x -= 4.0;
        }
    }

    fn draw(&self) {
        let iv = self.ivars();
        let bounds = self.bounds();
        NSColor::textBackgroundColor().setFill();
        fill(bounds);
        let f = frames(bounds.size.width);
        let d = iv.data.borrow();
        let nav = iv.nav.get();
        let (a, b) = nav.range(d.now);
        let clock = &iv.formats;
        // SAFETY: AppKit's font weight constants.
        let (regular, semibold) = unsafe { (NSFontWeightRegular, NSFontWeightSemibold) };
        let small = NSFont::systemFontOfSize(12.0);
        let big = NSFont::monospacedDigitSystemFontOfSize_weight(16.0, semibold);
        let dim = NSColor::secondaryLabelColor();
        for (i, [k, v, s]) in stats(&d, a, b, clock).iter().enumerate() {
            let x = PAD_X + (i % f.cols) as f64 * (f.stat_w + 12.0);
            let y = 10.0 + (i / f.cols) as f64 * (STAT_H + 6.0);
            text(k, x, y, Align::Left, &small, &dim);
            text(v, x, y + 14.0, Align::Left, &big, &NSColor::labelColor());
            // SAFETY: string drawing on the main thread, inside drawRect.
            unsafe {
                NSString::from_str(s).drawWithRect_options_attributes_context(
                    rect(x, y + 34.0, f.stat_w, 30.0),
                    NSStringDrawingOptions::UsesLineFragmentOrigin,
                    Some(&attrs(&small, &dim)),
                    None,
                )
            };
        }
        // The bar: the range on the left, the color key before the buttons.
        let range = range_label(&nav, d.now, clock);
        text(&range, PAD_X, f.bar_y + 3.0, Align::Left, &small, &dim);
        let buttons_x = iv
            .buttons
            .get()
            .map_or(bounds.size.width, |b| b[0].frame().origin.x);
        let key = [
            (Col::Done, "done"),
            (Col::Old, "done, time unknown"),
            (Col::Waiting, "waiting"),
            (Col::Error, "skipped"),
        ];
        let a_small = attrs(&small, &dim);
        let widths: Vec<f64> = key
            .iter()
            .map(|(_, l)| unsafe { NSString::from_str(l).sizeWithAttributes(Some(&a_small)) }.width)
            .collect();
        let mut x = buttons_x - 8.0 - widths.iter().map(|w| w + 20.0).sum::<f64>();
        for ((c, l), w) in key.iter().zip(&widths) {
            x += 8.0;
            color(*c).setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                rect(x, f.bar_y + 7.0, 9.0, 9.0),
                2.0,
                2.0,
            )
            .fill();
            x += 12.0;
            text(l, x, f.bar_y + 3.0, Align::Left, &small, &dim);
            x += w;
        }
        let tiny = NSFont::monospacedDigitSystemFontOfSize_weight(11.0, regular);
        let (lane_ops, hits) = lanes(&d, a, b, f.lanes.size.width, clock);
        paint(&lane_ops, f.lanes.origin, &tiny);
        *iv.hits.borrow_mut() = hits
            .iter()
            .map(|&(x0, x1, y0, y1, k)| {
                (
                    rect(
                        f.lanes.origin.x + x0,
                        f.lanes.origin.y + y0,
                        x1 - x0,
                        y1 - y0,
                    ),
                    k,
                )
            })
            .collect();
        let heading = NSFont::systemFontOfSize_weight(12.0, semibold);
        text(
            "Behind: how long after recording each chunk was done (dots), chunks waiting (line)",
            PAD_X,
            f.heading_y,
            Align::Left,
            &heading,
            &dim,
        );
        paint(
            &pace(&d, a, b, f.pace.size.width, clock),
            f.pace.origin,
            &tiny,
        );
        if let Some((p, k)) = iv.tip.get()
            && let Some(c) = d.chunks.get(k)
        {
            let a = attrs(&small, &NSColor::textBackgroundColor());
            let s = NSString::from_str(&tip(c, clock));
            let size = unsafe { s.sizeWithAttributes(Some(&a)) };
            let x = (p.x + 12.0).min(bounds.size.width - size.width - 18.0);
            let r = rect(x, p.y + 14.0, size.width + 14.0, size.height + 10.0);
            NSColor::labelColor().setFill();
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 5.0, 5.0).fill();
            unsafe { s.drawAtPoint_withAttributes(NSPoint::new(x + 7.0, p.y + 19.0), Some(&a)) };
        }
    }

    /// The scene as the render check prints it, and a PNG of the window's view.
    pub fn dump(&self, png: &str) -> String {
        let iv = self.ivars();
        let d = iv.data.borrow();
        let f = frames(self.bounds().size.width);
        let clock = &iv.formats;
        let mut out = vec![format!("WIDTH {:?}", f.lanes.size.width)];
        // the view as shown, then zoomed out to hours and days, and back in a day
        let day = 86400.0;
        let navs = [
            iv.nav.get(),
            Nav {
                span: 6.0 * 3600.0,
                end: None,
            },
            Nav {
                span: 3.0 * day,
                end: None,
            },
            Nav {
                span: 14.0 * day,
                end: None,
            },
            Nav {
                span: 2.0 * 3600.0,
                end: Some(d.now - day),
            },
        ];
        for nav in navs {
            let (a, b) = nav.range(d.now);
            out.push(format!("NAV {:?} {:?}", nav.span, nav.end));
            out.extend(
                stats(&d, a, b, clock)
                    .iter()
                    .map(|r| format!("STAT {}", r.join("|"))),
            );
            out.push(format!("RANGE {}", range_label(&nav, d.now, clock)));
            let fmt = |tag: &str, ops: &[Op]| {
                ops.iter()
                    .map(|o| format!("{tag} {}", op_line(o)))
                    .collect::<Vec<_>>()
            };
            out.extend(fmt("LANES", &lanes(&d, a, b, f.lanes.size.width, clock).0));
            out.extend(fmt("PACE", &pace(&d, a, b, f.pace.size.width, clock)));
        }
        if let Some(c) = d.chunks.last() {
            out.push(format!("TIP {}", tip(c, clock).replace('\n', " / ")));
        }
        drop(d);
        // the buttons and a drag, as the view sees them
        // SAFETY: clicking our own buttons on the main thread; their target is this view.
        if let Some([out_b, in_b, now_b]) = iv.buttons.get() {
            unsafe { in_b.performClick(None) };
            out.push(format!("CLICK + {:?}", iv.nav.get()));
            self.pan(-0.5);
            out.push(format!("DRAG half a view back {:?}", iv.nav.get()));
            unsafe { out_b.performClick(None) };
            out.push(format!("CLICK − {:?}", iv.nav.get()));
            unsafe { now_b.performClick(None) };
            out.push(format!("CLICK Now {:?}", iv.nav.get()));
        }
        if let Some(rep) = self.bitmapImageRepForCachingDisplayInRect(self.bounds()) {
            self.cacheDisplayInRect_toBitmapImageRep(self.bounds(), &rep);
            // SAFETY: PNG encoding of our own bitmap.
            if let Some(data) = unsafe {
                rep.representationUsingType_properties(
                    objc2_app_kit::NSBitmapImageFileType::PNG,
                    &NSDictionary::new(),
                )
            } {
                let _ = std::fs::write(png, data.to_vec());
            }
        }
        out.join("\n")
    }
}

fn op_line(o: &Op) -> String {
    match o {
        Op::Fill(c, x, y, w, h) => format!("FILL {c:?} {x:?} {y:?} {w:?} {h:?}"),
        Op::Text(c, al, x, y, s) => format!("TEXT {c:?} {al:?} {x:?} {y:?} {s}"),
        Op::Stroke(c, w, pts) => format!(
            "STROKE {c:?} {w:?}{}",
            pts.iter()
                .map(|(x, y)| format!(" {x:?},{y:?}"))
                .collect::<String>()
        ),
        Op::Dot(c, x, y) => format!("DOT {c:?} {x:?} {y:?}"),
    }
}

/// Paints canvas-style ops at `origin`; text sits on its baseline.
fn paint(ops: &[Op], origin: NSPoint, font: &NSFont) {
    let (ox, oy) = (origin.x, origin.y);
    let ascent = font.ascender();
    for op in ops {
        match op {
            Op::Fill(c, x, y, w, h) => {
                color(*c).setFill();
                fill(rect(ox + x, oy + y, *w, *h));
            }
            Op::Text(c, al, x, y, s) => {
                text(s, ox + x, oy + y - ascent, *al, font, &color(*c));
            }
            Op::Stroke(c, w, pts) if pts.len() > 1 => {
                let path = NSBezierPath::bezierPath();
                path.moveToPoint(NSPoint::new(ox + pts[0].0, oy + pts[0].1));
                for (x, y) in &pts[1..] {
                    path.lineToPoint(NSPoint::new(ox + x, oy + y));
                }
                path.setLineWidth(*w);
                color(*c).setStroke();
                path.stroke();
            }
            Op::Stroke(..) => {}
            Op::Dot(c, x, y) => {
                color(*c).setFill();
                NSBezierPath::bezierPathWithOvalInRect(rect(ox + x - 2.0, oy + y - 2.0, 4.0, 4.0))
                    .fill();
            }
        }
    }
}

impl App {
    pub fn timebar_view(&self) -> &Retained<TimebarView> {
        self.ivars()
            .timebar_view
            .get_or_init(|| TimebarView::new(self.mtm()))
    }

    pub fn show_timebar(&self) {
        let mtm = self.mtm();
        let iv = self.ivars();
        if iv.timebar_window.borrow().is_none() {
            // SAFETY: a plain window we keep (not released on close).
            let w = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    rect(0.0, 0.0, 900.0, 440.0),
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            w.setTitle(ns_string!("Ozen Timebar"));
            unsafe { w.setReleasedWhenClosed(false) };
            w.setContentView(Some(self.timebar_view()));
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
            if let Ok(d) = serde_json::from_str::<Data>(&out) {
                crate::APP.with(|a| a.get().unwrap().timebar_view().set_data(d));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Utc;
    impl Clock for Utc {
        fn time(&self, t: f64, secs: bool) -> String {
            let d = chrono::DateTime::from_timestamp(t.floor() as i64, 0).unwrap();
            d.format(if secs { "%H:%M:%S" } else { "%H:%M" })
                .to_string()
        }
        fn date(&self, t: f64) -> String {
            let d = chrono::DateTime::from_timestamp(t.floor() as i64, 0).unwrap();
            d.format("%-d.%-m.%Y").to_string()
        }
        fn day_time(&self, t: f64) -> String {
            let d = chrono::DateTime::from_timestamp(t.floor() as i64, 0).unwrap();
            d.format("%a %H:%M").to_string()
        }
        fn utc_minus_local(&self) -> f64 {
            0.0
        }
    }

    fn chunk(t: f64, tag: &str, state: &str, done: Option<f64>, took: Option<f64>) -> Chunk {
        Chunk {
            t,
            tag: tag.into(),
            state: state.into(),
            done,
            took,
            ..Default::default()
        }
    }

    #[test]
    fn formats_like_the_page() {
        assert_eq!(fmt_dur(None), "–");
        assert_eq!(fmt_dur(Some(f64::INFINITY)), "–");
        assert_eq!(fmt_dur(Some(2.5)), "3s"); // Math.round: halves up
        assert_eq!(fmt_dur(Some(-2.5)), "-2s");
        assert_eq!(fmt_dur(Some(-0.4)), "0s");
        assert_eq!(fmt_dur(Some(89.4)), "89s");
        assert_eq!(fmt_dur(Some(150.0)), "3m"); // 2.5 minutes, up
        assert_eq!(fmt_dur(Some(5400.0)), "1.5h");
        assert_eq!(fixed1(0.25), "0.3"); // toFixed: an exact tie goes up
        assert_eq!(fixed1(0.35), "0.3"); // 0.35 is really 0.34999…
        assert_eq!(fixed1(2.0), "2.0");
    }

    #[test]
    fn zoom_keeps_the_pointer_and_pans_back_to_live() {
        let now = 10_000.0;
        let mut n = Nav::default();
        n.zoom(0.5, None, now); // the + button: keep following now
        assert_eq!(
            n,
            Nav {
                span: 900.0,
                end: None
            }
        );
        n.pan(-300.0, now);
        assert_eq!(n.end, Some(9_700.0));
        n.zoom(0.5, Some(9_250.0), now); // 9250 stays where it was, halfway across
        assert_eq!(
            n,
            Nav {
                span: 450.0,
                end: Some(9_475.0)
            }
        );
        n.zoom(1e-9, None, now);
        assert_eq!(n.span, 120.0);
        n.pan(1e9, now); // past now: live again
        assert_eq!(n.end, None);
    }

    #[test]
    fn stats_and_charts() {
        let now = 3_600.0;
        let d = Data {
            now,
            chunks: vec![
                chunk(3_000.0, "call", "done", Some(3_030.0), Some(1.5)),
                chunk(3_000.0, "mic", "old", None, None),
                chunk(3_300.0, "local", "error", Some(3_320.0), Some(0.5)),
                chunk(3_500.0, "call", "waiting", None, None),
                chunk(3_550.0, "other", "waiting", None, None),
            ],
        };
        let (a, b) = Nav::default().range(now);
        let s = stats(&d, a, b, &Utc);
        let v: Vec<&str> = s.iter().map(|r| r[1].as_str()).collect();
        assert_eq!(v, ["5", "2", "2", "1", "15s", "15.0×", "0.2/min", "10m"]);
        assert_eq!(s[2][2], "30s audio");
        assert_eq!(s[7][2], "oldest 00:58");
        assert_eq!(
            range_label(&Nav::default(), now, &Utc),
            "Thu 00:30 – 01:00 · live"
        );
        let (ops, hits) = lanes(&d, a, b, 664.0, &Utc);
        assert_eq!(hits.len(), 4); // "other" has no lane
        // 30 min over 600px: a 15s chunk is 5px, drawn 0.5px short of its end
        assert_eq!(hits[0], (GUTTER + 400.0, GUTTER + 404.5, 3.0, 23.0, 0));
        assert!(ops.contains(&Op::Fill(Col::Done, GUTTER + 400.0, 3.0, 4.5, 20.0)));
        assert!(ops.contains(&Op::Text(Col::Dim, Align::Left, 0.0, 43.0, "room".into())));
        // 300s ticks (60s would be 20px apart): 00:30 to 01:00 is seven of them
        let grid = ops
            .iter()
            .filter(|o| matches!(o, Op::Stroke(Col::Grid, ..)))
            .count();
        assert_eq!(grid, 7);
        assert!(ops.contains(&Op::Stroke(
            Col::Now,
            2.0,
            vec![(664.0, 0.0), (664.0, 82.0)]
        )));
        let p = pace(&d, a, b, 664.0, &Utc);
        // behind: 15s (done) and 5s (skipped), under the 60s floor
        assert!(p.contains(&Op::Dot(
            Col::Done,
            GUTTER + 405.0,
            132.0 - 15.0 / 60.0 * 124.0
        )));
        assert!(p.contains(&Op::Dot(
            Col::Error,
            GUTTER + 505.0,
            132.0 - 5.0 / 60.0 * 124.0
        )));
        assert!(p.contains(&Op::Text(
            Col::Line,
            Align::Left,
            GUTTER + 4.0,
            16.0,
            "3 waiting".into()
        )));
        assert_eq!(
            tip(&d.chunks[0], &Utc),
            "call 00:50:00–00:50:15\ndone\ndone 00:50:30, 15s after recording\ntook 1.5s"
        );
    }
}
