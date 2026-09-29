//! The Timeline view: one lane per speaker, a bar for each line they spoke, on a horizontally scrollable time
//! axis. Silences longer than GAP_CAP are squeezed to a short break marker so a day of meetings stays scrollable.
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAttributedStringNSExtendedStringDrawing, NSBezierPath, NSColor, NSCompositingOperation,
    NSEvent, NSFont, NSFontAttributeName, NSFontWeightRegular, NSForegroundColorAttributeName,
    NSRectFillUsingOperation, NSStringDrawing, NSStringDrawingOptions, NSView, NSViewToolTipOwner,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSAttributedStringKey, NSDictionary,
    NSMutableAttributedString, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use std::cell::{Cell, RefCell};

pub const GUTTER: f64 = 112.0;
const LANE_H: f64 = 28.0;
const AXIS_H: f64 = 22.0;
const GAP_CAP: f64 = 120.0;
const BREAK_W: f64 = 36.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub id: String,
    pub t: f64,
    pub d: f64,
    pub speaker: String,
    pub text: String,
    pub unsure: bool,
}

/// Where everything goes, from the segments and the zoom (pure, so it's tested without a window).
#[derive(Default, Debug, PartialEq)]
pub struct Layout {
    pub lanes: Vec<String>,          // by total talk time, most first
    pub bars: Vec<(NSRect, usize)>,  // rect, segment index
    pub spans: Vec<(f64, f64, f64)>, // continuous stretches between breaks: t0, t1, x0
    pub breaks: Vec<(f64, f64)>,     // x, gap in seconds
    pub width: f64,                  // content width before fitting the scroll view
    pub height: f64,
}

pub fn layout(segments: &[Segment], px: f64) -> Layout {
    let mut talk: Vec<(String, f64)> = vec![];
    for s in segments {
        match talk.iter_mut().find(|(n, _)| *n == s.speaker) {
            Some(t) => t.1 += s.d,
            None => talk.push((s.speaker.clone(), s.d)),
        }
    }
    talk.sort_by(|a, b| b.1.total_cmp(&a.1)); // ponytail: Swift sorted a dictionary; ties may order differently
    let lanes: Vec<String> = talk.into_iter().map(|(n, _)| n).collect();
    let mut l = Layout {
        lanes,
        ..Default::default()
    };
    let mut order: Vec<usize> = (0..segments.len()).collect();
    order.sort_by(|&a, &b| segments[a].t.total_cmp(&segments[b].t));
    let (mut x, mut cursor): (f64, Option<f64>) = (GUTTER + 12.0, None);
    for i in order {
        let s = &segments[i];
        if let Some(c) = cursor
            && s.t - c > GAP_CAP
        {
            // long silence: fixed-width break instead of real time
            l.spans.last_mut().unwrap().1 = c;
            l.breaks.push((x + 4.0, s.t - c));
            x += BREAK_W;
            cursor = None;
        }
        if cursor.is_none() {
            l.spans.push((s.t, s.t, x));
            cursor = Some(s.t);
        }
        let c = cursor.unwrap();
        let c = if s.t > c {
            x += (s.t - c) * px;
            s.t
        } else {
            c
        };
        let start = x - (c - s.t) * px; // overlapping speech starts before the cursor
        let lane = l.lanes.iter().position(|n| *n == s.speaker).unwrap_or(0) as f64;
        let rect = NSRect::new(
            NSPoint::new(start, AXIS_H + lane * LANE_H + 5.0),
            NSSize::new((s.d * px).max(3.0), LANE_H - 10.0),
        );
        l.bars.push((rect, i));
        let c = if s.t + s.d > c {
            x += (s.t + s.d - c) * px;
            s.t + s.d
        } else {
            c
        };
        cursor = Some(c);
        l.spans.last_mut().unwrap().1 = c;
    }
    l.width = x + 60.0;
    l.height = AXIS_H + l.lanes.len() as f64 * LANE_H + 8.0;
    l
}

/// A stable color per speaker name.
pub fn color(name: &str) -> Retained<NSColor> {
    if name == "?" {
        return NSColor::tertiaryLabelColor();
    }
    let h = name
        .chars()
        .fold(5381u32, |h, c| h.wrapping_mul(33).wrapping_add(c as u32));
    let palette = [
        NSColor::systemBlueColor,
        NSColor::systemGreenColor,
        NSColor::systemPurpleColor,
        NSColor::systemPinkColor,
        NSColor::systemTealColor,
        NSColor::systemIndigoColor,
        NSColor::systemBrownColor,
        NSColor::systemMintColor,
        NSColor::systemCyanColor,
        NSColor::systemYellowColor,
    ];
    palette[(h % palette.len() as u32) as usize]()
}

/// Called with a line id when its bar is clicked.
type OnSelect = Box<dyn Fn(&str)>;

pub struct Ivars {
    segments: RefCell<Vec<Segment>>,
    px: Cell<f64>,
    layout: RefCell<Layout>,
    on_select: RefCell<Option<OnSelect>>,
}

define_class!(
    // SAFETY: NSView subclassing: we only override drawing, flipping, mouse and tooltip methods; no Drop.
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Ivars]
    pub struct TimelineView;

    unsafe impl NSObjectProtocol for TimelineView {}

    unsafe impl NSViewToolTipOwner for TimelineView {
        #[unsafe(method_id(view:stringForToolTip:point:userData:))]
        fn tooltip(&self, _view: &NSView, _tag: isize, point: NSPoint, _data: *mut std::ffi::c_void) -> Retained<NSString> {
            let segs = self.ivars().segments.borrow();
            NSString::from_str(&self.segment_at(point).map_or(String::new(), |i| {
                let s = &segs[i];
                format!("{}  {}{}  ({}s)\n{}", hms(s.t), s.speaker, if s.unsure { " ?" } else { "" }, s.d.round() as i64, s.text)
            }))
        }
    }

    impl TimelineView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: NSRect) {
            self.draw(dirty);
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let p = self.convertPoint_fromView(event.locationInWindow(), None);
            if let Some(i) = self.segment_at(p) {
                let id = self.ivars().segments.borrow()[i].id.clone();
                if let Some(f) = self.ivars().on_select.borrow().as_ref() {
                    f(&id);
                }
            }
        }
    }
);

fn hms(t: f64) -> String {
    local(t).format("%H:%M:%S").to_string()
}

fn local(t: f64) -> chrono::DateTime<chrono::Local> {
    chrono::DateTime::from_timestamp(t.floor() as i64, 0)
        .unwrap_or_default()
        .with_timezone(&chrono::Local)
}

fn str_attrs(
    font: &objc2_app_kit::NSFont,
    color: &NSColor,
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    // SAFETY: AppKit's attribute key statics.
    let (kf, kc) = unsafe { (NSFontAttributeName, NSForegroundColorAttributeName) };
    NSDictionary::from_slices(&[kf, kc], &[font as &AnyObject, color as &AnyObject])
}

/// Swift's NSRect.fill(): blends with source-over (NSRectFill would copy, ignoring alpha).
fn fill(r: NSRect) {
    NSRectFillUsingOperation(r, NSCompositingOperation::SourceOver);
}

impl TimelineView {
    pub fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars {
            segments: RefCell::new(vec![]),
            px: Cell::new(4.0),
            layout: RefCell::new(Layout::default()),
            on_select: RefCell::new(None),
        });
        // SAFETY: NSView's designated initializer with a zero frame.
        unsafe { msg_send![super(this), initWithFrame: NSRect::ZERO] }
    }

    pub fn set_on_select(&self, f: impl Fn(&str) + 'static) {
        *self.ivars().on_select.borrow_mut() = Some(Box::new(f));
    }

    pub fn segments(&self) -> Vec<Segment> {
        self.ivars().segments.borrow().clone()
    }

    pub fn set_segments(&self, s: Vec<Segment>) {
        *self.ivars().segments.borrow_mut() = s;
        self.relayout();
    }

    pub fn px(&self) -> f64 {
        self.ivars().px.get()
    }

    pub fn set_px(&self, px: f64) {
        self.ivars().px.set(px);
        self.relayout();
    }

    /// The layout and a PNG of the whole view, for the render check.
    pub fn dump(&self, png: &std::path::Path) -> Vec<String> {
        let l = self.ivars().layout.borrow();
        let f = |v: f64| format!("{v:?}");
        let r = |r: &NSRect| {
            format!(
                "{} {} {} {}",
                f(r.origin.x),
                f(r.origin.y),
                f(r.size.width),
                f(r.size.height)
            )
        };
        let segs = self.ivars().segments.borrow();
        let mut out = vec![format!("LANES {}", l.lanes.join("|"))];
        out.extend(
            l.bars
                .iter()
                .map(|(b, i)| format!("BAR {} {}", segs[*i].id, r(b))),
        );
        out.extend(
            l.spans
                .iter()
                .map(|s| format!("SPAN {} {} {}", f(s.0), f(s.1), f(s.2))),
        );
        out.extend(
            l.breaks
                .iter()
                .map(|b| format!("BREAK {} {}", f(b.0), f(b.1))),
        );
        out.push(format!("FRAME {}", r(&self.frame())));
        if let Some(rep) = self.bitmapImageRepForCachingDisplayInRect(self.bounds()) {
            self.cacheDisplayInRect_toBitmapImageRep(self.bounds(), &rep);
            // SAFETY: PNG encoding of our own bitmap.
            let data = unsafe {
                rep.representationUsingType_properties(
                    objc2_app_kit::NSBitmapImageFileType::PNG,
                    &objc2_foundation::NSDictionary::new(),
                )
            };
            if let Some(d) = data {
                let _ = std::fs::write(png, d.to_vec());
            }
        }
        out
    }

    fn relayout(&self) {
        let l = layout(&self.ivars().segments.borrow(), self.px());
        // SAFETY: reading our own superview's size.
        let sup = unsafe { self.superview() }.map_or(NSSize::new(0.0, 0.0), |s| s.bounds().size);
        self.setFrameSize(NSSize::new(
            l.width.max(sup.width),
            l.height.max(sup.height),
        ));
        // SAFETY: tooltip rects owned by this view, which answers them.
        unsafe {
            self.removeAllToolTips();
            for (r, _) in &l.bars {
                let owner: &AnyObject = self.as_ref();
                self.addToolTipRect_owner_userData(*r, owner, std::ptr::null_mut());
            }
        }
        *self.ivars().layout.borrow_mut() = l;
        self.setNeedsDisplay(true);
    }

    fn segment_at(&self, p: NSPoint) -> Option<usize> {
        self.ivars()
            .layout
            .borrow()
            .bars
            .iter()
            .find(|(r, _)| {
                let r = NSRect::new(
                    NSPoint::new(r.origin.x - 2.0, r.origin.y - 2.0),
                    NSSize::new(r.size.width + 4.0, r.size.height + 4.0),
                );
                p.x >= r.origin.x
                    && p.x <= r.origin.x + r.size.width
                    && p.y >= r.origin.y
                    && p.y <= r.origin.y + r.size.height
            })
            .map(|(_, i)| *i)
    }

    fn draw(&self, dirty: NSRect) {
        let l = self.ivars().layout.borrow();
        let segs = self.ivars().segments.borrow();
        let px = self.px();
        let bounds = self.bounds();
        NSColor::textBackgroundColor().setFill();
        fill(dirty);
        let small_font =
            NSFont::monospacedDigitSystemFontOfSize_weight(10.0, unsafe { NSFontWeightRegular });
        let small = str_attrs(&small_font, &NSColor::secondaryLabelColor());
        for (i, _) in l.lanes.iter().enumerate().filter(|(i, _)| i % 2 == 1) {
            // zebra lanes
            NSColor::quaternaryLabelColor()
                .colorWithAlphaComponent(0.08)
                .setFill();
            fill(NSRect::new(
                NSPoint::new(dirty.origin.x, AXIS_H + i as f64 * LANE_H),
                NSSize::new(dirty.size.width, LANE_H),
            ));
        }
        let step = if px >= 8.0 {
            30.0
        } else if px >= 2.0 {
            60.0
        } else if px >= 0.5 {
            300.0
        } else {
            900.0
        }; // tick spacing, s
        for &(t0, t1, x0) in &l.spans {
            let mut m = (t0 / step).ceil() * step;
            while m <= t1 {
                let tx = x0 + (m - t0) * px;
                NSColor::separatorColor().setFill();
                fill(NSRect::new(
                    NSPoint::new(tx, AXIS_H - 6.0),
                    NSSize::new(1.0, bounds.size.height),
                ));
                let label = NSString::from_str(&local(m).format("%H:%M").to_string());
                unsafe {
                    label.drawAtPoint_withAttributes(NSPoint::new(tx + 3.0, 4.0), Some(&small))
                };
                m += step;
            }
        }
        for &(x, gap) in &l.breaks {
            // squeezed silence
            let label = if gap >= 3600.0 {
                format!("{}h", (gap / 3600.0) as i64)
            } else {
                format!("{}m", (gap / 60.0) as i64)
            };
            NSColor::separatorColor().setFill();
            fill(NSRect::new(
                NSPoint::new(x + BREAK_W / 2.0 - 3.0, AXIS_H),
                NSSize::new(1.0, bounds.size.height),
            ));
            fill(NSRect::new(
                NSPoint::new(x + BREAK_W / 2.0 + 2.0, AXIS_H),
                NSSize::new(1.0, bounds.size.height),
            ));
            unsafe {
                NSString::from_str(&format!("⋯{label}"))
                    .drawAtPoint_withAttributes(NSPoint::new(x, 4.0), Some(&small))
            };
        }
        for &(r, i) in l.bars.iter().filter(|(r, _)| intersects(*r, dirty)) {
            let s = &segs[i];
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 3.0, 3.0);
            color(&s.speaker)
                .colorWithAlphaComponent(if s.unsure { 0.45 } else { 0.85 })
                .setFill();
            path.fill();
            if s.unsure {
                NSColor::systemOrangeColor().setStroke();
                path.setLineWidth(1.5);
                path.stroke();
            }
        }
        // Speaker names stay pinned to the left edge while scrolling horizontally.
        let visible = self.visibleRect();
        let g = NSRect::new(
            NSPoint::new(visible.origin.x, 0.0),
            NSSize::new(GUTTER, bounds.size.height),
        );
        NSColor::windowBackgroundColor().setFill();
        fill(g);
        NSColor::separatorColor().setFill();
        fill(NSRect::new(
            NSPoint::new(g.origin.x + g.size.width - 1.0, 0.0),
            NSSize::new(1.0, bounds.size.height),
        ));
        let bold = str_attrs(&NSFont::boldSystemFontOfSize(11.0), &NSColor::labelColor());
        for (i, name) in l.lanes.iter().enumerate() {
            let y = AXIS_H + i as f64 * LANE_H;
            color(name).setFill();
            NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
                NSPoint::new(g.origin.x + 8.0, y + LANE_H / 2.0 - 4.0),
                NSSize::new(8.0, 8.0),
            ))
            .fill();
            let total: f64 = segs
                .iter()
                .filter(|s| s.speaker == *name)
                .map(|s| s.d)
                .sum();
            let label = NSMutableAttributedString::new();
            let piece = |s: &str, a: &NSDictionary<NSAttributedStringKey, AnyObject>| unsafe {
                NSAttributedString::initWithString_attributes(
                    <NSAttributedString as objc2::AnyThread>::alloc(),
                    &NSString::from_str(s),
                    Some(a),
                )
            };
            label.appendAttributedString(&piece(name, &bold));
            label.appendAttributedString(&piece(
                &format!(" {}m{}s", (total / 60.0) as i64, (total as i64) % 60),
                &small,
            ));
            label.drawWithRect_options_context(
                NSRect::new(
                    NSPoint::new(g.origin.x + 20.0, y + 6.0),
                    NSSize::new(GUTTER - 24.0, LANE_H - 8.0),
                ),
                NSStringDrawingOptions::UsesLineFragmentOrigin
                    | NSStringDrawingOptions::TruncatesLastVisibleLine,
                None,
            );
        }
    }
}

fn intersects(a: NSRect, b: NSRect) -> bool {
    a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: &str, t: f64, d: f64, speaker: &str) -> Segment {
        Segment {
            id: id.into(),
            t,
            d,
            speaker: speaker.into(),
            text: String::new(),
            unsure: false,
        }
    }

    #[test]
    fn lays_out_lanes_by_talk_time_and_squeezes_long_silences() {
        let segs = [
            seg("a", 100.0, 5.0, "Dana"),
            seg("b", 103.0, 2.0, "Omer"),
            seg("c", 400.0, 10.0, "Omer"),
        ];
        let l = layout(&segs, 4.0);
        assert_eq!(l.lanes, ["Omer", "Dana"]);
        let x0 = GUTTER + 12.0;
        assert_eq!(l.bars[0].0.origin.x, x0); // a starts the axis
        assert_eq!(l.bars[1].0.origin.x, x0 + 12.0); // b overlaps a, 3s in
        assert_eq!(l.breaks, [(x0 + 20.0 + 4.0, 295.0)]); // 105 -> 400 is squeezed to a break
        assert_eq!(l.bars[2].0.origin.x, x0 + 20.0 + BREAK_W);
        assert_eq!(
            l.spans,
            [(100.0, 105.0, x0), (400.0, 410.0, x0 + 20.0 + BREAK_W)]
        );
        assert_eq!(l.bars[1].0.size.width, 8.0);
        assert_eq!(
            layout(&[seg("x", 1.0, 0.1, "S1")], 4.0).bars[0]
                .0
                .size
                .width,
            3.0
        ); // at least 3px wide
    }
}
