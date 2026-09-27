//! ozen control. Owns the recorder and transcriber processes; the menu bar app only asks it.
use std::fs::{self, File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio, exit};
use std::thread::sleep;
use std::time::Duration;

const USAGE: &str = "\
ozen control: start | pause | resume | stop | status | health | look | app | bar
  start/resume  record + transcribe
  pause         stop recording; transcriber stays loaded so resume is instant
  stop          stop recording, finish transcribing what's queued, then exit
  status        prints recording | paused | stopping | stopped
  look [N]      screenshot to screen-small.png and print the last N transcript lines (default 40)
  health        prints one line per problem (recording blocked or on hold, silent mic, transcriber down or behind)
  app           build Ozen.app into ~/Applications (open it from Spotlight/Launchpad)
  bar           build if needed and open Ozen.app (its buttons call this binary)";

const REC_BIN: &str = "target/release/rec"; // src/bin/rec.rs, built by cargo alongside this CLI
const REC: &str = r"^target/release/rec chunks"; // anchored so pgrep never matches shells that merely mention the command
const TR: &str = r"uv run transcribe\.py chunks|python3 transcribe\.py chunks";
const DRAIN: &str = r"/ozen drain$"; // the detached helper `stop` leaves behind
const BLOCKED: &str = "declined TCCs"; // ScreenCaptureKit's error when the recording permission is missing

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

/// Launched from Ozen.app the PATH is launchd's minimal one: add where uv and ffmpeg usually live.
fn cmd(program: &str) -> Command {
    let h = home();
    let path = format!(
        "{h}/.local/bin:{h}/.cargo/bin:/opt/homebrew/bin:/usr/local/bin:{}",
        std::env::var("PATH").unwrap_or_default()
    );
    let mut c = Command::new(program);
    c.env("PATH", path);
    c
}

fn ok(c: &mut Command) -> bool {
    c.status().is_ok_and(|s| s.success())
}

fn running(pattern: &str) -> bool {
    ok(cmd("pgrep").args(["-qf", pattern]))
}

fn signal(sig: &str, pattern: &str) {
    let _ = cmd("pkill").args([sig, "-f", pattern]).status();
}

fn log() -> File {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open("start.log")
        .expect("open start.log")
}

/// Own process group, so it outlives whoever asked (a closing shell or the app).
fn spawn_detached(c: &mut Command, out: Stdio, err: Stdio) {
    if let Err(e) = c
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .process_group(0)
        .spawn()
    {
        eprintln!("failed to start {c:?}: {e}");
        exit(1);
    }
}

fn newer(a: &str, b: &str) -> bool {
    let mtime = |p| fs::metadata(p).and_then(|m| m.modified()).ok();
    matches!((mtime(a), mtime(b)), (Some(x), Some(y)) if x > y)
}

fn chunks_waiting() -> usize {
    fs::read_dir("chunks").map_or(0, |d| {
        d.flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "wav"))
            .count()
    })
}

/// A rebuild re-signs rec ad hoc, which can revoke the permission. The transcriber logs to the same
/// file, so judge by the recorder's last start event, not the last line.
fn recorder_blocked(log: &str) -> bool {
    log.lines()
        .rfind(|l| l.contains(BLOCKED) || l.starts_with("recording to"))
        .is_some_and(|l| l.contains(BLOCKED))
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

const TR_STARTED: &str = ".transcriber-started"; // when it was last launched, to pace automatic restarts

fn start_transcriber() {
    if !ok(cmd("git")
        .args(["-C", "voices", "pull", "-q", "--ff-only"])
        .stderr(Stdio::null()))
    {
        let _ = std::io::Write::write_all(
            &mut log(),
            b"voices registry pull failed; using local copy\n",
        );
    }
    let _ = File::create(TR_STARTED);
    spawn_detached(
        cmd("uv").args(["run", "transcribe.py", "chunks", "transcript.txt"]),
        log().into(),
        log().into(),
    );
}

/// While recording, a transcriber that died gets restarted. The app polls `status` every 2s, so this is the
/// supervisor. At most once a minute, so one that crashes on start doesn't respawn in a tight loop.
fn restart_dead_transcriber() {
    let recent = fs::metadata(TR_STARTED)
        .and_then(|m| m.modified())
        .is_ok_and(|t| t.elapsed().is_ok_and(|e| e < Duration::from_secs(60)));
    if !recent && !running(TR) {
        let _ = std::io::Write::write_all(
            &mut log(),
            b"transcriber not running while recording; restarting it\n",
        );
        start_transcriber();
    }
}

/// `cargo build` re-links rec with an ad-hoc signature, which macOS would treat as a new app; re-sign it
/// with the stable identity so its recording permission carries over.
fn prepare_rec() -> bool {
    if !Path::new(REC_BIN).exists() {
        eprintln!("{REC_BIN} missing: run `cargo build --release`");
        return false;
    }
    let signed = cmd("codesign")
        .args(["-dr", "-", REC_BIN])
        .output()
        .is_ok_and(|o| {
            String::from_utf8_lossy(&o.stdout).contains("certificate leaf")
                || String::from_utf8_lossy(&o.stderr).contains("certificate leaf")
        });
    if !signed {
        sign(REC_BIN, false);
    }
    true
}

fn build_app(app: &str) -> bool {
    let bin = format!("{app}/Contents/MacOS/Ozen");
    if newer(&bin, "menubar.swift") && newer(&bin, "icon.swift") {
        return true;
    }
    let resources = format!("{app}/Contents/Resources");
    for d in [format!("{app}/Contents/MacOS"), resources.clone()] {
        fs::create_dir_all(d).expect("create app bundle");
    }
    if !ok(cmd("swiftc").args(["-O", "menubar.swift", "-o", &bin])) {
        return false;
    }
    if !ok(cmd("swift").args(["icon.swift", &resources])) {
        eprintln!("icon build failed; app still works");
    }
    let version = cmd("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map_or("dev".into(), |o| {
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        });
    fs::write(format!("{app}/Contents/Info.plist"), format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Ozen</string>
  <key>CFBundleDisplayName</key><string>Ozen</string>
  <key>CFBundleIdentifier</key><string>com.tupe12334.ozen</string>
  <key>CFBundleExecutable</key><string>Ozen</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleShortVersionString</key><string>{version}</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>LSUIElement</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Ozen transcribes what you say in meetings, on this Mac only.</string>
  <key>NSAudioCaptureUsageDescription</key><string>Ozen transcribes the meeting audio, on this Mac only.</string>
</dict></plist>
"#)).expect("write Info.plist");
    sign(app, true); // macOS grants permissions only to signed apps
    // Register with Launch Services + Spotlight so it's findable right away, not after the next index pass.
    let _ = cmd("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
        .args(["-f", app]).status();
    let _ = cmd("mdimport").arg(app).stderr(Stdio::null()).status();
    true
}

fn main() {
    // The repo is where the sources, chunks and logs live, wherever this is called from.
    std::env::set_current_dir(env!("CARGO_MANIFEST_DIR")).expect("cd to the ozen checkout");
    let app = format!("{}/Applications/Ozen.app", home());
    match std::env::args().nth(1).as_deref().unwrap_or("") {
        "start" | "resume" => {
            if !prepare_rec() {
                exit(1);
            }
            fs::create_dir_all("chunks").expect("create chunks/");
            if !running(TR) {
                start_transcriber();
            }
            if !running(REC) {
                spawn_detached(cmd(REC_BIN).arg("chunks"), log().into(), log().into());
            }
        }
        "pause" => signal("-INT", REC), // SIGINT: recorder flushes its current chunk first
        "stop" => {
            signal("-INT", REC);
            File::create(".stopping").expect("create .stopping");
            let me = std::env::current_exe().expect("own path");
            spawn_detached(Command::new(me).arg("drain"), Stdio::null(), Stdio::null());
        }
        // Let the transcriber drain queued chunks (max 2 min) so the last words aren't lost.
        "drain" => {
            for _ in 0..120 {
                if chunks_waiting() == 0 {
                    break;
                }
                sleep(Duration::from_secs(1));
            }
            signal("-TERM", TR);
            let _ = fs::remove_file(".stopping");
        }
        "status" => {
            let state = if running(REC) {
                restart_dead_transcriber();
                "recording"
            // .stopping outlives a drain that was killed; without the drain it's stale, not "stopping"
            } else if Path::new(".stopping").exists() && running(TR) && running(DRAIN) {
                "stopping"
            } else if running(TR) {
                let _ = fs::remove_file(".stopping");
                "paused"
            } else {
                let _ = fs::remove_file(".stopping");
                "stopped"
            };
            println!("{state}");
        }
        "health" => {
            if !running(REC) {
                if recorder_blocked(&String::from_utf8_lossy(
                    &fs::read("start.log").unwrap_or_default(),
                )) {
                    println!(
                        "Recording blocked: allow Ozen in System Settings > Privacy & Security > Screen & System Audio Recording, then press Start"
                    );
                }
                return;
            }
            if Path::new("no-display").exists() {
                println!(
                    "Recording on hold: the screen is asleep or locked. It resumes when you wake it"
                );
            }
            if let Ok(names) = fs::read_to_string("mic-fallback")
                && let Some((using, silent)) = names.split_once('\n')
            {
                println!(
                    "Using {using} because {silent} is silent. Pick an input in System Settings > Sound to switch"
                );
            }
            if let Ok(device) = fs::read_to_string("mic-silent") {
                println!(
                    "Microphone is silent ({device}): pick another input in System Settings > Sound"
                );
            }
            let n = chunks_waiting();
            if !running(TR) {
                println!(
                    "Transcriber stopped, {n} chunks waiting: restarting it automatically (details in start.log)"
                );
            } else if n > 12 {
                // >1 min behind (call+mic+local per 15s); first run also downloads the models
                println!("Transcriber catching up: {n} chunks waiting");
            }
        }
        // Snapshot for answering a question mid-meeting: screen image + recent transcript.
        "look" => {
            let n = std::env::args().nth(2).unwrap_or_else(|| "40".into());
            if ok(cmd("screencapture").args(["-x", "-D1", "screen.png"]))
                && ok(cmd("sips")
                    .args(["-Z", "1280", "screen.png", "--out", "screen-small.png"])
                    .stdout(Stdio::null()))
            {
                println!("screen: {}/screen-small.png", env!("CARGO_MANIFEST_DIR"));
            }
            // labels corrected by your tags
            if !ok(cmd("uv").args(["run", "-q", "train.py", "show", &n])) {
                exit(1);
            }
        }
        "app" => {
            if !build_app(&app) {
                exit(1);
            }
            println!("installed {app}");
        }
        "bar" => {
            if !build_app(&app) {
                exit(1);
            }
            let _ = cmd("open").arg(&app).status();
        }
        _ => {
            println!("{USAGE}");
            exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::recorder_blocked;

    #[test]
    fn blocked_only_when_the_last_recorder_start_failed() {
        let failed = "recording to /x/chunks\nFatal error: ... declined TCCs ...\n";
        assert!(recorder_blocked(failed));
        assert!(recorder_blocked(&format!(
            "{failed}transcribing chunks -> transcript.txt\nFetching 4 files\n"
        )));
        assert!(!recorder_blocked(&format!(
            "{failed}recording to /x/chunks\n"
        )));
        assert!(!recorder_blocked(""));
    }
}
