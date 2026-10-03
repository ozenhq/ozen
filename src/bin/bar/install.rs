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
#[path = "install_tests.rs"]
mod tests;
