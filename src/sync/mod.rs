//! `ozen sync`: share lines, tags, fixes, places and vocabulary with your other Macs through a relay
//! (ozenhq/sync) that only forwards sealed frames: bucket hashes, summaries of the buckets that differ,
//! then the records the other Mac lacks (protocol.rs, wire.rs, buckets.rs).
pub mod apply;
pub mod buckets;
pub mod config;
pub mod dropped;
pub mod key;
pub mod link;
pub mod local;
pub mod marks;
pub mod off;
pub mod pair;
pub mod parts;
pub mod preview;
pub mod protocol;
pub mod records;
pub mod restore;
pub mod rotate;
pub mod run;
pub mod seal;
pub mod status;
pub mod summaries;
pub mod talk;
pub mod valid;
pub mod version;
pub mod wake;
pub mod wire;

use std::path::Path;

/// `ozen health`'s sync lines: dropped frames from other Macs, and a runner that should run but doesn't.
pub fn health() -> Vec<String> {
    let mut lines = dropped::health();
    lines.extend(run::health());
    lines.extend(link::health());
    lines.extend(status::health());
    lines
}

const USAGE: &str = "\
ozen sync: share lines, tags, fixes, places and vocabulary with your other Macs
  init [--server URL | --lan-only | --relay]
                make this Mac's vault key (kept in the login Keychain; running it again keeps it; if the
                key is lost, it asks you to `join` again instead of leaving your vault) and save
                the relay URL (wss://) after checking it answers; OZEN_SYNC_URL overrides the saved one.
                Turns sync on here: while Ozen.app runs, it keeps `ozen sync run` going.
                --lan-only: never contact any relay, sync only with Macs on the same network;
                --relay: back to the saved relay (no pairing again)
  preview       what turning sync on would share (counts, dates, places) and what it never sends; reads
                files only, connects nowhere
  rotate        a new vault key after losing a Mac: Macs on the old key can't sync with this one; a
                new pair code goes on the clipboard for the Macs you still use (`join --force`)
  off           leave the vault on this Mac: remove its key from the Keychain and the relay URL, stop
                syncing; meetings here stay, and no copy of them is on any server
  undo          put the synced files back as they were before the last big batch from another Mac
                (saving the current ones first, so it can be undone too) and pause sync until `init`
  run           sync with this vault's Macs, on this network and through the relay, until the app stops asking
                (Ozen.app starts it)
  wake          tell a running `sync run` the Mac just woke, so it reconnects now (Ozen.app runs it on wake)
  status        JSON: whether sync is on and running, the relay connection, other Macs online now, and
                when this Mac last synced with each of the others
  pair          copy a pairing code for another Mac to the clipboard (cleared after 2 minutes); the
                key never goes through the relay
  join [--force]
                join the vault of the Mac that ran `pair`: paste its code at the hidden prompt; --force
                replaces a different vault key already on this Mac";

/// `ozen sync init [--server URL]`: makes the vault key on first run (kept after), saves the relay URL.
pub fn init(server: Option<&str>, lan_only: Option<bool>) -> Result<String, String> {
    let k = key::keychain(key::remembered().as_deref())?;
    key::remember(&k)?; // keys made before OFE-77 are noted on the next init
    turn_on()?;
    if let Some(s) = server {
        config::save(s, Path::new(config::FILE))?;
    }
    match lan_only {
        Some(true) => std::fs::write(config::LAN_ONLY, "")
            .map_err(|e| format!("{}: {e}", config::LAN_ONLY))?,
        Some(false) => {
            let _ = std::fs::remove_file(config::LAN_ONLY);
        }
        None => {}
    }
    if Path::new(config::LAN_ONLY).exists() {
        return Ok(format!(
            "vault {}\nLAN only: syncs with this vault's Macs on the same network, never through a relay",
            key::short(&key::vault_id(&k))
        ));
    }
    let url = config::server()?; // OZEN_SYNC_URL still wins over what was just saved
    Ok(report(&key::vault_id(&k), url.as_deref()))
}

/// Sync on here, and not paused (by `ozen sync undo` or `off`): what `init` and `join` both do.
pub(super) fn turn_on() -> Result<(), String> {
    std::fs::write(run::ON, "").map_err(|e| format!("{}: {e}", run::ON))?;
    match std::fs::remove_file(restore::PAUSED) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", restore::PAUSED))
        }
        _ => Ok(()),
    }
}

/// What `init` prints: the vault by its short id only (key::short), and the relay or that sync is off.
fn report(vault: &str, url: Option<&str>) -> String {
    let short = key::short(vault);
    match url {
        Some(u) => format!("vault {short}\nrelay {u}"),
        None => format!(
            "vault {short}\nsync is off: run `ozen sync init --server URL` or set OZEN_SYNC_URL"
        ),
    }
}

/// `ozen sync ...`: prints the result, or exits 1 (2 with its usage on bad arguments).
pub fn cli() {
    let a: Vec<String> = std::env::args().skip(2).collect();
    let out = match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["run"] => {
            if let Err(e) = run::run() {
                eprintln!("{e}");
                std::process::exit(1);
            }
            return;
        }
        ["wake"] => {
            if let Err(e) = wake::touch() {
                eprintln!("{e}");
                std::process::exit(1);
            }
            return;
        }
        ["rotate"] => {
            match rotate::rotate() {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        ["off"] => {
            match off::off() {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        ["undo"] => {
            match restore::undo() {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("{e}");
                    std::process::exit(1);
                }
            }
            return;
        }
        ["preview"] => {
            println!("{}", preview::preview());
            return;
        }
        ["init"] => init(None, None),
        ["init", "--server", url] => init(Some(url), Some(false)),
        ["init", "--lan-only"] => init(None, Some(true)),
        ["init", "--relay"] => init(None, Some(false)),
        ["status"] => Ok(status::status().to_string()),
        ["pair"] => pair::pair(),
        ["join"] => pair::join_prompt(false),
        ["join", "--force"] => pair::join_prompt(true),
        // the detached helper `pair` leaves behind to clear the clipboard
        ["forget-code", n] if n.parse::<isize>().is_ok() => {
            pair::forget_later(n.parse().expect("checked"));
            return;
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    match out {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
pub(super) mod tests;
