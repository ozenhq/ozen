//! `ozen sync off`: this Mac leaves the vault (OFE-34). The relay keeps nothing, so leaving is purely
//! local: no copy of the meetings is on any server to erase. What must go is what lets this Mac sync,
//! so it can't rejoin silently: the vault key (Keychain), the relay URL and the switches that turn sync
//! on. The meetings here stay as they are. The other Macs go on syncing with each other; to lock this
//! Mac out of the vault too, re-key it from another one.
use super::{config, restore, run};
use std::fs;

/// `ozen sync off`, removing the key with `forget` (the login Keychain's, or a test's).
pub fn off_with(forget: impl FnOnce() -> Result<(), String>) -> Result<String, String> {
    // a batch from another Mac being merged finishes first; none merges after (`PAUSED`, restore.rs)
    let _merging = restore::merging()?;
    fs::write(restore::PAUSED, "").map_err(|e| format!("{}: {e}", restore::PAUSED))?;
    for f in [run::ON, config::FILE, config::LAN_ONLY] {
        match fs::remove_file(f) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(format!("{f}: {e}")),
            _ => {}
        }
    }
    forget().map_err(|e| {
        format!("sync is off, but the vault key is still in the Keychain ({e}); run `ozen sync off` again")
    })?;
    Ok("sync is off on this Mac: its vault key and relay are gone, and the running sync stops within \
a second. Your meetings here are untouched, and no copy of them is on any server; what sync kept here \
(restore points in .sync-restore/) stays with them. To sync again, `ozen sync join` from another Mac \
or `ozen sync init`."
        .into())
}

/// `ozen sync off`.
pub fn off() -> Result<String, String> {
    off_with(super::key::forget)
}

#[cfg(test)]
#[path = "off_tests.rs"]
mod tests;
