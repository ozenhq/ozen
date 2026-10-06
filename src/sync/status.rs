//! `ozen sync status` (OFE-24): the relay keeps nothing, so Macs catch up only while online together. Each
//! Mac names itself in its hello (protocol.rs); the Mac that hears it notes "last synced with <Mac> at
//! <time>" here, so a user whose Macs are never on at once can see why their transcripts differ.
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs;
use std::sync::Mutex;

/// `{"<device id>": {"name": "...", "at": <unix seconds>}}`: the last hello heard from each other Mac.
pub const MACS: &str = ".sync-macs.json";
/// How often a connection that stays up says hello again (protocol.rs), so the other Macs' "last synced"
/// stays fresh and any drift between the Macs' files is found even without new edits.
pub const HELLO_EVERY: std::time::Duration = std::time::Duration::from_secs(3600);
/// `ozen health` speaks up about a Mac not heard from for this long.
const STALE_DAYS: u64 = 7;
/// The LAN and relay connections run on their own threads: one note at a time.
static WRITING: Mutex<()> = Mutex::new(());

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// This Mac's name as the others list it: its host name without `.local`.
pub fn name() -> String {
    let mut b = [0u8; 256];
    // SAFETY: gethostname writes at most `b.len()` bytes into `b`
    if unsafe { libc::gethostname(b.as_mut_ptr().cast(), b.len()) } != 0 {
        return "Mac".into();
    }
    CStr::from_bytes_until_nul(&b)
        .map(|c| c.to_string_lossy().trim_end_matches(".local").to_string())
        .unwrap_or_else(|_| "Mac".into())
}

/// Each other Mac's (name, last synced).
fn macs() -> BTreeMap<String, (String, u64)> {
    let v: Value = fs::read(MACS)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    v.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(id, m)| Some((id.clone(), (m["name"].as_str()?.into(), m["at"].as_u64()?))))
        .collect()
}

/// Notes that Mac `id`, named `name`, said hello at `at`: the two then exchange what either lacks.
pub(super) fn seen(id: &str, name: &str, at: u64) -> Result<(), String> {
    let _one = WRITING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut all = macs();
    all.insert(id.into(), (name.into(), at));
    let v: BTreeMap<_, _> = all
        .into_iter()
        .map(|(id, (name, at))| (id, json!({"name": name, "at": at})))
        .collect();
    fs::write(MACS, json!(v).to_string()).map_err(|e| format!("{MACS}: {e}"))
}

/// What `ozen sync status` prints at `now`.
pub fn status_at(now: u64) -> Value {
    let sync = if !super::run::configured() {
        "off"
    } else if super::run::on() {
        "on"
    } else {
        "paused"
    };
    let running = sync == "on" && super::run::running();
    // the relay's view, while a runner keeps it current
    let relay: Option<Value> = running
        .then(|| fs::read(super::link::STATUS).ok())
        .flatten()
        .and_then(|b| serde_json::from_slice(&b).ok());
    // ponytail: online now counts Macs on the relay only; Macs on this network alone aren't counted
    // until local.rs tracks its connections by Mac
    let online = relay
        .as_ref()
        .filter(|r| r["connected"] == true)
        .and_then(|r| r["online"].as_u64())
        .map(|n| n.saturating_sub(1));
    // why sync isn't working, while it should: the runner's last error, else the relay's
    let error = (sync == "on")
        .then(|| {
            let ran = (!running).then(|| fs::read_to_string(super::run::ERROR).ok());
            let relay_error = relay
                .as_ref()
                .and_then(|r| r["error"].as_str().map(String::from));
            ran.flatten().map(|e| e.trim().to_string()).or(relay_error)
        })
        .flatten();
    let mut macs: Vec<_> = macs().into_iter().collect();
    macs.sort_by_key(|(_, (_, at))| std::cmp::Reverse(*at));
    let macs: Vec<Value> = macs
        .into_iter()
        .map(|(id, (name, at))| {
            json!({
                "id": id,
                "name": name,
                "last_synced": chrono::DateTime::from_timestamp(at as i64, 0)
                    .map(|t| t.to_rfc3339()),
                "days_ago": now.saturating_sub(at) / 86400,
            })
        })
        .collect();
    json!({
        "sync": sync,
        "running": running,
        "lan_only": std::path::Path::new(super::config::LAN_ONLY).exists(),
        "error": error,
        "relay": relay,
        "other_macs_online": online,
        "macs": macs,
        // a big exchange in progress (progress.rs), while the runner that counts it runs
        "progress": running.then(|| super::progress::read_at(now)).flatten(),
    })
}

pub fn status() -> Value {
    status_at(now())
}

/// One `ozen health` line per other Mac not synced with for `STALE_DAYS`, while sync is set up here.
pub fn health_at(now: u64) -> Vec<String> {
    if !super::run::configured() {
        return vec![];
    }
    macs()
        .into_values()
        .filter_map(|(name, at)| {
            let days = now.saturating_sub(at) / 86400;
            (days >= STALE_DAYS).then(|| {
                format!(
                    "Sync: this Mac last synced with {name} {days} days ago; Macs sync only while both are on and online at the same time"
                )
            })
        })
        .collect()
}

pub fn health() -> Vec<String> {
    health_at(now())
}

#[cfg(test)]
#[path = "status_tests.rs"]
mod tests;
