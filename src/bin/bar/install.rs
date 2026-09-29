//! Ozen.app from the DMG carries a prebuilt checkout in Resources/ozen (.github/workflows/release.yml): copy it into
//! ~/ozen on first launch and after an update. Only the files it ships are overwritten; recordings and tags stay.
//! A git checkout there is a developer's, built with `ozen app`: leave it alone.
use std::path::Path;
use std::process::Command;

fn version(d: &Path) -> Option<String> {
    std::fs::read_to_string(d.join(".version")).ok()
}

pub fn bundled(src: &Path, dest: &Path) {
    let Some(v) = version(src) else { return };
    if dest.join(".git").exists() || version(dest).as_ref() == Some(&v) {
        return;
    }
    let _ = std::fs::create_dir_all(dest);
    // A downloaded app's files are quarantined, which would block its CLI; .version goes last, so a copy cut short
    // is retried on the next launch.
    let copied = Command::new("/usr/bin/rsync")
        .args(["-a", "--exclude=.version"])
        .arg(format!("{}/", src.display()))
        .arg(format!("{}/", dest.display()))
        .status()
        .is_ok_and(|s| s.success());
    if !copied {
        return;
    }
    if Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(dest.join("target"))
        .status()
        .is_err()
    {
        return;
    }
    let _ = std::fs::write(dest.join(".version"), v);
}

#[cfg(test)]
mod tests {
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
}
