//! ozen control. Owns the recorder and transcriber processes; the menu bar app only asks it.
mod compare;
mod dmg;
mod ecapa;
mod eval;
mod fixes;
mod ignore;
mod low_disk_alert;
mod mcp;
mod meetings;
mod panel;
mod places;
mod separate;
mod text;
mod timebar;
mod train;
mod voices;
mod whisper;

use std::fs::{self, File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio, exit};
use std::thread::sleep;
use std::time::Duration;

const USAGE: &str = "\
ozen control: start | pause | resume | stop | record | process | status | health | look | fix | eval | compare | tag | ignore | tag-menu | unsure | controls | voices | name | rename | forget | retrain | show | place | places | meetings | gather | live | open | app | bar | mcp
  start/resume  record + transcribe
  pause         stop recording; transcriber stays loaded so resume is instant
  stop          stop recording, finish transcribing what's queued, then exit
  record        record only: chunks queue up in chunks/ untranscribed until `process` (stop ends it)
  process [stop]
                transcribe the queued chunks without recording, then exit; while recording, keeps
                transcribing live until the recording stops. `process stop` stops transcribing
  status        prints recording | paused | stopping | processing | stopped
  look [N]      screenshot to screen-small.png and print the last N transcript lines (default 40)
  eval [--vocab 0,10,30,60] [--repeat 0,1,2,3] [--real] [--fresh]
                score learning settings on a fixed set of spoken lines (see src/eval.rs)
  compare [N]   transcribe the last N chunks with speech in recent/ (default 6) with stock Whisper, the
                Hebrew model and Hebrew + vocab.txt, to judge a model or prompt change on your own speech
  fix ID [TEXT] correct a transcript line (empty clears); relearns the words and corrections the transcriber uses
  tag ID [NAME] set who said a transcript line (empty clears), then retrain
  ignore ID...  tag transcript lines as a new voice to ignore (a video playing nearby: Ignored, Ignored 2...),
                then retrain; `tag ID 'Ignored 2'` adds a line to one you already ignore
  tag-menu ID   JSON: the panel's menu for tagging that line (people, new person, ignore, clear)
  unsure        JSON: untagged lines ozen isn't sure who said, most uncertain first, and when each leaves Review
  controls STATE [split]
                JSON: the panel's Start/Pause/Stop/Process buttons for that recorder state
  voices        JSON: people, this run's unnamed speakers and ignored voices, with line counts and recent lines
  name NAME ID...
                tag those lines as NAME (an unnamed speaker's lines, from `voices`), then retrain
  rename FROM TO
                move every line tagged FROM to TO (an existing TO merges them), then retrain; TO Ignored
                makes FROM a new ignored voice
  forget NAME   clear every tag NAME on this Mac, then retrain; `forget 'Ignored 2'` stops ignoring that voice
  retrain       rebuild voiceprints, labels and the ignored voices from all tags
  show [N]      print the last N transcript lines (default 40), speakers corrected by your tags
  health        prints one line per problem (recording blocked or on hold, silent mic, transcriber down or behind)
  place [--restart]
                where you are and which place that is (JSON); keeps a location watcher running while some place
                has coordinates. --restart replaces the watcher (after sleep)
  places here N set place N (from 0, as in the Places window) to where you are now
  meetings      list past meetings: id, start, minutes, lines, first words (tab separated)
  gather [--kev] ID...
                write those meetings into context/<now>/ to start Claude Code or Hermes in; --kev also adds
                the ones local Kev (localhost:8009) judges related. Prints the files written, then the folder
  live [--open claude|hermes]
                write the meeting happening now into context/live/ and print the folder; a background
                live-sync keeps it current every 15s until the meeting ends. --open also starts that agent there
  open DIR claude|hermes|finder
                start that agent (or Finder) in a folder written by gather or live
  app           build Ozen.app into ~/Applications (open it from Spotlight/Launchpad)
  bar           build if needed and open Ozen.app (its buttons call this binary)
  dmg APP ICNS OUT  pack APP (a built Ozen.app) into the release DMG at OUT, ICNS as its volume icon
  mcp           MCP server on stdio: agents read and edit meetings, lines, speakers, places and vocab";

const REC_BUILT: &str = "target/release/rec"; // src/bin/rec.rs, built by cargo alongside this CLI
const LOCATE_BUILT: &str = "target/release/locate"; // src/bin/locate.rs
const LOCATE_BIN: &str = "target/locator/locate";
const LOCATE: &str = r"^target/locator/locate watch"; // the watcher `place` keeps running
// macOS lists a bare binary under its file name in Privacy & Security, so run a copy named ozen.
const REC_BIN: &str = "target/recorder/ozen";
// The old path too, so pause/stop still reach a recorder started before the rename.
const REC: &str = r"^target/(recorder/ozen|release/rec) chunks"; // anchored so pgrep never matches shells that merely mention the command
const TR: &str = r"uv run transcribe\.py chunks|python3 transcribe\.py chunks";
const DRAIN: &str = r"/ozen drain$"; // the detached helper `stop` leaves behind
const PROCESS: &str = r"/ozen process-queue$"; // the detached helper `process` leaves behind
const RECORD_ONLY: &str = ".record-only"; // recording started by `record`: no transcriber is kept running for it
const PROCESSING: &str = ".processing"; // `process` asked to transcribe the queue; removed when it's done or stopped
const LIVE_SYNC: &str = r"/ozen live-sync$"; // keeps context/live/ current while the meeting goes on
/// Tests that chdir into a temp dir hold this: the working directory is shared by every test thread.
#[cfg(test)]
static CWD: std::sync::Mutex<()> = std::sync::Mutex::new(());
const BLOCKED: &str = "declined TCCs"; // ScreenCaptureKit's error when the recording permission is missing

/// The checkout holding the sources, chunks and logs: the one this binary sits in (`<root>/target/release/ozen`),
/// so a prebuilt release unpacked anywhere works; else the one it was built from (`cargo test`, odd layouts).
pub fn root() -> String {
    if let Ok(dir) = std::env::var("OZEN_DIR")
        && !dir.is_empty()
    {
        return dir; // the bar app sets it; tests point it at a sample folder
    }
    std::env::current_exe()
        .and_then(|e| e.canonicalize()) // through a symlink like ~/.local/bin/ozen
        .ok()
        .and_then(|e| Some(e.parent()?.parent()?.parent()?.to_path_buf()))
        .filter(|r| r.join("Cargo.toml").exists())
        .map_or(env!("CARGO_MANIFEST_DIR").into(), |r| {
            r.display().to_string()
        })
}

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

/// PIDs from `lsof -Fpn -d cwd` output whose working directory is `dir`.
fn pids_in(lsof: &str, dir: &Path) -> Vec<String> {
    let mut pid = "";
    let mut ours = Vec::new();
    for l in lsof.lines() {
        if let Some(p) = l.strip_prefix('p') {
            pid = p;
        } else if let Some(n) = l.strip_prefix('n')
            && fs::canonicalize(n).is_ok_and(|n| n == dir)
        {
            ours.push(pid.to_string());
        }
    }
    ours
}

/// Processes matching `pattern` that run in this checkout. The same commands run from another checkout,
/// worktree or test copy (`uv run transcribe.py chunks` anywhere) are someone else's: never count or kill them.
fn ours(pattern: &str) -> Vec<String> {
    let Ok(found) = cmd("pgrep").args(["-f", pattern]).output() else {
        return Vec::new();
    };
    let pids: Vec<&str> = std::str::from_utf8(&found.stdout)
        .unwrap_or("")
        .split_whitespace()
        .collect();
    if pids.is_empty() {
        return Vec::new();
    }
    let Ok(cwd) = cmd("lsof")
        .args(["-a", "-d", "cwd", "-Fpn", "-p", &pids.join(",")])
        .output()
    else {
        return Vec::new();
    };
    let here = std::env::current_dir()
        .and_then(fs::canonicalize)
        .unwrap_or_default();
    pids_in(&String::from_utf8_lossy(&cwd.stdout), &here)
}

fn now() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}

fn running(pattern: &str) -> bool {
    !ours(pattern).is_empty()
}

fn signal(sig: &str, pattern: &str) {
    let pids = ours(pattern);
    if !pids.is_empty() {
        let _ = cmd("kill").arg(sig).args(pids).status();
    }
}

/// Keep start.log bounded: at `start`, move a log past 1 MiB aside to start.log.1 (one generation kept).
/// Processes already running keep appending to the moved file until they restart; nothing is lost.
fn rotate_log() {
    if fs::metadata("start.log").is_ok_and(|m| m.len() > 1 << 20) {
        let _ = fs::rename("start.log", "start.log.1");
    }
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
    text::mark_junk();
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

/// Hand the transcriber to a detached `process-queue`, which stops it once the queue is transcribed.
fn start_processing() {
    File::create(PROCESSING).expect("create .processing");
    if !running(PROCESS) {
        let me = std::env::current_exe().expect("own path");
        spawn_detached(
            Command::new(me).arg("process-queue"),
            Stdio::null(),
            Stdio::null(),
        );
    }
}

/// `cargo build` re-links rec and locate with ad-hoc signatures, which macOS would treat as new apps; copy each
/// to its stable path and re-sign it with the stable identity so its permission (recording, location) carries over.
fn prepare(built: &str, bin: &str) -> bool {
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
fn drop_script_envs() {
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

fn stale_env(name: &str, ps: &str) -> bool {
    let ours = ["asr-", "eval-", "overlap-", "transcribe-"]
        .iter()
        .any(|p| {
            name.strip_prefix(p)
                .is_some_and(|h| h.len() == 16 && h.bytes().all(|b| b.is_ascii_hexdigit()))
        });
    ours && !ps.contains(&format!("environments-v2/{name}/"))
}

fn prepare_rec() -> bool {
    prepare(REC_BUILT, REC_BIN)
}

fn build_app(app: &str) -> bool {
    let bin = format!("{app}/Contents/MacOS/Ozen");
    if newer(&bin, "menubar.swift")
        && newer(&bin, "icon.swift")
        && newer(&bin, "map.html")
        && newer(&bin, "chunks.html")
    {
        return true;
    }
    let resources = format!("{app}/Contents/Resources");
    for d in [format!("{app}/Contents/MacOS"), resources.clone()] {
        fs::create_dir_all(d).expect("create app bundle");
    }
    if !ok(cmd("swiftc").args(["-O", "menubar.swift", "-o", &bin])) {
        return false;
    }
    for page in ["map.html", "chunks.html"] {
        if fs::copy(page, format!("{resources}/{page}")).is_err() {
            eprintln!("{page} missing; its window stays blank");
        }
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
  <key>NSLocationUsageDescription</key><string>Ozen starts or stops recording when you arrive at places you set, like Home or Work.</string>
</dict></plist>
"#)).expect("write Info.plist");
    sign(app, true); // macOS grants permissions only to signed apps
    // Register with Launch Services + Spotlight so it's findable right away, not after the next index pass.
    let _ = cmd("/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister")
        .args(["-f", app]).status();
    let _ = cmd("mdimport").arg(app).stderr(Stdio::null()).status();
    true
}

/// Rebuild the people's voiceprints and labels; then add the voices to ignore on top, even if training failed.
fn retrain() {
    let trained = std::panic::catch_unwind(|| train::retrain(true)).is_ok();
    ignore::apply();
    if !trained {
        exit(1);
    }
}

fn main() {
    let cwd = std::env::current_dir().unwrap_or_default();
    // The repo is where the sources, chunks and logs live, wherever this is called from.
    std::env::set_current_dir(root()).expect("cd to the ozen checkout");
    let app = format!("{}/Applications/Ozen.app", home());
    match std::env::args().nth(1).as_deref().unwrap_or("") {
        "start" | "resume" => {
            rotate_log();
            if !prepare_rec() {
                exit(1);
            }
            fs::create_dir_all("chunks").expect("create chunks/");
            // Live again: the transcriber stays loaded across pauses, not just until the queue empties.
            let _ = fs::remove_file(RECORD_ONLY);
            let _ = fs::remove_file(PROCESSING);
            if !running(TR) {
                start_transcriber();
            }
            if !running(REC) {
                spawn_detached(cmd(REC_BIN).arg("chunks"), log().into(), log().into());
            }
        }
        "pause" => signal("-INT", REC), // SIGINT: recorder flushes its current chunk first
        "record" => {
            rotate_log();
            if !prepare_rec() {
                exit(1);
            }
            fs::create_dir_all("chunks").expect("create chunks/");
            File::create(RECORD_ONLY).expect("create .record-only");
            // A live transcriber already running keeps going as processing, so nothing heard so far waits.
            if running(TR) {
                start_processing();
            }
            if !running(REC) {
                spawn_detached(cmd(REC_BIN).arg("chunks"), log().into(), log().into());
            }
        }
        "process" if std::env::args().nth(2).as_deref() == Some("stop") => {
            let _ = fs::remove_file(PROCESSING);
            signal("-TERM", TR); // chunks are deleted only once transcribed, so the rest waits for the next run
        }
        "process" => {
            fs::create_dir_all("chunks").expect("create chunks/");
            if !running(TR) {
                start_transcriber();
            }
            start_processing();
        }
        // Keep the transcriber running until the queue is empty and nothing is recording, then stop it.
        "process-queue" => {
            while Path::new(PROCESSING).exists() {
                if !running(REC) && chunks_waiting() == 0 {
                    signal("-TERM", TR);
                    let _ = fs::remove_file(PROCESSING);
                    break;
                }
                restart_dead_transcriber();
                sleep(Duration::from_secs(2));
            }
        }
        // Record only: no transcriber to drain. A `process` run finishes the queue by itself.
        "stop" if Path::new(RECORD_ONLY).exists() => {
            signal("-INT", REC);
            let _ = fs::remove_file(RECORD_ONLY);
        }
        "stop" => {
            signal("-INT", REC);
            File::create(".stopping").expect("create .stopping");
            let me = std::env::current_exe().expect("own path");
            spawn_detached(Command::new(me).arg("drain"), Stdio::null(), Stdio::null());
        }
        // Let the transcriber drain queued chunks (max 2 min) so the last words aren't lost.
        "drain" => {
            for _ in 0..120 {
                if chunks_waiting() == 0 || !running(TR) {
                    break;
                }
                sleep(Duration::from_secs(1));
            }
            signal("-TERM", TR);
            let _ = fs::remove_file(".stopping");
        }
        "status" => {
            if Path::new(PROCESSING).exists() && !running(PROCESS) {
                let _ = fs::remove_file(PROCESSING); // its helper was killed
            }
            let state = if running(REC) {
                if !Path::new(RECORD_ONLY).exists() {
                    restart_dead_transcriber();
                }
                "recording"
            } else if Path::new(PROCESSING).exists() {
                "processing"
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
            // Place switching doesn't depend on recording: say why it can't see where you are.
            // Not while the menu bar app supplies the location instead (it touches here.json.app each poll).
            let error = format!("{}.error", places::HERE);
            let supplied = fs::metadata(format!("{}.app", places::HERE))
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() < 30));
            if places::tracked(&places::load(places::FILE))
                && !supplied
                && let Ok(why) = fs::read_to_string(&error)
            {
                println!("Place switching is off: {}", why.trim());
            }
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
            if let Some(alert) = low_disk_alert::check() {
                println!("{alert}");
            }
            if Path::new("no-display").exists() {
                println!(
                    "Recording on hold: the screen is asleep or locked. It resumes when you wake it"
                );
            }
            if let Ok(error) = fs::read_to_string("capture-error") {
                println!("Recording paused: capture failed to start ({error}). Retrying every 10s");
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
            if Path::new(RECORD_ONLY).exists() && !Path::new(PROCESSING).exists() {
                // recording without transcribing is what was asked for
            } else if !running(TR) {
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
                println!("screen: {}/screen-small.png", root());
            }
            fixes::show(n.parse().unwrap_or(40)); // speakers corrected by your tags, text by your fixes
        }
        "eval" => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            if let Err(e) = eval::run(&args) {
                eprintln!("{e}");
                exit(1);
            }
        }
        "compare" => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            if let Err(e) = compare::run(&args) {
                eprintln!("{e}");
                exit(1);
            }
        }
        "fix" => {
            let (Some(id), text) = (
                std::env::args().nth(2),
                std::env::args().nth(3).unwrap_or_default(),
            ) else {
                println!("{USAGE}");
                exit(2);
            };
            if let Err(e) = fixes::fix(&id, text.trim()) {
                eprintln!("{e}");
                exit(1);
            }
        }
        // Where you are and which place that is, as JSON for the menu bar (polled with status). Keeps a
        // location watcher running while some place has coordinates, and stops it once none does.
        // --restart replaces the watcher (after wake).
        "place" => {
            let ps = places::load(places::FILE);
            if !places::tracked(&ps) {
                signal("-TERM", LOCATE);
                let _ = fs::remove_file(places::HERE);
                let _ = fs::remove_file(format!("{}.error", places::HERE));
                return println!(r#"{{"here":null,"place":null}}"#);
            }
            // After sleep the Mac may have moved: a new watcher sends a fresh fix; here.json holds the old one till then.
            if std::env::args().any(|a| a == "--restart") {
                signal("-TERM", LOCATE);
                sleep(Duration::from_millis(200));
            }
            let _ = File::create(format!("{}.asked", places::HERE)); // the watcher's heartbeat
            // A watcher that stopped on a permission problem (FILE.error) is retried each minute, not each poll.
            let error = format!("{}.error", places::HERE);
            let backing_off = fs::metadata(&error)
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|e| e.as_secs() < 60));
            if !backing_off && !running(LOCATE) && prepare(LOCATE_BUILT, LOCATE_BIN) {
                spawn_detached(
                    cmd(LOCATE_BIN).args(["watch", places::HERE]),
                    Stdio::null(),
                    log().into(),
                );
            }
            let here: serde_json::Value = fs::read(places::HERE)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or_default();
            let (Some(lat), Some(lon), Some(t)) = (
                here["lat"].as_f64(),
                here["lon"].as_f64(),
                here["t"].as_f64(),
            ) else {
                return println!(r#"{{"here":null,"place":null}}"#);
            };
            let place = places::at(&ps, lat, lon)
                .map(|p| serde_json::json!({"label": p.label, "action": p.action}));
            println!(
                "{}",
                serde_json::json!({"here": {"lat": lat, "lon": lon, "age": now() - t}, "place": place})
            );
        }
        // Set a place's coordinates to where you are now.
        "places" => {
            let a: Vec<String> = std::env::args().skip(2).collect();
            let ["here", i] = a.iter().map(String::as_str).collect::<Vec<_>>()[..] else {
                eprintln!("usage: ozen places here N");
                exit(2);
            };
            let Ok(i) = i.parse::<usize>() else {
                eprintln!("{i} isn't a place number");
                exit(2);
            };
            if !prepare(LOCATE_BUILT, LOCATE_BIN) {
                exit(1);
            }
            let out = cmd(LOCATE_BIN)
                .arg("once")
                .stderr(Stdio::inherit())
                .output()
                .expect("run locate");
            let fix: Vec<f64> = String::from_utf8_lossy(&out.stdout)
                .split_whitespace()
                .filter_map(|w| w.parse().ok())
                .collect();
            let [lat, lon, _] = fix[..] else {
                exit(out.status.code().unwrap_or(1))
            };
            if let Err(e) = places::set_location(places::FILE, i, lat, lon) {
                eprintln!("{e}");
                exit(1);
            }
            println!("{lat} {lon}");
        }
        "meetings" => {
            for m in meetings::all().iter().rev() {
                println!("{}", m.row());
            }
        }
        "gather" => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            let ids: Vec<String> = args.iter().filter(|a| *a != "--kev").cloned().collect();
            match meetings::gather(&ids, args.iter().any(|a| a == "--kev")) {
                // the transcripts it wrote, then the folder on the last line
                Ok((dir, files)) => println!(
                    "{}{dir}",
                    files.iter().map(|f| f.clone() + "\n").collect::<String>()
                ),
                Err(e) => {
                    eprintln!("{e}");
                    exit(1);
                }
            }
        }
        "live" => {
            let open = std::env::args().skip_while(|a| a != "--open").nth(1);
            match meetings::live(now()).and_then(|dir| {
                if !running(LIVE_SYNC) {
                    let me = std::env::current_exe().map_err(|e| e.to_string())?;
                    spawn_detached(
                        Command::new(me).arg("live-sync"),
                        Stdio::null(),
                        Stdio::null(),
                    );
                }
                open.map_or(Ok(()), |what| meetings::open(&dir, &what))?;
                Ok(dir)
            }) {
                Ok(dir) => println!("{dir}"),
                Err(e) => {
                    eprintln!("{e}");
                    exit(1);
                }
            }
        }
        // What the panel calls when you tag a line: who said it, or a voice to ignore.
        "tag" => {
            let Some(id) = std::env::args().nth(2) else {
                println!("{USAGE}");
                exit(2);
            };
            ignore::tag(&[id], &std::env::args().nth(3).unwrap_or_default());
            retrain();
        }
        // Rewrites context/live/ every 15s until no line has arrived for 10 minutes: the meeting is over.
        "live-sync" => {
            while {
                sleep(Duration::from_secs(15));
                meetings::live(now()).is_ok()
            } {}
        }
        "open" => {
            let a: Vec<String> = std::env::args().skip(2).collect();
            let [dir, what] = a.as_slice() else {
                eprintln!("usage: ozen open DIR claude|hermes|finder");
                exit(2);
            };
            if let Err(e) = meetings::open(dir, what) {
                eprintln!("{e}");
                exit(1);
            }
        }
        "ignore" => {
            let ids: Vec<String> = std::env::args().skip(2).collect();
            if ids.is_empty() {
                println!("{USAGE}");
                exit(2);
            }
            ignore::tag(&ids, &ignore::fresh(&fixes::read("tags.json")));
            retrain();
        }
        "retrain" => retrain(),
        "voices" => println!("{}", serde_json::Value::from(voices::list())),
        "timebar" => println!("{}", timebar::json()),
        "tag-menu" => match std::env::args().nth(2) {
            Some(id) => println!("{}", panel::tag_menu_json(&id)),
            None => {
                println!("{USAGE}");
                exit(2);
            }
        },
        "unsure" => println!("{}", panel::unsure_json()),
        "controls" => {
            let state = std::env::args().nth(2).unwrap_or_default();
            let split = std::env::args().nth(3).as_deref() == Some("split");
            println!("{}", panel::controls_json(&state, split));
        }
        "name" => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            match args.split_first() {
                Some((name, ids)) if !name.trim().is_empty() && !ids.is_empty() => {
                    ignore::tag(ids, name)
                }
                _ => {
                    println!("{USAGE}");
                    exit(2);
                }
            }
            retrain();
        }
        "rename" | "forget" => {
            let from = std::env::args().nth(2).unwrap_or_default();
            let to = if std::env::args().nth(1).as_deref() == Some("forget") {
                String::new()
            } else {
                std::env::args().nth(3).unwrap_or_default()
            };
            if from.is_empty()
                || (to.trim().is_empty() && std::env::args().nth(1).as_deref() == Some("rename"))
            {
                println!("{USAGE}");
                exit(2);
            }
            // Ignoring a person starts a voice of its own rather than merging into the other ignored ones.
            let to = if to == ignore::IGNORE && from != to {
                ignore::fresh(&fixes::read("tags.json"))
            } else {
                to
            };
            match voices::retag(&from, &to) {
                Ok(n) => println!("retagged {n} lines"),
                Err(e) => {
                    eprintln!("{e}");
                    exit(1);
                }
            }
            retrain();
        }
        // Whisper (src/whisper.rs) for asr.py, fed audio on stdin.
        "whisper" => {
            if let Err(e) = whisper::serve() {
                eprintln!("{e}");
                exit(1);
            }
        }
        // The transcriber's voiceprint encoder (src/ecapa.rs), fed audio on stdin.
        "separate" => {
            if let Err(e) = separate::serve() {
                eprintln!("{e}");
                exit(1);
            }
        }
        "embed" => {
            if let Err(e) = ecapa::serve() {
                eprintln!("{e}");
                exit(1);
            }
        }
        "show" => fixes::show(
            std::env::args()
                .nth(2)
                .and_then(|n| n.parse().ok())
                .unwrap_or(40),
        ),
        "mcp" => mcp::serve(),
        "app" => {
            if !build_app(&app) {
                exit(1);
            }
            println!("installed {app}");
            if !ok(cmd("uv").args(["sync", "-q"])) {
                eprintln!("uv sync failed: the transcriber will set up its env on first start");
            }
            drop_script_envs();
        }
        "dmg" => {
            let a: Vec<String> = std::env::args().skip(2).collect();
            let [app, icon, out] = &a[..] else {
                eprintln!("usage: ozen dmg <Ozen.app> <volume.icns> <out.dmg>");
                exit(2);
            };
            // relative paths are the caller's, not the checkout main() moved into
            let abs = |p: &str| {
                std::path::absolute(Path::new(&cwd).join(p))
                    .unwrap()
                    .display()
                    .to_string()
            };
            if let Err(e) = dmg::build(&abs(app), &abs(icon), &abs(out)) {
                eprintln!("dmg: {e}");
                exit(1);
            }
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
    #[test]
    fn drops_only_idle_script_envs() {
        let ps = "/u/.cache/uv/environments-v2/transcribe-1b646a70201e1ec6/bin/python3 transcribe.py chunks\n";
        assert!(super::stale_env("transcribe-04996fcebd6a04ca", ps));
        assert!(super::stale_env("asr-2871e1033d644c4b", ps));
        assert!(!super::stale_env("transcribe-1b646a70201e1ec6", ps)); // running
        assert!(!super::stale_env("train-02cfad274e3027bf", ps)); // not one of our scripts
        assert!(!super::stale_env("transcribe-notahash", ps));
    }

    #[test]
    fn keeps_only_processes_in_this_checkout() {
        let tmp = std::env::temp_dir().canonicalize().unwrap();
        let here = tmp.join(format!("ozen-pids-{}", std::process::id()));
        let other = tmp.join(format!("ozen-pids-other-{}", std::process::id()));
        std::fs::create_dir_all(&here).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let out = format!(
            "p10\nfcwd\nn{}\np11\nfcwd\nn{}\np12\nfcwd\nn/gone\n",
            here.display(),
            other.display()
        );
        assert_eq!(super::pids_in(&out, &here), ["10"]);
        let _ = (std::fs::remove_dir(&here), std::fs::remove_dir(&other));
    }

    #[test]
    fn rotates_only_a_log_past_one_mib() {
        let _cwd = super::CWD.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("ozen-rotate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_current_dir(&dir).unwrap();
        std::fs::write("start.log", vec![b'x'; 1000]).unwrap();
        super::rotate_log();
        assert!(
            std::path::Path::new("start.log").exists(),
            "small log stays"
        );
        std::fs::write("start.log", vec![b'x'; (1 << 20) + 1]).unwrap();
        super::rotate_log();
        assert!(!std::path::Path::new("start.log").exists());
        assert_eq!(
            std::fs::metadata("start.log.1").unwrap().len(),
            (1 << 20) + 1
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

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
