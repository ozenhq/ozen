use super::*;

#[test]
fn declares_local_network_access_for_same_network_sync() {
    let p: plist::Dictionary = plist::from_bytes(info_plist("abc1234").as_bytes()).unwrap();
    let s = |k: &str| {
        p.get(k)
            .and_then(plist::Value::as_string)
            .unwrap_or_default()
    };
    assert!(s("NSLocalNetworkUsageDescription").contains("other Macs on this network"));
    let services = p
        .get("NSBonjourServices")
        .and_then(plist::Value::as_array)
        .unwrap();
    assert_eq!(services, &[plist::Value::String(BONJOUR.into())]);
    // the rest is unchanged
    assert_eq!(s("CFBundleIdentifier"), "com.tupe12334.ozen");
    assert_eq!(s("CFBundleShortVersionString"), "abc1234");
    for k in [
        "NSMicrophoneUsageDescription",
        "NSAudioCaptureUsageDescription",
        "NSLocationUsageDescription",
    ] {
        assert!(!s(k).is_empty(), "{k}");
    }
}
