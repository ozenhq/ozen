#[test]
fn writes_both_icns_with_every_size() {
    let dir = std::env::temp_dir().join(format!("ozen-icon-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (installer, name) in [(false, "AppIcon.icns"), (true, "Installer.icns")] {
        super::write(&dir, installer).unwrap();
        let icns = std::fs::read(dir.join(name)).unwrap();
        assert_eq!(&icns[..4], b"icns");
        assert!(
            icns.windows(4).any(|w| w == b"ic10"),
            "{name} lacks the 1024px (512@2x) image"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
