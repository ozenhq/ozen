//! Where the relay is: `OZEN_SYNC_URL`, else the URL `ozen sync init --server` saved, else sync is off.
use std::path::Path;
use std::time::Duration;

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
    pick(
        Path::new(LAN_ONLY).exists(),
        std::env::var("OZEN_SYNC_URL").ok(),
        Path::new(FILE),
    )
}

/// Written by `ozen sync init --lan-only`: never contact any relay, whatever URL is saved or set, and
/// sync only with this vault's Macs on the same network (local.rs). `ozen sync init --relay` removes it;
/// the saved URL and the key stay, so the relay comes back without pairing again.
pub const LAN_ONLY: &str = ".sync-lan-only";

/// The relay to use: none in LAN-only mode, else `resolve`'s.
pub fn pick(lan_only: bool, env: Option<String>, saved: &Path) -> Result<Option<String>, String> {
    if lan_only {
        return Ok(None);
    }
    resolve(env, saved)
}

/// The relay's `/health` (plain HTTP on the same host and path) answers "ok", so a typo can't leave
/// sync silently dead. `url` is a checked one. Rust's own HTTP client (rustls, with the Mac's trust
/// store for https), no `curl` subprocess: nothing depends on PATH or parses another program's output.
pub fn healthy(url: &str) -> Result<(), String> {
    healthy_within(url, Duration::from_secs(10))
}

fn healthy_within(url: &str, wait: Duration) -> Result<(), String> {
    let http = url.replacen("ws", "http", 1); // wss:// → https://, ws:// → http://
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(ureq::tls::RootCerts::PlatformVerifier)
        .build();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .tls_config(tls)
        .timeout_global(Some(wait))
        .max_redirects(0) // as curl did: a URL that redirects elsewhere isn't the relay
        .build()
        .into();
    let fail = |e: String| format!("no sync relay answering at {url}: {e}");
    let mut res = agent
        .get(format!("{http}/health"))
        .call()
        .map_err(|e| fail(e.to_string()))?;
    let body = res.body_mut().with_config().limit(1024).read_to_string();
    match body.map_err(|e| fail(e.to_string()))?.trim() {
        "ok" => Ok(()),
        other => Err(fail(format!("/health said {other:?}, not ok"))),
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
