//! `ozen sync`: share lines, tags, fixes, places and vocabulary with your other Macs through a relay
//! (ozenhq/sync) that only sees encrypted ops.
pub mod config;
pub mod key;

use std::path::Path;

const USAGE: &str = "\
ozen sync: share lines, tags, fixes, places and vocabulary with your other Macs
  init [--server URL]
                make this Mac's vault key (kept in the login Keychain; running it again keeps it) and save
                the relay URL (wss://) after checking it answers; OZEN_SYNC_URL overrides the saved one";

/// `ozen sync init [--server URL]`: makes the vault key on first run (kept after), saves the relay URL.
pub fn init(server: Option<&str>) -> Result<String, String> {
    let k = key::keychain()?;
    if let Some(s) = server {
        config::save(s, Path::new(config::FILE))?;
    }
    let url = config::server()?; // OZEN_SYNC_URL still wins over what was just saved
    let id = key::vault_id(&k);
    Ok(match url {
        Some(u) => format!("vault {id}\nrelay {u}"),
        None => format!(
            "vault {id}\nsync is off: run `ozen sync init --server URL` or set OZEN_SYNC_URL"
        ),
    })
}

/// `ozen sync ...`: prints the result, or exits 1 (2 with its usage on bad arguments).
pub fn cli() {
    let a: Vec<String> = std::env::args().skip(2).collect();
    let server = match a.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
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
