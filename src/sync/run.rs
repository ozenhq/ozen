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
/// Touched by each `ozen status` poll while sync is on.
pub const ASKED: &str = ".sync-asked";
/// Held by the running `ozen sync run` for as long as it runs: one per folder.
const LOCK: &str = ".sync.lock";
/// When `status` last started it, so one that dies on start isn't respawned every poll.
const STARTED: &str = ".sync-started";
/// It exits once nobody has asked for this long (the app polls every 2s).
const IDLE: Duration = Duration::from_secs(60);
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

/// Called from each `ozen status` poll: whether to start `ozen sync run` now. Marks sync as wanted;
/// true when sync is on, none runs here and none was started in the last minute.
pub fn wanted() -> bool {
    if !Path::new(ON).exists() {
        return false;
    }
    let _ = File::create(ASKED);
    let free = File::create(LOCK).is_ok_and(|l| l.try_lock().is_ok()); // released as `l` drops
    let paced = age(STARTED).is_none_or(|a| a > IDLE);
    if free && paced {
        let _ = File::create(STARTED);
    }
    free && paced
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

/// `ozen sync run`: same-network sync until nobody asks for a minute or sync is turned off.
pub fn run() -> Result<(), String> {
    let lock = File::create(LOCK).map_err(|e| format!("{LOCK}: {e}"))?;
    if lock.try_lock().is_err() {
        println!("another `ozen sync run` is running in this folder; exiting");
        return Ok(());
    }
    if !Path::new(ON).exists() {
        return Err("sync is off here: run `ozen sync init` first".into());
    }
    let key = key::stored()?.ok_or("no vault key here: run `ozen sync init` first")?;
    // ponytail: every interface (Wi-Fi, Ethernet; loopback and VPNs carry no peers); per-interface
    // choice if one ever leaks the tag where it shouldn't (OFE-83)
    let _local = serve(&key, IfKind::All, Coalesced::retrain(), |step| step())?;
    let _ = File::create(ASKED); // started by hand: the first minute counts as asked
    while Path::new(ON).exists() && age(ASKED).is_some_and(|a| a < IDLE) {
        std::thread::sleep(Duration::from_secs(5));
    }
    Ok(()) // dropping `_local` says goodbye on Bonjour and stops accepting
}

#[cfg(test)]
#[path = "run_tests.rs"]
mod tests;
