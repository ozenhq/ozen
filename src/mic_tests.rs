use super::*;

#[test]
fn names_meeting_apps_by_bundle_prefix() {
    assert_eq!(app_of("us.zoom.xos"), Some("Zoom"));
    assert_eq!(app_of("com.google.Chrome.helper"), Some("Chrome"));
    assert_eq!(app_of("com.apple.avconferenced"), Some("FaceTime"));
    assert_eq!(app_of("com.apple.replayd"), None); // ozen's own capture
    assert_eq!(app_of(""), None);
    assert_eq!(PROCESS_OBJECT_LIST, 0x7072_7323); // 'prs#'
}

#[test]
fn reads_the_process_list_without_crashing() {
    let _ = meeting_app(); // whatever is running on this Mac; must not fail or leak
}
