//! `ozen sync run`: syncs with the vault's other Macs on this network (local.rs) for as long as someone
//! wants it. Ozen.app runs no work itself: its `ozen status` poll marks that sync is wanted (`ASKED`) and
//! starts this when it isn't running, and this exits soon after the polls stop, so quitting the app
//! stops sync and its Bonjour advertisement.
use super::apply::Coalesced;
use super::key::{self, Key};
use super::local::{self, Local};
use super::protocol::Session;
use super::talk::{Input, talk};
use mdns_sd::IfKind;
use std::fs::{self, File};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Written by `ozen sync init`: this folder syncs. Without it `status` starts nothing.
pub const ON: &str = ".sync-on";

/// Sync is on here: `init` wrote `ON`, or saved a relay URL before `ON` existed.
fn on() -> bool {
    Path::new(ON).exists() || Path::new(super::config::FILE).exists()
}
/// Touched by each `ozen status` poll while sync is on.
pub const ASKED: &str = ".sync-asked";
/// Held by the running `ozen sync run` for as long as it runs: one per folder.
const LOCK: &str = ".sync.lock";
/// When `status` last started it (modified time) and how many starts in a row didn't stay up (contents).
const STARTED: &str = ".sync-started";
/// Why the last `ozen sync run` stopped, for `ozen health`; removed once one stays up.
const ERROR: &str = ".sync-error";
/// After a start, the next may follow this long later, doubling with each start in a row that didn't
/// stay up, to `RETRY_MAX`. A runner that stays up `STAYED` resets the count, so a killed one comes back
/// at the next poll.
const RETRY: Duration = Duration::from_secs(30);
const RETRY_MAX: Duration = Duration::from_secs(600);
const STAYED: Duration = Duration::from_secs(60);
/// It exits once nobody has asked for this long: the app polls every 2s, so quitting it stops sync
/// (and its Bonjour advertisement) within seconds.
const IDLE: Duration = Duration::from_secs(10);
/// How often an idle connection checks for local edits to send.
const TICK: Duration = Duration::from_secs(2);

fn age(f: &str) -> Option<Duration> {
    fs::metadata(f)
        .and_then(|m| m.modified())
        .ok()?
        .elapsed()
        .ok()
}

/// Advertises this Mac on `on` and syncs with every authenticated peer there, each over its own
/// protocol::Session. `after` runs once received records changed something (relearn + retrain);
/// `within` runs each step: the CLI runs it as is, tests switch to a Mac's folder.
pub fn serve(
    key: &Key,
    on: IfKind,
    after: Coalesced,
    within: impl Fn(&mut dyn FnMut()) + Send + Sync + 'static,
) -> Result<Local, String> {
    let (seal, vault) = (key::seal_key(key), key::vault_id(key));
    local::start(key::lan_key(key), on, move |s| {
        let mut session = Session::with(seal, &vault, after.clone());
        let _ = talk(s, TICK, |i| {
            let mut out = Ok(vec![]);
            within(&mut || {
                out = match i {
                    Input::Hello => session.hello(),
                    Input::Frame(f) => session.receive(f),
                    Input::Tick => session.tick(),
                }
            });
            out
        });
    })
}

/// Starts in a row that didn't stay up.
fn failed_starts() -> u32 {
    fs::read_to_string(STARTED)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

/// How long after the last start the next may follow, given `failed` starts in a row before it.
fn retry_after(failed: u32) -> Duration {
    RETRY.saturating_mul(1 << failed.min(5)).min(RETRY_MAX)
}

/// Whether an `ozen sync run` holds the lock here. Takes it for an instant: a runner starting at that
/// moment retries (`run`).
fn running() -> bool {
    File::create(LOCK).is_ok_and(|l| l.try_lock().is_err())
}

/// Called from each `ozen status` poll: whether to start `ozen sync run` now. Marks sync as wanted;
/// true when sync is on, none runs here and the backoff since the last start has passed.
pub fn wanted() -> bool {
    if !on() {
        return false;
    }
    let _ = File::create(ASKED);
    if running() {
        return false;
    }
    let n = failed_starts();
    let due = match age(STARTED) {
        Some(a) if n > 0 => a >= retry_after(n - 1),
        _ => true, // never started, or the last one stayed up: it died or was killed since
    };
    if due {
        let _ = fs::write(STARTED, (n + 1).to_string());
    }
    due
}

/// One line for `ozen health` while the app wants sync (it polls `status`) and none runs, with why the
/// last one stopped. Nothing when sync is off or the app isn't running.
pub fn health() -> Option<String> {
    if !still_wanted() || running() {
        return None;
    }
    let why = fs::read_to_string(ERROR)
        .map(|e| format!(" ({})", e.trim()))
        .unwrap_or_default();
    let next = retry_after(failed_starts().saturating_sub(1)).as_secs();
    Some(format!(
        "Sync with your other Macs isn't running{why}: retrying every {next}s at most (details in start.log)"
    ))
}

/// From each `ozen status` poll: starts `ozen sync run` in its own process group when `wanted`, its
/// output going to `log()` (start.log).
pub fn keep(log: impl Fn() -> File) {
    if !wanted() {
        return;
    }
    let exe = std::env::current_exe().unwrap_or_else(|_| "ozen".into());
    let spawned = Command::new(exe)
        .args(["sync", "run"])
        .stdin(Stdio::null())
        .stdout(log())
        .stderr(log())
        .process_group(0)
        .spawn();
    if let Err(e) = spawned {
        eprintln!("sync: can't start ozen sync run: {e}");
    }
}

/// Sync is on and the app asked within `IDLE`.
fn still_wanted() -> bool {
    on() && age(ASKED).is_some_and(|a| a < IDLE)
}

/// `ozen sync run`: same-network sync until nobody asks for `IDLE` or sync is turned off. An error is
/// kept in `ERROR` for `ozen health`.
pub fn run() -> Result<(), String> {
    // a `status` or `health` poll may hold the lock for an instant: try for a second before deciding
    let lock = File::create(LOCK).map_err(|e| format!("{LOCK}: {e}"))?;
    if !(0..10).any(|_| {
        lock.try_lock().is_ok() || {
            std::thread::sleep(Duration::from_millis(100));
            false
        }
    }) {
        println!("another `ozen sync run` is running in this folder; exiting");
        return Ok(());
    }
    let r = serve_here();
    if let Err(e) = &r {
        let _ = fs::write(ERROR, e);
    }
    r
}

fn serve_here() -> Result<(), String> {
    if !on() {
        return Err("sync is off here: run `ozen sync init` first".into());
    }
    let key = key::stored()?.ok_or("no vault key here: run `ozen sync init` first")?;
    local_network(probe())?;
    // ponytail: every interface (Wi-Fi, Ethernet; loopback and VPNs carry no peers); per-interface
    // choice if one ever leaks the tag where it shouldn't (OFE-83)
    let _local = serve(&key, IfKind::All, Coalesced::retrain(), |step| step())?;
    let _ = File::create(ASKED); // started by hand: counts as asked until `IDLE` passes
    let started = std::time::Instant::now();
    let mut stayed = false;
    while still_wanted() {
        if !stayed && started.elapsed() >= STAYED {
            stayed = true;
            stayed_up();
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Ok(()) // dropping `_local` says goodbye on Bonjour and stops accepting
}

/// Sends one empty mDNS query to the mDNS group. With Ozen's Local Network access denied, macOS fails
/// multicast sends with EHOSTUNREACH (Apple TN3179) while Bonjour itself reports nothing.
fn probe() -> std::io::Result<usize> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0")?;
    s.send_to(&[0; 12], "224.0.0.251:5353") // a DNS header asking nothing
}

/// The probe's result as the reason sync can't run, when it's the Local Network permission.
fn local_network(sent: std::io::Result<usize>) -> Result<(), String> {
    match sent {
        Err(e) if e.raw_os_error() == Some(65) => Err(
            "Local Network access is off: allow Ozen in System Settings > Privacy & Security > Local Network"
                .into(),
        ),
        _ => Ok(()), // no network at all, or another failure: Bonjour retries on its own
    }
}

/// This runner stayed up: clear the failed-start count (keeping the time) and the last error.
fn stayed_up() {
    let _ = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(STARTED)
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"0"));
    let _ = fs::remove_file(ERROR);
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
