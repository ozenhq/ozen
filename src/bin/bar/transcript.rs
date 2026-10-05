//! The panel's transcript, drawn from `ozen transcript` (src/panel.rs decides what it says).
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, msg_send};
use objc2_app_kit::{
    NSColor, NSFont, NSFontAttributeName, NSFontWeightRegular, NSForegroundColorAttributeName,
    NSLinkAttributeName, NSMutableParagraphStyle, NSParagraphStyle, NSParagraphStyleAttributeName,
    NSTextAlignment, NSTextView, NSToolTipAttributeName, NSWritingDirection,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSAttributedStringKey, NSDictionary,
    NSMutableAttributedString, NSRange, NSString, NSURL,
};
use serde_json::Value;
use std::collections::HashMap;

/// What was drawn, for clicks, Review and the render check.
#[derive(Default)]
pub struct Shown {
    pub headers: HashMap<String, (usize, usize)>, // line id -> speaker name range (UTF-16)
    pub heard: HashMap<String, String>,           // what ozen heard, before your fixes
    pub ids: Vec<String>,
    pub review: Vec<(String, f64)>, // unsure lines shown, most uncertain first, and when each ages out
    pub footer: String,
    pub segments: Vec<crate::timeline::Segment>, // the timeline's bars
}

fn attrs(
    pairs: &[(&NSAttributedStringKey, &AnyObject)],
) -> Retained<NSDictionary<NSAttributedStringKey, AnyObject>> {
    let (keys, values): (Vec<&NSAttributedStringKey>, Vec<&AnyObject>) =
        pairs.iter().copied().unzip();
    NSDictionary::from_slices(&keys, &values)
}

fn append(
    out: &NSMutableAttributedString,
    s: &str,
    a: &NSDictionary<NSAttributedStringKey, AnyObject>,
) {
    let piece = unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(s),
            Some(a),
        )
    };
    out.appendAttributedString(&piece);
}

/// Swift's description of a Double: shortest round trip, always with a fractional part.
pub fn swift_double(v: f64) -> String {
    format!("{v:?}")
}

pub fn render(
    view: &Value,
    _mtm: MainThreadMarker,
) -> (Retained<NSMutableAttributedString>, Shown) {
    let out = NSMutableAttributedString::new();
    let mut shown = Shown::default();
    // SAFETY: the attribute key statics are initialized by AppKit.
    let (k_para, k_color, k_font, k_link, k_tip) = unsafe {
        (
            NSParagraphStyleAttributeName,
            NSForegroundColorAttributeName,
            NSFontAttributeName,
            NSLinkAttributeName,
            NSToolTipAttributeName,
        )
    };
    let lines = view["lines"].as_array().cloned().unwrap_or_default();
    if lines.is_empty() {
        let grey = NSColor::secondaryLabelColor();
        append(
            &out,
            "No transcript yet. Press Start.",
            &attrs(&[(k_color, &grey)]),
        );
    }
    let (tertiary, secondary, orange, label) = (
        NSColor::tertiaryLabelColor(),
        NSColor::secondaryLabelColor(),
        NSColor::systemOrangeColor(),
        NSColor::labelColor(),
    );
    let digits =
        NSFont::monospacedDigitSystemFontOfSize_weight(11.0, unsafe { NSFontWeightRegular });
    let small_bold = NSFont::boldSystemFontOfSize(11.0);
    let (bold, small, body) = (
        NSFont::boldSystemFontOfSize(12.0),
        NSFont::systemFontOfSize(11.0),
        NSFont::systemFontOfSize(13.0),
    );
    let mut prev = None; // the line above's header and source
    let mut day = String::new();
    for l in &lines {
        let s = |k: &str| l[k].as_str().unwrap_or("").to_string();
        // the times have no date: a line naming the day wherever it changes
        if s("day") != day {
            day = s("day");
            prev = None;
            let para = NSMutableParagraphStyle::new();
            para.setParagraphSpacing(4.0);
            para.setParagraphSpacingBefore(if out.length() == 0 { 0.0 } else { 8.0 });
            append(
                &out,
                &format!("{day}\n"),
                &attrs(&[
                    (k_para, &para),
                    (k_color, &secondary),
                    (k_font, &small_bold),
                ]),
            );
        }
        let (id, said, heard) = (s("id"), s("text"), s("heard"));
        let ignored = l["ignored"].as_bool().unwrap_or(false);
        let para = NSMutableParagraphStyle::new();
        para.setParagraphSpacing(6.0);
        if l["rtl"].as_bool().unwrap_or(false) {
            para.setBaseWritingDirection(NSWritingDirection::RightToLeft);
            para.setAlignment(NSTextAlignment::Right);
        }
        let para: &NSParagraphStyle = &para;
        append(
            &out,
            &format!("[{}] ", s("time")),
            &attrs(&[(k_para, para), (k_color, &tertiary), (k_font, &digits)]),
        );
        let mark = s("mark");
        let header = if mark.is_empty() {
            s("speaker")
        } else {
            format!("{} {mark}", s("speaker"))
        };
        let at = out.length();
        shown
            .headers
            .insert(id.clone(), (at, NSString::from_str(&header).length()));
        let tag_url =
            NSURL::URLWithString(&NSString::from_str(&format!("ozen://tag/{id}"))).unwrap();
        // the same voice talking on: its name dimmed and no source, so speaker changes stand out
        let this = Some((header.clone(), s("src")));
        let same = prev == this;
        prev = this;
        // unsure lines are what the loop wants tagged next
        let header_color = if l["unsure"].as_bool().unwrap_or(false) {
            &orange
        } else if same {
            &tertiary
        } else {
            &secondary
        };
        append(
            &out,
            &header,
            &attrs(&[
                (k_para, para),
                (k_color, header_color),
                (k_font, &bold),
                (k_link, &tag_url),
            ]),
        );
        append(
            &out,
            &if same {
                ": ".to_string()
            } else {
                format!(" ({}): ", s("src"))
            },
            &attrs(&[(k_para, para), (k_color, &tertiary), (k_font, &small)]),
        );
        let fix_url =
            NSURL::URLWithString(&NSString::from_str(&format!("ozen://fix/{id}"))).unwrap();
        let tip = NSString::from_str(&if said == heard {
            "Click to fix the text".to_string()
        } else {
            format!("Fixed. Heard: {heard}")
        });
        let (font, color) = if ignored {
            (&small, &tertiary)
        } else {
            (&body, &label)
        };
        append(
            &out,
            &said,
            &attrs(&[
                (k_para, para),
                (k_color, color),
                (k_font, font),
                (k_link, &fix_url),
                (k_tip, &tip),
            ]),
        );
        append(&out, "\n", &attrs(&[(k_para, para), (k_color, &label)]));
        shown.heard.insert(id.clone(), heard);
        shown.ids.push(id);
    }
    shown.review = view["review"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|r| {
            (
                r["id"].as_str().unwrap_or("").to_string(),
                r["until"].as_f64().unwrap_or(0.0),
            )
        })
        .collect();
    shown.footer = view["footer"].as_str().unwrap_or("").to_string();
    shown.segments = view["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| crate::timeline::Segment {
            id: g["id"].as_str().unwrap_or("").into(),
            t: g["t"].as_f64().unwrap_or(0.0),
            d: g["d"].as_f64().unwrap_or(1.0),
            speaker: g["speaker"].as_str().unwrap_or("?").into(),
            text: g["text"].as_str().unwrap_or("").into(),
            unsure: g["unsure"].as_bool().unwrap_or(false),
        })
        .collect();
    (out, shown)
}

/// Swift's String.debugDescription.
fn debug(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\'' => o.push_str("\\'"),
            '\n' => o.push_str("\\n"),
            '\r' => o.push_str("\\r"),
            '\t' => o.push_str("\\t"),
            '\0' => o.push_str("\\0"),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Every attribute run of the text view, as the Swift render check prints them.
pub fn dump_runs(text: &NSTextView) -> Vec<String> {
    let mut out = vec![];
    // SAFETY: reading the text view's own storage and the AppKit attribute key statics.
    let storage = unsafe { text.textStorage() }.unwrap();
    let (k_para, k_color, k_font, k_link, k_tip) = unsafe {
        (
            NSParagraphStyleAttributeName,
            NSForegroundColorAttributeName,
            NSFontAttributeName,
            NSLinkAttributeName,
            NSToolTipAttributeName,
        )
    };
    let whole = storage.string();
    let len = storage.length();
    let mut at = 0;
    while at < len {
        let mut range = NSRange::new(0, 0);
        let a = unsafe { storage.attributesAtIndex_effectiveRange(at, &mut range) };
        let s = whole.substringWithRange(range).to_string();
        let get = |k: &NSAttributedStringKey| a.objectForKey(k);
        let link = get(k_link)
            .and_then(|u| u.downcast::<NSURL>().ok())
            .and_then(|u| u.absoluteString())
            .map_or(String::new(), |s| s.to_string());
        let color = get(k_color).map_or(String::new(), |c| {
            // SAFETY: every object answers -description with an NSString.
            let d: Retained<NSString> = unsafe { msg_send![&*c, description] };
            d.to_string()
        });
        let font = get(k_font)
            .and_then(|f| f.downcast::<NSFont>().ok())
            .map_or(String::new(), |f| {
                format!("{} {}", f.fontName(), swift_double(f.pointSize()))
            });
        let para = get(k_para).and_then(|p| p.downcast::<NSParagraphStyle>().ok());
        let (dir, align, spacing) = para.map_or(("-9".into(), "-9".into(), "-1".into()), |p| {
            (
                p.baseWritingDirection().0.to_string(),
                p.alignment().0.to_string(),
                swift_double(p.paragraphSpacing()),
            )
        });
        let tip = get(k_tip)
            .and_then(|t| t.downcast::<NSString>().ok())
            .map_or(String::new(), |t| t.to_string());
        out.push(format!(
            "{}|{link}|{color}|{font}|{dir}|{align}|{spacing}|{tip}",
            debug(&s)
        ));
        at = range.location + range.length;
    }
    out
}

#[cfg(test)]
#[path = "transcript_tests.rs"]
mod tests;
