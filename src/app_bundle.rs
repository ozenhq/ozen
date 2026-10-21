//! The macOS app bundle and its code signing: builds Ozen.app, signs the recorder and locator binaries with a
//! local identity, so the permissions they are granted survive rebuilds.
use crate::{REC_BIN, REC_BUILT, app_plist, cmd, home, icon, ok};
use std::fs;
use std::path::Path;
use std::process::Stdio;

fn newer(a: &str, b: &str) -> bool {
    let mtime = |p| fs::metadata(p).and_then(|m| m.modified()).ok();
    matches!((mtime(a), mtime(b)), (Some(x), Some(y)) if x > y)
}

/// Local self-signed code-signing identity, kept in its own keychain so it never prompts. macOS ties the
/// Screen Recording and Microphone permissions to a signature's designated requirement: ad hoc that is the
/// binary's hash, so every rebuild lost them; with a certificate it is the certificate, which stays put.
/// The keychain password only guards this throwaway local certificate.
const SIGNING_KEYCHAIN: &str = "Library/Keychains/ozen-signing.keychain-db";
const SIGNING_PASS: &str = "ozen";

fn create_identity(keychain: &str) -> bool {
    let tmp = std::env::temp_dir().join(format!("ozen-signing-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    let (key, cert, p12) = (tmp.join("k.pem"), tmp.join("c.pem"), tmp.join("id.p12"));
    let path = |p: &Path| p.to_string_lossy().into_owned();
    let pass = format!("pass:{SIGNING_PASS}");
    // /usr/bin/openssl (LibreSSL) writes a PKCS#12 that `security import` reads; OpenSSL 3's default doesn't.
    let made = ok(cmd("/usr/bin/openssl")
        .args(["req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650"])
        .args(["-subj", "/CN=Ozen Local Signing", "-addext", "extendedKeyUsage=codeSigning"])
        .args(["-addext", "keyUsage=critical,digitalSignature", "-keyout", &path(&key), "-out", &path(&cert)])
        .stderr(Stdio::null()))
        && ok(cmd("/usr/bin/openssl").args(["pkcs12", "-export", "-inkey", &path(&key), "-in", &path(&cert)])
            .args(["-out", &path(&p12), "-passout", &pass]))
        && ok(cmd("security").args(["create-keychain", "-p", SIGNING_PASS, keychain]))
        && ok(cmd("security").args(["import", &path(&p12), "-k", keychain, "-P", SIGNING_PASS, "-T", "/usr/bin/codesign"])
            .stdout(Stdio::null()))
        // lets codesign use the key without a GUI prompt
        && ok(cmd("security").args(["set-key-partition-list", "-S", "apple-tool:,apple:", "-s", "-k", SIGNING_PASS, keychain])
            .stdout(Stdio::null()));
    let _ = fs::remove_dir_all(&tmp);
    if !made {
        let _ = fs::remove_file(keychain);
    }
    made
}

/// SHA-1 of the signing identity, creating it on first use; None falls back to ad-hoc signing.
fn signing_identity() -> Option<String> {
    let keychain = format!("{}/{SIGNING_KEYCHAIN}", home());
    if !Path::new(&keychain).exists() && !create_identity(&keychain) {
        eprintln!(
            "could not create the local signing identity; signing ad hoc (permissions reset on rebuild)"
        );
        return None;
    }
    // no auto-lock, unlocked, and on the search list: codesign only finds identities there
    let _ = cmd("security")
        .args(["set-keychain-settings", &keychain])
        .status();
    let _ = cmd("security")
        .args(["unlock-keychain", "-p", SIGNING_PASS, &keychain])
        .status();
    let listed = cmd("security")
        .args(["list-keychains", "-d", "user"])
        .output()
        .ok()?;
    let listed = String::from_utf8_lossy(&listed.stdout);
    if !listed.contains(&keychain) {
        let mut all: Vec<&str> = listed
            .split_whitespace()
            .map(|k| k.trim_matches('"'))
            .collect();
        all.push(&keychain);
        let _ = cmd("security")
            .args(["list-keychains", "-d", "user", "-s"])
            .args(all)
            .status();
    }
    let found = cmd("security")
        .args(["find-identity", "-p", "codesigning", &keychain])
        .output()
        .ok()?;
    // self-signed, so it's listed as not trusted; codesign accepts it by hash anyway
    String::from_utf8_lossy(&found.stdout)
        .split_whitespace()
        .find(|w| w.len() == 40 && w.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_string)
}

fn sign(path: &str, deep: bool) {
    let id = signing_identity().unwrap_or_else(|| "-".into());
    let mut c = cmd("codesign");
    c.args(["--force", "-s", &id]);
    if deep {
        c.arg("--deep");
    }
    if !ok(c.arg(path).stderr(Stdio::null())) {
        eprintln!("codesign failed for {path}");
    }
}

/// `cargo build` re-links rec and locate with ad-hoc signatures, which macOS would treat as new apps; copy each
/// to its stable path and re-sign it with the stable identity so its permission (recording, location) carries over.
pub(crate) fn prepare(built: &str, bin: &str) -> bool {
    if !Path::new(built).exists() {
        eprintln!("{built} missing: run `cargo build --release`");
        return false;
    }
    if !newer(bin, built) {
        // Copy and sign in a private dir, then rename into place: concurrent callers (the panel polls `place`)
        // never run or sign a half-copied file. Same file name, since codesign names an unbundled binary by it.
        let bin_path = Path::new(bin);
        let staging = bin_path.with_file_name(format!(".staging-{}", std::process::id()));
        let staged = staging.join(bin_path.file_name().unwrap_or_default());
        let _ = fs::create_dir_all(&staging);
        let copied = fs::copy(built, &staged);
        if let Err(e) = &copied {
            eprintln!("copy {built} to {}: {e}", staged.display());
        } else {
            sign(&staged.to_string_lossy(), false);
            let _ = fs::rename(&staged, bin);
        }
        let _ = fs::remove_dir_all(&staging);
        if copied.is_err() {
            return false;
        }
    }
    let signed = cmd("codesign")
        .args(["-dr", "-", bin])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout).contains("certificate leaf")
                || String::from_utf8_lossy(&o.stderr).contains("certificate leaf")
        });
    if !signed {
        sign(bin, false);
    }
    true
}

/// Before pyproject.toml, uv built a separate env per script path in its cache and never deleted them.
/// Drop our old ones (named after a script, holding mlx_whisper) that no running process uses.
pub(crate) fn drop_script_envs() {
    let Ok(dir) = cmd("uv").args(["cache", "dir"]).output() else {
        return;
    };
    let dir = Path::new(String::from_utf8_lossy(&dir.stdout).trim()).join("environments-v2");
    let Ok(ps) = cmd("ps").args(["-axo", "command"]).output() else {
        return; // can't tell which are in use
    };
    let ps = String::from_utf8_lossy(&ps.stdout);
    for e in fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let whisper = fs::read_dir(e.path().join("lib"))
            .into_iter()
            .flatten()
            .flatten()
            .any(|py| py.path().join("site-packages/mlx_whisper").is_dir());
        if whisper && stale_env(&name, &ps) && fs::remove_dir_all(e.path()).is_ok() {
            println!("removed old env {name}");
        }
    }
}

pub(crate) fn stale_env(name: &str, ps: &str) -> bool {
    let ours = ["asr-", "eval-", "overlap-", "transcribe-"]
        .iter()
        .any(|p| {
            name.strip_prefix(p)
                .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        });
    ours && !ps.contains(&format!("environments-v2/{name}/"))
}

pub(crate) fn prepare_rec() -> bool {
    prepare(REC_BUILT, REC_BIN)
}

/// The menu bar app (src/bin/bar), built by cargo, in an app bundle macOS lists, signs and remembers permissions for.
pub(crate) fn build_app(app: &str) -> bool {
    const BAR_BUILT: &str = "target/release/bar";
    // Already built alongside this CLI by `cargo build --release`; `cargo run -- app` builds only the CLI.
    if !ok(cmd("cargo").args(["build", "--release", "--bin", "bar"]))
        && !Path::new(BAR_BUILT).exists()
    {
        eprintln!("couldn't build the menu bar app (cargo build --release --bin bar)");
        return false;
    }
    let bin = format!("{app}/Contents/MacOS/Ozen");
    if newer(&bin, BAR_BUILT) && newer(&bin, "src/icon.rs") {
        return true;
    }
    let resources = format!("{app}/Contents/Resources");
    for d in [format!("{app}/Contents/MacOS"), resources.clone()] {
        fs::create_dir_all(d).expect("create app bundle");
    }
    // Copied, not linked: the bundle is signed as a whole. Replaced by rename, so a running app keeps its file.
    let staged = format!("{bin}.new");
    if fs::copy(BAR_BUILT, &staged).is_err() || fs::rename(&staged, &bin).is_err() {
        eprintln!("couldn't copy {BAR_BUILT} into {app}");
        return false;
    }
    for page in ["map.html", "chunks.html"] {
        let _ = fs::remove_file(format!("{resources}/{page}")); // the web pages older apps loaded
    }
    if let Err(e) = icon::write(Path::new(&resources), false) {
        eprintln!("icon build failed ({e}); app still works");
    }
    let version = cmd("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or("dev".into(), |o| {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        });
    fs::write(
        format!("{app}/Contents/Info.plist"),
        app_plist::info_plist(&version),
    )
    .expect("write Info.plist");
    sign(app, true); // macOS grants permissions only to signed apps
    // Register with Launch Services + Spotlight so it's findable right away, not after the next index pass.
    let _ = cmd("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
        .args(["-f", app]).status();
    let _ = cmd("mdimport").arg(app).stderr(Stdio::null()).status();
    true
}
