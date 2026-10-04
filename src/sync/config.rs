//! Where the relay is: `OZEN_SYNC_URL`, else the URL `ozen sync init --server` saved, else sync is off.
use std::path::Path;
use std::process::Command;

/// The saved relay URL, in the ozen folder (unlike the key, it's no secret).
pub const FILE: &str = ".sync-url";

/// The URL if the token may go there: wss, or ws to this Mac only (tests, a local relay), so the
/// bearer token never crosses the network in the clear. No user, password, query or fragment: the
/// relay's paths are appended to it. Trailing slashes dropped.
pub fn check(raw: &str) -> Result<String, String> {
    let u = url::Url::parse(raw.trim()).map_err(|e| format!("bad sync URL {raw}: {e}"))?;
    let local = matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match u.scheme() {
        "wss" => {}
        "ws" if local => {}
        _ => return Err(format!("sync URL must be wss://: {raw}")),
    }
    if !u.username().is_empty()
        || u.password().is_some()
        || u.query().is_some()
        || u.fragment().is_some()
    {
        return Err(format!(
            "sync URL must be just wss://host[:port][/path]: {raw}"
        ));
    }
    Ok(u.as_str().trim_end_matches('/').into())
}

/// The relay to sync with, or None when sync is off.
pub fn resolve(env: Option<String>, saved: &Path) -> Result<Option<String>, String> {
    if let Some(e) = env.filter(|e| !e.trim().is_empty()) {
        return check(&e).map(Some);
    }
    match std::fs::read_to_string(saved) {
        Ok(s) if !s.trim().is_empty() => check(&s).map(Some),
        _ => Ok(None),
    }
}

/// The relay this Mac syncs with, or None when sync is off.
pub fn server() -> Result<Option<String>, String> {
    resolve(std::env::var("OZEN_SYNC_URL").ok(), Path::new(FILE))
}

/// The relay's `/health` (plain HTTP on the same host and path) answers "ok", so a typo can't leave
/// sync silently dead. `url` is a checked one.
pub fn healthy(url: &str) -> Result<(), String> {
    let http = url.replacen("ws", "http", 1); // wss:// → https://, ws:// → http://
    let out = Command::new("curl")
        .args(["-fsSg", "--max-time", "10", &format!("{http}/health")]) // -g: [::1] is a host, not a glob
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    match out.status.success() && out.stdout.trim_ascii() == b"ok" {
        true => Ok(()),
        false => Err(format!(
            "no sync relay answering at {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
    }
}

/// Checks `raw` and that its relay answers, then saves it to `saved`; returns the saved URL.
pub fn save(raw: &str, saved: &Path) -> Result<String, String> {
    let url = check(raw)?;
    healthy(&url)?;
    std::fs::write(saved, format!("{url}\n")).map_err(|e| e.to_string())?;
    Ok(url)
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
