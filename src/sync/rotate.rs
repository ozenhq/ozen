//! `ozen sync rotate`: a new vault key after a Mac is lost, stolen or sold (OFE-19). That Mac still
//! holds the old key and would receive every change made from then on; a new key (so a new vault id,
//! seal key and LAN tag) is the only way to lock it out. Nothing to delete on the relay: it never held
//! anything, and Macs still on the old key just find nobody to talk to. The other Macs join the new
//! vault with the pair code this leaves on the clipboard.
use super::key::{self, Key};
use super::{restore, run};
use security_framework::random::SecRandom;
use std::fs;
use std::time::{Duration, Instant};

/// How long to wait for the running `ozen sync run` (on the old key) to stop.
const STOP: Duration = Duration::from_secs(10);

/// `ozen sync rotate`, storing the new key with `store` and sharing it with `pair`.
pub fn rotate_with(
    old: Key,
    store: impl FnOnce(&Key) -> Result<(), String>,
    pair: impl FnOnce() -> Result<String, String>,
) -> Result<String, String> {
    if !run::configured() {
        return Err("sync isn't set up here: nothing to rotate".into());
    }
    // the new key must be shareable, or no other Mac could follow it (pair.rs `pair` needs either)
    if super::config::server()?.is_none() && !std::path::Path::new(super::config::LAN_ONLY).exists()
    {
        return Err(
            "no relay and not LAN only: run `ozen sync init --server URL` or `--lan-only` first"
                .into(),
        );
    }
    let new = {
        // a batch being merged finishes; the pause stops the running sync, which holds the old key
        let _merging = restore::merging()?;
        fs::write(restore::PAUSED, "").map_err(|e| format!("{}: {e}", restore::PAUSED))?;
        let started = Instant::now();
        while run::running() {
            if started.elapsed() > STOP {
                return Err(
                    "the running sync didn't stop; sync is paused, run `ozen sync rotate` again"
                        .into(),
                );
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let mut new = old;
        while new == old {
            SecRandom::default()
                .copy_bytes(&mut new)
                .map_err(|e| format!("random key: {e}"))?;
        }
        store(&new).map_err(|e| {
            format!("couldn't store the new key ({e}); sync is paused on the old one: run `ozen sync rotate` again")
        })?; // replaces the old key
        super::turn_on()?; // the app starts sync again, on the new key
        new
    };
    let id = key::vault_id(&new);
    let short = key::short(&id);
    let shared = pair().map_err(|e| {
        format!("the key rotated (new vault {short}), but its pair code couldn't be made ({e}); run `ozen sync pair`")
    })?;
    Ok(format!(
        "new vault {short}: Macs on the old key can no longer sync with this one\n{shared}\n\
on each Mac you still use, run `ozen sync join --force` (it replaces their old key) and paste it, soon: \
until then they still sync with the lost Mac"
    ))
}

/// `ozen sync rotate`.
pub fn rotate() -> Result<String, String> {
    let old = key::stored()?.ok_or("no vault key here: nothing to rotate")?;
    rotate_with(old, key::store, super::pair::pair)
}

#[cfg(test)]
#[path = "rotate_tests.rs"]
mod tests;
