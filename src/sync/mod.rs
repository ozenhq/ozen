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
pub mod parts;
pub mod preview;
pub mod protocol;
pub mod records;
pub mod restore;
pub mod run;
pub mod seal;
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
    lines
}

const USAGE: &str = "\
ozen sync: share lines, tags, fixes, places and vocabulary with your other Macs
  init [--server URL]
                make this Mac's vault key (kept in the login Keychain; running it again keeps it) and save
                the relay URL (wss://) after checking it answers; OZEN_SYNC_URL overrides the saved one.
                Turns sync on here: while Ozen.app runs, it keeps `ozen sync run` going
  preview       what turning sync on would share (counts, dates, places) and what it never sends; reads
                files only, connects nowhere
  undo          put the synced files back as they were before the last big batch from another Mac
                (saving the current ones first, so it can be undone too) and pause sync until `init`
  run           sync with this vault's Macs on this network until the app stops asking (Ozen.app starts it)";

/// `ozen sync init [--server URL]`: makes the vault key on first run (kept after), saves the relay URL.
pub fn init(server: Option<&str>) -> Result<String, String> {
    let k = key::keychain()?;
    std::fs::write(run::ON, "").map_err(|e| format!("{}: {e}", run::ON))?;
    let _ = std::fs::remove_file(restore::PAUSED); // resumes after `ozen sync undo`
    if let Some(s) = server {
        config::save(s, Path::new(config::FILE))?;
    }
    let url = config::server()?; // OZEN_SYNC_URL still wins over what was just saved
    Ok(report(&key::vault_id(&k), url.as_deref()))
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
    let server = match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["run"] => {
            if let Err(e) = run::run() {
                eprintln!("{e}");
                std::process::exit(1);
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
        ["init"] => None,
        ["init", "--server", url] => Some(url.to_string()),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    };
    match init(server.as_deref()) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
#[path = "mod_tests.rs"]
mod tests;
