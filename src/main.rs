//! ozen control. Owns the recorder and transcriber processes; the menu bar app only asks it.
mod app_bundle;
mod app_plist;
mod compare;
mod crdt;
mod dmg;
mod ecapa;
mod eval;
mod eval_overlap;
mod fixes;
mod icon;
mod ignore;
mod low_disk_alert;
mod mcp;
mod meetings;
mod merge;
mod mic;
mod one_at_a_time;
mod overlap;
mod panel;
mod places;
mod procs;
mod separate;
mod sync;
mod text;
mod timebar;
mod train;
mod transcribe;
mod transcribe_io;
mod voices;
mod whisper;

use app_bundle::{build_app, drop_script_envs, prepare, prepare_rec};
use procs::{Signal, ours, running, signal};
use std::fs::{self, File, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio, exit};
use std::thread::sleep;
use std::time::Duration;

const USAGE: &str = "\
ozen control: start | pause | resume | stop | record | process | priority | status | health | look | fix | eval | compare | tag | ignore | tag-menu | mic | transcript | unsure | controls | voices | name | rename | forget | retrain | merge | sync | show | place | places | meetings | gather | live | open | app | bar | mcp
  start/resume  record + transcribe
  pause         stop recording; transcriber stays loaded so resume is instant
  stop          stop recording, finish transcribing what's queued, then exit
  record        record only: chunks queue up in chunks/ untranscribed until `process` (stop ends it)
  process [stop]
                transcribe the queued chunks without recording, then exit; while recording, keeps
                transcribing live until the recording stops. `process stop` stops transcribing
  priority [low|normal]
                low runs the transcriber at macOS background priority (it yields CPU, disk and GPU to other
                apps, so the transcript can lag), now and on every start; no argument prints the current one
  status        prints recording | paused | stopping | processing | stopped
  look [N]      screenshot to screen-small.png and print the last N transcript lines (default 40)
  eval [--vocab 0,10,30,60] [--repeat 0,1,2,3] [--real] [--fresh]
                score learning settings on a fixed set of spoken lines (see src/eval.rs)
  eval-overlap [ami] [he] [call] [--n 20]
                score separating people talking at once on real speech (see src/eval_overlap.rs)
  compare [N]   transcribe the last N chunks with speech in recent/ (default 6) with stock Whisper, the
                Hebrew model and Hebrew + vocabulary, to judge a model or prompt change on your own speech
  fix ID [TEXT] correct a transcript line (empty clears); relearns the words and corrections the transcriber uses
  tag ID [NAME] set who said a transcript line (empty clears), then retrain
  ignore ID...  tag transcript lines as a new voice to ignore (a video playing nearby: Ignored, Ignored 2...),
                then retrain; `tag ID 'Ignored 2'` adds a line to one you already ignore
  tag-menu ID   JSON: the panel's menu for tagging that line (people, new person, ignore, clear)
  mic           JSON: the meeting app using the microphone now, {\"app\": \"Zoom\"} or {\"app\": null}
  transcript [PENDING]
                JSON: the panel's transcript (last 400 lines), timeline bars, Review queue and footer;
                PENDING is {\"tags\": {id: name}, \"fixes\": {id: text}} the panel set but hasn't written yet
  unsure        JSON: untagged lines ozen isn't sure who said, most uncertain first, and when each leaves Review
  controls STATE [split]
                JSON: the panel's Start/Pause/Stop buttons for that recorder state
  voices        JSON: people, this run's unnamed speakers and ignored voices, with line counts and recent lines
  name NAME ID...
                tag those lines as NAME (an unnamed speaker's lines, from `voices`), then retrain
  rename FROM TO
                move every line tagged FROM to TO (an existing TO merges them), then retrain; TO Ignored
                makes FROM a new ignored voice
  forget NAME   clear every tag NAME on this Mac, then retrain; `forget 'Ignored 2'` stops ignoring that voice
  retrain       rebuild voiceprints, labels and the ignored voices from all tags
  merge DIR     merge another ozen folder's lines, tags, fixes, places and vocabulary into this one (another
                Mac's, a backup), then relearn and retrain; merging is safe to repeat (src/crdt.rs)
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
  dmg APP OUT   pack APP (a built Ozen.app) into the release DMG at OUT
  mcp           MCP server on stdio: agents read and edit meetings, lines, speakers, places and vocab";

const REC_BUILT: &str = "target/release/rec"; // src/bin/rec/main.rs, built by cargo alongside this CLI
const LOCATE_BUILT: &str = "target/release/locate"; // src/bin/locate.rs
const LOCATE_BIN: &str = "target/locator/locate";
const LOCATE: &str = r"^target/locator/locate watch"; // the watcher `place` keeps running
// macOS lists a bare binary under its file name in Privacy & Security, so run a copy named ozen.
const REC_BIN: &str = "target/recorder/ozen";
// The old path too, so pause/stop still reach a recorder started before the rename.
const REC: &str = r"^target/(recorder/ozen|release/rec) chunks"; // anchored so the process scan never matches shells that merely mention the command
// The Python transcriber too, so stop and restarts still reach one started before the port to Rust.
const TR: &str =
    r"/ozen transcribe chunks|uv run transcribe\.py chunks|python3 transcribe\.py chunks";
const DRAIN: &str = r"/ozen drain$"; // the detached helper `stop` leaves behind
const PROCESS: &str = r"/ozen process-queue$"; // the detached helper `process` leaves behind
const RECORD_ONLY: &str = ".record-only"; // recording started by `record`: no transcriber is kept running for it
const PROCESSING: &str = ".processing"; // `process` asked to transcribe the queue; removed when it's done or stopped
const LOW_PRIORITY: &str = ".low-priority"; // `priority low`: the transcriber runs at macOS background priority
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

fn now() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
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

const TR_LOCK: &str = ".transcriber.lock"; // held by the running transcriber for as long as it runs
const RT_LOCK: &str = ".retrain.lock"; // held by the running retrain
const RT_AGAIN: &str = ".retrain-again"; // a retrain was asked for while one ran: run once more
const START_LOCK: &str = ".start.lock"; // held while a command checks what's running and starts what's missing
const TR_STARTED: &str = ".transcriber-started"; // when it was last launched, to pace automatic restarts

fn start_transcriber() {
    // First: the pull below can take seconds, and a `status` poll in between would start a second transcriber.
    let _ = File::create(TR_STARTED);
    if !train::pull_registry() {
        let _ = std::io::Write::write_all(
            &mut log(),
            b"voices registry pull failed; using local copy\n",
        );
    }
    text::mark_junk();
    spawn_detached(
        Command::new(std::env::current_exe().expect("own path")).args([
            "transcribe",
            "chunks",
            "transcript.txt",
        ]),
        log().into(),
        log().into(),
    );
    if Path::new(LOW_PRIORITY).exists() {
        set_priority(true);
    }
}

/// Moves this checkout's transcriber in or out of macOS background priority. Process-wide on purpose:
/// `taskpolicy -b` at launch would mark the threads instead, and `-B` can't undo that.
fn set_priority(low: bool) {
    for pid in ours(TR) {
        let _ = cmd("taskpolicy")
            .args([if low { "-b" } else { "-B" }, "-p", &pid.to_string()])
            .status();
    }
}

/// While recording, a transcriber that died gets restarted. The app polls `status` every 2s, so this is the
/// supervisor. At most once a minute, so one that crashes on start doesn't respawn in a tight loop.
/// Waits for any other command that is starting the recorder or transcriber, then holds the turn until the returned
/// file drops. Without it, two `start`s at once both see nothing running and both spawn: two recorders record every
/// chunk twice.
fn starting() -> File {
    let f = File::create(START_LOCK).expect("create .start.lock");
    f.lock().expect("lock .start.lock");
    f
}

fn restart_dead_transcriber() {
    let _turn = starting();
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

/// Rebuild the people's voiceprints and labels; then add the voices to ignore on top, even if training failed.
/// One retrain at a time per checkout (sync's and a tag's would overwrite each other's labels); one
/// asked for while another runs is run by that one right after, on fresh inputs.
fn retrain() {
    let mut trained = true;
    one_at_a_time::run(RT_LOCK, RT_AGAIN, || {
        trained &= std::panic::catch_unwind(|| train::retrain(true)).is_ok();
        ignore::apply();
    });
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
            let _turn = starting();
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
        "pause" => signal(Signal::Interrupt, REC), // SIGINT: recorder flushes its current chunk first
        "record" => {
            let _turn = starting();
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
        "priority" => {
            let low = match std::env::args().nth(2).as_deref() {
                Some("low") => {
                    File::create(LOW_PRIORITY).expect("create .low-priority");
                    true
                }
                Some("normal") => {
                    let _ = fs::remove_file(LOW_PRIORITY);
                    false
                }
                _ => {
                    println!(
                        "{}",
                        if Path::new(LOW_PRIORITY).exists() {
                            "low"
                        } else {
                            "normal"
                        }
                    );
                    return;
                }
            };
            set_priority(low); // a running transcriber switches now; one started later reads the file
        }
        "process" if std::env::args().nth(2).as_deref() == Some("stop") => {
            let _ = fs::remove_file(PROCESSING);
            signal(Signal::Term, TR); // chunks are deleted only once transcribed, so the rest waits for the next run
        }
        "process" => {
            let _turn = starting();
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
                    signal(Signal::Term, TR);
                    let _ = fs::remove_file(PROCESSING);
                    break;
                }
                restart_dead_transcriber();
                sleep(Duration::from_secs(2));
            }
        }
        // Record only: no transcriber to drain. A `process` run finishes the queue by itself.
        "stop" if Path::new(RECORD_ONLY).exists() => {
            signal(Signal::Interrupt, REC);
            let _ = fs::remove_file(RECORD_ONLY);
        }
        "stop" => {
            signal(Signal::Interrupt, REC);
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
            signal(Signal::Term, TR);
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
            sync::run::keep(log);
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
            // Sync runs whether or not this Mac records.
            sync::health().iter().for_each(|l| println!("{l}"));
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
        "eval-overlap" => {
            let args: Vec<String> = std::env::args().skip(2).collect();
            if let Err(e) = eval_overlap::run(&args) {
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
                signal(Signal::Term, LOCATE);
                let _ = fs::remove_file(places::HERE);
                let _ = fs::remove_file(format!("{}.error", places::HERE));
                return println!(r#"{{"here":null,"place":null}}"#);
            }
            // After sleep the Mac may have moved: a new watcher sends a fresh fix; here.json holds the old one till then.
            if std::env::args().any(|a| a == "--restart") {
                signal(Signal::Term, LOCATE);
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
            ignore::tag(&ids, &ignore::fresh(&crdt::read_map("tags.json")));
            retrain();
        }
        "retrain" => retrain(),
        "merge" => merge::cli(&cwd, USAGE),
        "sync" => sync::cli(),
        // The meeting app using the microphone now: {"app": "Zoom"} or {"app": null}. Meetings mode polls it.
        "mic" => println!("{}", serde_json::json!({"app": mic::meeting_app()})),
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
        "transcript" => println!(
            "{}",
            panel::transcript_json(&std::env::args().nth(2).unwrap_or_default())
        ),
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
                ignore::fresh(&crdt::read_map("tags.json"))
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
        "transcribe" => {
            // One transcriber per checkout, whoever starts it (start, process, the app's status poll): a second one
            // would load the models twice and could transcribe a chunk twice. macOS drops the lock when this exits.
            let lock = File::create(TR_LOCK).expect("create .transcriber.lock");
            if lock.try_lock().is_err() {
                println!("another transcriber is running in this checkout; exiting");
                return;
            }
            let arg = |i| std::env::args().nth(i).unwrap_or_default();
            if let Err(e) = transcribe::run(Path::new(&arg(2)), Path::new(&arg(3))) {
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
            drop_script_envs();
        }
        "dmg" => {
            let a: Vec<String> = std::env::args().skip(2).collect();
            let [app, out] = &a[..] else {
                eprintln!("usage: ozen dmg <Ozen.app> <out.dmg>");
                exit(2);
            };
            // relative paths are the caller's, not the checkout main() moved into
            let abs = |p: &str| {
                std::path::absolute(Path::new(&cwd).join(p))
                    .unwrap()
                    .display()
                    .to_string()
            };
            if let Err(e) = dmg::build(&abs(app), &abs(out)) {
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
#[path = "main_tests.rs"]
mod tests;
