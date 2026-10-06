//! `ozen sync pair` / `ozen sync join`: the vault key goes from one Mac to the next by the user's hand,
//! never through the relay, which is why the relay can never read what it forwards. `pair` puts a
//! pairing code (key + relay URL, base32 with a checksum) on the clipboard, marked concealed so
//! clipboard managers skip it, and clears it after two minutes; it never prints the key. `join` reads
//! the code from a hidden prompt (argv would land in shell history), checks it, stores the key and
//! saves the URL.
use super::config;
use super::key::{self, Key};
use data_encoding::BASE32_NOPAD;
use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
use objc2_foundation::{NSData, NSString};
use sha2::{Digest, Sha256};
use std::os::unix::process::CommandExt;
use std::time::{Duration, SystemTime};

const VERSION: u8 = 1;
/// The marker clipboard managers honor to leave an item out of their history (nspasteboard.org).
pub const CONCEALED: &str = "org.nspasteboard.ConcealedType";
/// How long the code stays on the clipboard.
pub const CLEAR_AFTER: Duration = Duration::from_secs(120);

fn checksum(payload: &[u8]) -> [u8; 4] {
    Sha256::digest(payload)[..4].try_into().expect("4 bytes")
}

/// The pairing code for key `k` and relay `url` (empty for a LAN-only vault), in dash-separated groups of
/// four.
pub fn code(k: &Key, url: &str) -> String {
    // holds the key: wiped when dropped, and sized up front so no reallocation leaves a copy behind
    let mut p = zeroize::Zeroizing::new(Vec::with_capacity(1 + 32 + url.len() + 4));
    p.push(VERSION);
    p.extend(k.iter());
    p.extend(url.as_bytes());
    let sum = checksum(&p);
    p.extend(sum);
    let c = BASE32_NOPAD.encode(&p);
    c.as_bytes()
        .chunks(4)
        .map(|g| std::str::from_utf8(g).expect("base32 is ascii"))
        .collect::<Vec<_>>()
        .join("-")
}

/// The key and relay URL in a pairing code; dashes, spaces and case don't matter.
pub fn parse(code: &str) -> Result<(Key, String), String> {
    let bad = "not a pairing code: copy it again with `ozen sync pair` on your other Mac";
    // only base32's letters and digits count: dashes of any kind, spaces and line breaks are layout
    let c: String = code
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_uppercase();
    let p = zeroize::Zeroizing::new(BASE32_NOPAD.decode(c.as_bytes()).map_err(|_| bad)?);
    if p.len() < 1 + 32 + 4 {
        return Err(bad.into());
    }
    let (payload, sum) = p.split_at(p.len() - 4);
    if checksum(payload) != sum {
        return Err("the pairing code has a typo (checksum doesn't match): copy it again".into());
    }
    if payload[0] != VERSION {
        return Err("this pairing code is from a newer ozen: update ozen on this Mac".into());
    }
    let mut k = key::key([0; 32]);
    k.copy_from_slice(&payload[1..33]);
    let url = std::str::from_utf8(&payload[33..]).map_err(|_| bad)?;
    if url.is_empty() {
        return Ok((k, String::new())); // a LAN-only vault: no relay to join
    }
    Ok((k, config::check(url)?))
}

/// Joins the vault in `code`: saves its relay through `save_url` (which checks it answers), then stores
/// its key through `write` (unless `existing` already is that key). The key goes last, so a relay that
/// is down leaves this Mac's key as it was. A different key already here is kept unless `force`.
/// Returns what to print, which holds nothing derived from the key.
pub fn join(
    code: &str,
    force: bool,
    existing: Option<Key>,
    write: impl FnOnce(&Key) -> Result<(), String>,
    save_url: impl FnOnce(&str) -> Result<String, String>,
) -> Result<String, String> {
    let (k, url) = parse(code)?;
    let same = existing.as_ref() == Some(&k);
    if existing.is_some() && !same && !force {
        return Err(
            "this Mac already has a different vault key; `ozen sync join --force` \
                    replaces it, and this Mac then leaves its old vault"
                .into(),
        );
    }
    let url = save_url(&url)?; // checks the relay answers: before anything replaces the key
    if !same {
        write(&k)?;
    }
    if url.is_empty() {
        return Ok(
            "joined: this Mac now syncs with your other Macs on the same network (LAN only)".into(),
        );
    }
    Ok(format!(
        "joined: this Mac now syncs with your other Macs through {url}"
    ))
}

/// Puts `code` on `pb`, marked concealed; returns the pasteboard's change count after.
pub fn put(pb: &NSPasteboard, code: &str) -> Result<isize, String> {
    pb.clearContents();
    let ok = pb.setString_forType(&NSString::from_str(code), unsafe { NSPasteboardTypeString })
        && pb.setData_forType(Some(&NSData::new()), &NSString::from_str(CONCEALED));
    if !ok {
        pb.clearContents();
        return Err("couldn't copy the pairing code to the clipboard".into());
    }
    Ok(pb.changeCount())
}

/// Clears `pb` if nothing was copied since `count` (so a later copy of the user's own stays).
pub fn forget(pb: &NSPasteboard, count: isize) -> bool {
    let same = pb.changeCount() == count;
    if same {
        pb.clearContents();
    }
    same
}

/// `ozen sync pair`: the code on the clipboard, cleared by a detached `ozen sync forget-code`.
pub fn pair() -> Result<String, String> {
    let url = match config::server()? {
        Some(u) => u,
        None if std::path::Path::new(config::LAN_ONLY).exists() => String::new(), // no relay to share
        None => {
            return Err(
                "no relay yet: run `ozen sync init --server URL` (or `--lan-only`) first".into(),
            );
        }
    };
    let k: Key = key::stored()?
        .ok_or_else(|| key::missing("no vault key yet: run `ozen sync init --server URL` first"))?;
    let count = put(&NSPasteboard::generalPasteboard(), &code(&k, &url))?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .args(["sync", "forget-code", &count.to_string()])
        .process_group(0) // outlives a killed terminal or tool's process group
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("can't schedule clearing the clipboard: {e}"))?;
    Ok(
        "pairing code copied to the clipboard (cleared in 2 minutes)\n\
        on your other Mac run `ozen sync join` and paste it"
            .into(),
    )
}

/// `ozen sync forget-code COUNT`: after `CLEAR_AFTER`, clears the clipboard if it still holds the code.
pub fn forget_later(count: isize) {
    // wall-clock deadline: a Mac that sleeps meanwhile doesn't stretch the two minutes
    let deadline = SystemTime::now() + CLEAR_AFTER;
    while SystemTime::now() < deadline {
        std::thread::sleep(Duration::from_secs(5));
    }
    forget(&NSPasteboard::generalPasteboard(), count);
}

/// `ozen sync join [--force]`: reads the code without echoing it.
pub fn join_prompt(force: bool) -> Result<String, String> {
    use std::io::IsTerminal;
    let code = if std::io::stdin().is_terminal() {
        rpassword::prompt_password("pairing code (from `ozen sync pair` on your other Mac): ")
    } else {
        // Ozen.app's Join sends the code on stdin: still never in argv or shell history
        let mut c = String::new();
        std::io::stdin().read_line(&mut c).map(|_| c)
    }
    .map_err(|e| format!("can't read the code: {e}"))?;
    let out = join(&code, force, key::stored()?, key::store, |u| {
        if u.is_empty() {
            // the other Mac is LAN-only: so is this one
            std::fs::write(config::LAN_ONLY, "")
                .map_err(|e| format!("{}: {e}", config::LAN_ONLY))?;
            return Ok(String::new());
        }
        let _ = std::fs::remove_file(config::LAN_ONLY);
        config::save(u, std::path::Path::new(config::FILE))
    })?;
    // the vault this Mac is in now, also when the key was already here (a retry after this failed)
    if let Some(k) = key::stored()? {
        key::remember(&k)?;
    }
    // sync is on here now, as after `init`: Ozen.app starts `ozen sync run`
    super::turn_on()?;
    Ok(out)
}

#[cfg(test)]
#[path = "pair_tests.rs"]
mod tests;
