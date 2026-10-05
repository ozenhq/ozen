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
use std::time::Duration;

const VERSION: u8 = 1;
/// The marker clipboard managers honor to leave an item out of their history (nspasteboard.org).
pub const CONCEALED: &str = "org.nspasteboard.ConcealedType";
/// How long the code stays on the clipboard.
pub const CLEAR_AFTER: Duration = Duration::from_secs(120);

fn checksum(payload: &[u8]) -> [u8; 4] {
    Sha256::digest(payload)[..4].try_into().expect("4 bytes")
}

/// The pairing code for key `k` and relay `url`, in dash-separated groups of four.
pub fn code(k: &Key, url: &str) -> String {
    let mut p = vec![VERSION];
    p.extend(k);
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
    let c: String = code
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_uppercase();
    let p = BASE32_NOPAD.decode(c.as_bytes()).map_err(|_| bad)?;
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
    let k: Key = payload[1..33].try_into().expect("32 bytes");
    let url = std::str::from_utf8(&payload[33..]).map_err(|_| bad)?;
    Ok((k, config::check(url)?))
}

/// Joins the vault in `code`: stores its key through `write` (unless `existing` already is that key)
/// and saves its relay through `save_url`. A different key already here is kept unless `force`.
/// Returns what to print, which holds nothing derived from the key.
pub fn join(
    code: &str,
    force: bool,
    existing: Option<Key>,
    write: impl FnOnce(&Key) -> Result<(), String>,
    save_url: impl FnOnce(&str) -> Result<String, String>,
) -> Result<String, String> {
    let (k, url) = parse(code)?;
    match existing {
        Some(e) if e == k => {}
        Some(_) if !force => {
            return Err(
                "this Mac already has a different vault key; `ozen sync join --force` \
                        replaces it, and this Mac then leaves its old vault"
                    .into(),
            );
        }
        _ => write(&k)?,
    }
    let url = save_url(&url)?;
    Ok(format!(
        "joined: this Mac now syncs with your other Macs through {url}"
    ))
}

/// Puts `code` on `pb`, marked concealed; returns the pasteboard's change count after.
pub fn put(pb: &NSPasteboard, code: &str) -> isize {
    pb.clearContents();
    pb.setString_forType(&NSString::from_str(code), unsafe { NSPasteboardTypeString });
    pb.setData_forType(Some(&NSData::new()), &NSString::from_str(CONCEALED));
    pb.changeCount()
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
    let url = config::server()?.ok_or("no relay yet: run `ozen sync init --server URL` first")?;
    let k = key::keychain()?;
    let count = put(&NSPasteboard::generalPasteboard(), &code(&k, &url));
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    std::process::Command::new(exe)
        .args(["sync", "forget-code", &count.to_string()])
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
    std::thread::sleep(CLEAR_AFTER);
    forget(&NSPasteboard::generalPasteboard(), count);
}

/// `ozen sync join [--force]`: reads the code without echoing it.
pub fn join_prompt(force: bool) -> Result<String, String> {
    let code =
        rpassword::prompt_password("pairing code (from `ozen sync pair` on your other Mac): ")
            .map_err(|e| format!("can't read the code: {e}"))?;
    join(&code, force, key::stored()?, key::store, |u| {
        config::save(u, std::path::Path::new(config::FILE))
    })
}

#[cfg(test)]
#[path = "pair_tests.rs"]
mod tests;
