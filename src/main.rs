//! ozen control. Owns the recorder and transcriber processes; the menu bar app only asks it.
use std::fs::{self, File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio, exit};
use std::thread::sleep;
use std::time::Duration;

const USAGE: &str = "\
ozen control: start | pause | resume | stop | status | health | app | bar
  start/resume  record + transcribe (builds the recorder if rec.swift changed)
  pause         stop recording; transcriber stays loaded so resume is instant
  stop          stop recording, finish transcribing what's queued, then exit
  status        prints recording | paused | stopping | stopped
  health        prints one line per problem (recording blocked, silent mic, transcriber down or behind)
  app           build Ozen.app into ~/Applications (open it from Spotlight/Launchpad)
  bar           build if needed and open Ozen.app (its buttons call this binary)";

const REC: &str = r"^\./rec chunks"; // anchored so pgrep never matches shells that merely mention the command
const TR: &str = r"uv run transcribe\.py chunks|python3 transcribe\.py chunks";
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

fn build_rec() -> bool {
    newer("rec", "rec.swift") || ok(cmd("swiftc").args(["-O", "rec.swift", "-o", "rec"]))
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
    // ad-hoc: required for macOS to grant it permissions
    let _ = cmd("codesign")
        .args(["--force", "--deep", "-s", "-", app])
        .stderr(Stdio::null())
        .status();
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
            if !build_rec() {
                exit(1);
            }
            fs::create_dir_all("chunks").expect("create chunks/");
            if !running(TR) {
                if !ok(cmd("git")
                    .args(["-C", "voices", "pull", "-q", "--ff-only"])
                    .stderr(Stdio::null()))
                {
                    let _ = std::io::Write::write_all(
                        &mut log(),
                        b"voices registry pull failed; using local copy\n",
                    );
                }
                spawn_detached(
                    cmd("uv").args(["run", "transcribe.py", "chunks", "transcript.txt"]),
                    log().into(),
                    log().into(),
                );
            }
            if !running(REC) {
                spawn_detached(cmd("./rec").arg("chunks"), log().into(), log().into());
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
                "recording"
            } else if Path::new(".stopping").exists() && running(TR) {
                "stopping"
            } else if running(TR) {
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
            if let Ok(device) = fs::read_to_string("mic-silent") {
                println!(
                    "Microphone is silent ({device}): pick another input in System Settings > Sound"
                );
            }
            let n = chunks_waiting();
            if !running(TR) {
                println!(
                    "Transcriber isn't running, {n} chunks waiting: press Stop, then Start (details in start.log)"
                );
            } else if n > 12 {
                // >1 min behind (call+mic+local per 15s); first run also downloads the models
                println!("Transcriber catching up: {n} chunks waiting");
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
