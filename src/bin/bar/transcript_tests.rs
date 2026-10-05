use super::*;
use serde_json::json;

#[test]
fn a_speaker_talking_on_drops_the_repeated_source() {
    let line = |id: &str, speaker: &str, src: &str| json!({"id": id, "time": "10:00:00", "speaker": speaker, "src": src, "text": id, "heard": id});
    let view = json!({"lines": [
        line("a", "Dana", "call"),
        line("b", "Dana", "call"),
        line("c", "Dana", "room"),
        line("d", "Omer", "room"),
    ]});
    // SAFETY: render only builds attributed strings; it never touches a view.
    let (out, shown) = render(&view, unsafe { MainThreadMarker::new_unchecked() });
    assert_eq!(
        out.string().to_string(),
        "[10:00:00] Dana (call): a\n[10:00:00] Dana: b\n[10:00:00] Dana (room): c\n[10:00:00] Omer (room): d\n"
    );
    assert_eq!(shown.headers["b"], (26 + 11, 4)); // the name is still the tag link
}

#[test]
fn a_new_day_gets_a_line_naming_it() {
    let line = |id: &str, day: &str| json!({"id": id, "time": "10:00:00", "day": day, "speaker": "Dana", "src": "call", "text": id, "heard": id});
    let view = json!({"lines": [line("a", "Sun 04 Oct"), line("b", "Sun 04 Oct"), line("c", "Mon 05 Oct")]});
    // SAFETY: render only builds attributed strings; it never touches a view.
    let (out, shown) = render(&view, unsafe { MainThreadMarker::new_unchecked() });
    assert_eq!(
        out.string().to_string(),
        "Sun 04 Oct\n[10:00:00] Dana (call): a\n[10:00:00] Dana: b\nMon 05 Oct\n[10:00:00] Dana (call): c\n"
    );
    assert_eq!(shown.ids, ["a", "b", "c"]); // day lines aren't transcript lines
}
