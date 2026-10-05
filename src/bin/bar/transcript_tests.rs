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
