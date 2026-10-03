use super::*;

#[test]
fn copies_a_new_version_but_never_over_a_git_checkout() {
    let root = std::env::temp_dir().join(format!("ozen-install-{}", std::process::id()));
    let (src, dest) = (root.join("Resources/ozen"), root.join("home/ozen"));
    std::fs::create_dir_all(src.join("target/release")).unwrap();
    std::fs::write(src.join("target/release/ozen"), "bin").unwrap();
    std::fs::write(src.join(".version"), "1.0").unwrap();
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("lines.jsonl"), "mine").unwrap();
    bundled(&src, &dest);
    assert_eq!(
        std::fs::read_to_string(dest.join("target/release/ozen")).unwrap(),
        "bin"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join(".version")).unwrap(),
        "1.0"
    );
    assert_eq!(
        std::fs::read_to_string(dest.join("lines.jsonl")).unwrap(),
        "mine"
    ); // recordings stay
    std::fs::write(src.join("target/release/ozen"), "newer").unwrap(); // rsync -a compares size and mtime
    bundled(&src, &dest); // same version: nothing to do
    assert_eq!(
        std::fs::read_to_string(dest.join("target/release/ozen")).unwrap(),
        "bin"
    );
    std::fs::write(src.join(".version"), "1.1").unwrap();
    std::fs::create_dir_all(dest.join(".git")).unwrap();
    bundled(&src, &dest); // a developer's checkout: left alone
    assert_eq!(
        std::fs::read_to_string(dest.join("target/release/ozen")).unwrap(),
        "bin"
    );
    std::fs::remove_dir_all(dest.join(".git")).unwrap();
    bundled(&src, &dest);
    assert_eq!(
        std::fs::read_to_string(dest.join("target/release/ozen")).unwrap(),
        "newer"
    );
    let _ = std::fs::remove_dir_all(&root);
}
