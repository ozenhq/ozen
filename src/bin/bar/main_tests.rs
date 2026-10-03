use super::transcribe_switched;

#[test]
fn transcribe_switch_stops_or_catches_up() {
    assert_eq!(
        transcribe_switched(false, "recording", 0),
        [&["record"][..], &["process", "stop"]]
    );
    assert_eq!(
        transcribe_switched(false, "processing", 3),
        [&["process", "stop"][..]]
    );
    assert!(transcribe_switched(false, "stopped", 3).is_empty());
    assert_eq!(transcribe_switched(true, "recording", 3), [&["start"][..]]);
    assert_eq!(transcribe_switched(true, "stopped", 3), [&["process"][..]]);
    assert!(transcribe_switched(true, "stopped", 0).is_empty());
}
