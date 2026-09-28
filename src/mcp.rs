//! `ozen mcp`: an MCP server on stdio, so agents can read and edit everything ozen keeps.
//!
//! Edits ozen already knows how to make (fix, tag, retrain, start/stop) run this binary as a subprocess:
//! their side effects stay in one place and their prints never land in the protocol stream on stdout.
use crate::fixes::read;
use crate::meetings;
use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars::{self, JsonSchema},
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::process::{Command, Stdio};

const LINES: &str = "lines.jsonl";
const TAGS: &str = "tags.json";
const LABELS: &str = "labels.json";
const FIXES: &str = "fixes.json";
const JUNK: &str = "junk.json"; // ids of old Whisper echoes the transcriber flags; the panel hides them
const STATS: &str = "stats.json";
const PLACES: &str = "places.json";
const VOCAB: &str = "vocab.txt";
const APP_ID: &str = "com.tupe12334.ozen"; // Ozen.app's defaults domain, where the menu bar keeps the record mode

type Row = Map<String, Value>;
type Reply = Result<String, String>;

/// Run this binary with `args`; its stdout is the reply.
fn ozen(args: &[&str]) -> Reply {
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let out = Command::new(me)
        .args(args)
        .stdin(Stdio::null()) // detached recorder/transcriber must not hold the protocol's stdin
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if out.status.success() {
        Ok(text)
    } else {
        Err(format!("{text}\n{}", String::from_utf8_lossy(&out.stderr))
            .trim()
            .into())
    }
}

fn err(e: impl ToString) -> String {
    e.to_string()
}

fn rows() -> Vec<Row> {
    fs::read_to_string(LINES)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|r: &Row| {
            r.get("id").is_some_and(Value::is_string) && r.get("t").is_some_and(Value::is_f64)
        })
        .collect()
}

fn str_of<'a>(r: &'a Row, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or("")
}

/// A line without a voiceprint: one an agent wrote, not the transcriber.
fn is_note(r: &Row) -> bool {
    r.get("e").is_none_or(Value::is_null)
}

enum Edit {
    Keep,
    Drop,
    Set(Row),
}

/// lines.jsonl open for appending and locked. `rewrite` swaps in a new file while holding the old one's
/// lock, so a lock won on a swapped-out file is retried on the current one (transcribe.py does the same).
fn locked() -> Result<File, String> {
    loop {
        let f = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(LINES)
            .map_err(err)?;
        f.lock().map_err(err)?;
        if fs::metadata(LINES).is_ok_and(|m| m.ino() == f.metadata().map_or(0, |m| m.ino())) {
            return Ok(f);
        }
    }
}

/// Rewrite lines.jsonl under the transcriber's append lock, so no new line is lost, and swap it in by
/// rename, so readers that don't lock (the panel, retraining) see the old file or the new one, never half.
fn rewrite(mut edit: impl FnMut(&Row) -> Edit) -> Result<(), String> {
    let mut f = locked()?;
    let mut raw = String::new();
    f.read_to_string(&mut raw).map_err(err)?;
    let mut out = String::with_capacity(raw.len());
    for l in raw.lines() {
        match serde_json::from_str::<Row>(l).map_or(Edit::Keep, |r| edit(&r)) {
            Edit::Keep => out.push_str(l),
            Edit::Drop => continue,
            Edit::Set(r) => out.push_str(&Value::Object(r).to_string()),
        }
        out.push('\n');
    }
    let tmp = format!("{LINES}.tmp");
    fs::write(&tmp, out).map_err(err)?;
    fs::rename(&tmp, LINES).map_err(err) // the old file's lock is released when f drops, after the swap
}

fn append(r: Row) -> Result<(), String> {
    locked()?
        .write_all((Value::Object(r).to_string() + "\n").as_bytes())
        .map_err(err)
}

/// A line as agents see it: tagged speaker and fixed text applied, no voiceprint.
fn view(r: &Row, tags: &Row, labels: &Row, fixes: &Row, junk: &[String]) -> Value {
    let id = str_of(r, "id");
    let tag = tags
        .get(id)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let label = labels.get(id);
    let guess = label.and_then(|l| l["spk"].as_str());
    let (speaker, from) = match (tag, guess) {
        (Some(t), _) => (t, "tag"),
        (_, Some(g)) => (g, "voice"),
        _ => (
            r.get("spk").and_then(Value::as_str).unwrap_or("?"),
            "transcriber",
        ),
    };
    let t = r["t"].as_f64().unwrap_or(0.0);
    let fixed = fixes.get(id).and_then(Value::as_str);
    json!({
        "id": id,
        "t": t,
        "time": chrono::DateTime::from_timestamp(t as i64, 0)
            .map(|d| d.with_timezone(&chrono::Local).to_rfc3339()),
        "src": r.get("src"),
        "speaker": speaker,
        "speaker_from": from,
        "unsure": tag.is_none() && label.is_some_and(|l| l["unsure"] == true),
        "text": fixed.unwrap_or(str_of(r, "text")),
        "heard": fixed.map(|_| str_of(r, "text")), // the transcriber's text, when a fix replaced it
        "note": is_note(r),
        "junk": junk.iter().any(|j| j == id), // not said by anyone: Whisper invented it on noise
    })
}

fn views(filter: impl Fn(&Row) -> bool) -> Vec<Value> {
    let (tags, labels, fixes) = (read(TAGS), read(LABELS), read(FIXES));
    let junk: Vec<String> = fs::read(JUNK)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let mut all: Vec<Row> = rows().into_iter().filter(|r| filter(r)).collect();
    all.sort_by(|a, b| {
        a["t"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&b["t"].as_f64().unwrap_or(0.0))
    });
    all.iter()
        .map(|r| view(r, &tags, &labels, &fixes, &junk))
        .collect()
}

fn find(id: &str) -> Result<Row, String> {
    rows()
        .into_iter()
        .find(|r| str_of(r, "id") == id)
        .ok_or(format!("no line {id}"))
}

fn meeting_span(id: &str) -> Result<(f64, f64), String> {
    meetings::all()
        .into_iter()
        .find(|m| m.id == id)
        .map(|m| m.span())
        .ok_or(format!("no meeting {id}"))
}

fn retrain() -> Reply {
    ozen(&["retrain"])
}

/// Delete lines and everything keyed by them: fixes (relearned), tags (voiceprints retrained), labels.
fn delete(ids: &[String]) -> Reply {
    let all = rows();
    let unknown: Vec<&String> = ids
        .iter()
        .filter(|id| !all.iter().any(|r| str_of(r, "id") == *id))
        .collect();
    if !unknown.is_empty() {
        return Err(format!("no lines {unknown:?}; nothing deleted"));
    }
    let fixes = read(FIXES);
    for id in ids.iter().filter(|id| fixes.contains_key(*id)) {
        ozen(&["fix", id, ""])?; // while the line still exists: fix looks it up
    }
    let tags = read(TAGS);
    let tagged: Vec<String> = ids
        .iter()
        .filter(|id| {
            tags.get(*id)
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
        })
        .cloned()
        .collect();
    // An empty tag, not a removed one: it tells retraining to drop the line's sample from the registry.
    crate::ignore::tag(&tagged, "");
    let mut labels = read(LABELS);
    labels.retain(|id, _| !ids.contains(id));
    fs::write(LABELS, Value::Object(labels).to_string()).map_err(err)?;
    let mut n = 0;
    rewrite(|r| {
        if ids.iter().any(|id| id == str_of(r, "id")) {
            n += 1;
            Edit::Drop
        } else {
            Edit::Keep
        }
    })?;
    if !tagged.is_empty() {
        retrain()?;
    }
    Ok(format!(
        "deleted {n} lines (transcript.txt and kept chunk audio are not touched)"
    ))
}

/// Replace every tag `from` with `to` (empty clears them), then retrain.
fn retag(from: &str, to: &str) -> Reply {
    let n = crate::voices::retag(from, to)?;
    retrain()?;
    Ok(format!("retagged {n} lines"))
}

fn places() -> Vec<Value> {
    fs::read(PLACES)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Atomic like the menu bar's own save, so it never reads half a file.
fn save_places(p: &[Value]) -> Reply {
    let tmp = format!("{PLACES}.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(p).map_err(err)? + "\n").map_err(err)?;
    fs::rename(&tmp, PLACES).map_err(err)?;
    serde_json::to_string(p).map_err(err)
}

/// vocab.txt is words one per line or comma separated; it's written back comma separated.
fn vocab() -> Vec<String> {
    fs::read_to_string(VOCAB)
        .unwrap_or_default()
        .split([',', '\n'])
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect()
}

fn save_vocab(words: &[String]) -> Reply {
    fs::write(VOCAB, words.join(", ") + "\n").map_err(err)?;
    Ok(json!(words).to_string())
}

fn mode() -> String {
    Command::new("defaults")
        .args(["read", APP_ID, "mode"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or("always".into())
}

fn status() -> Reply {
    let (state, health) = (ozen(&["status"])?, ozen(&["health"])?);
    Ok(
        json!({"state": state, "mode": mode(), "problems": health.lines().collect::<Vec<_>>()})
            .to_string(),
    )
}

#[derive(Deserialize, JsonSchema)]
struct LineQuery {
    /// Only lines of this meeting (an id from list_meetings).
    meeting: Option<String>,
    /// Only lines at or after this unix time (seconds).
    since: Option<f64>,
    /// Only lines at or before this unix time (seconds).
    until: Option<f64>,
    /// Only lines whose text or speaker contains this, ignoring case.
    query: Option<String>,
    /// At most this many lines, the latest ones (default 200).
    limit: Option<usize>,
}

#[derive(Deserialize, JsonSchema)]
struct Id {
    /// A line id, like "1790520395366-mic-0".
    id: String,
}

#[derive(Deserialize, JsonSchema)]
struct Ids {
    /// Line ids.
    ids: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
struct MeetingId {
    /// A meeting id from list_meetings.
    id: String,
}

#[derive(Deserialize, JsonSchema)]
struct NewLine {
    text: String,
    /// Who it's from (default "Note").
    speaker: Option<String>,
    /// Unix time in seconds it belongs at (default now). A time inside a meeting puts the note in it.
    t: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
struct LineEdit {
    id: String,
    /// The right text. Empty clears an earlier fix.
    text: Option<String>,
    /// Who really said it. "Ignored" marks a voice to drop; empty clears the tag.
    speaker: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
struct Rename {
    from: String,
    to: String,
}

#[derive(Deserialize, JsonSchema)]
struct Name {
    name: String,
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum PlaceAction {
    /// Always record while here.
    Record,
    /// Record only while a meeting app uses the microphone.
    Meetings,
    /// Never record while here.
    Off,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct Place {
    /// Unique name; setting an existing label replaces that place.
    label: String,
    action: PlaceAction,
    #[serde(skip_serializing_if = "Option::is_none")]
    lat: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    lon: Option<f64>,
    /// Meters (default 150).
    #[serde(skip_serializing_if = "Option::is_none")]
    radius: Option<f64>,
}

#[derive(Deserialize, JsonSchema)]
struct Label {
    label: String,
}

#[derive(Deserialize, JsonSchema)]
struct Words {
    words: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Action {
    Start,
    Pause,
    Resume,
    Stop,
}

#[derive(Deserialize, JsonSchema)]
struct Control {
    /// start/resume: record and transcribe. pause: stop recording, keep the transcriber loaded.
    /// stop: stop recording, finish transcribing what's queued, then exit.
    action: Action,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Mode {
    /// Record whenever ozen is started.
    Always,
    /// Start and stop by themselves while a meeting app uses the microphone.
    Meetings,
}

#[derive(Deserialize, JsonSchema)]
struct SetMode {
    mode: Mode,
}

#[derive(Clone)]
pub struct Ozen {
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl Ozen {
    #[tool(
        description = "Recording state (recording | paused | stopping | stopped), record mode, and current problems.",
        annotations(read_only_hint = true)
    )]
    async fn status(&self) -> Reply {
        status()
    }

    #[tool(
        description = "Start, pause, resume or stop recording. Starting records the room's microphone."
    )]
    async fn control(&self, Parameters(p): Parameters<Control>) -> Reply {
        let a = match p.action {
            Action::Start => "start",
            Action::Pause => "pause",
            Action::Resume => "resume",
            Action::Stop => "stop",
        };
        ozen(&[a])?;
        status()
    }

    #[tool(
        description = "Set the record mode the menu bar app follows (places override it while you're at one)."
    )]
    async fn set_mode(&self, Parameters(p): Parameters<SetMode>) -> Reply {
        let m = match p.mode {
            Mode::Always => "always",
            Mode::Meetings => "meetings",
        };
        let ok = Command::new("defaults")
            .args(["write", APP_ID, "mode", m])
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            status()
        } else {
            Err("defaults write failed".into())
        }
    }

    #[tool(
        description = "Past meetings, newest first: id, start, minutes, lines, first words (tab separated).",
        annotations(read_only_hint = true)
    )]
    async fn list_meetings(&self) -> Reply {
        Ok(meetings::all()
            .iter()
            .rev()
            .map(|m| m.row())
            .collect::<Vec<_>>()
            .join("\n"))
    }

    #[tool(
        description = "Delete a meeting: all its lines, with their fixes, speaker tags and labels.",
        annotations(destructive_hint = true)
    )]
    async fn delete_meeting(&self, Parameters(p): Parameters<MeetingId>) -> Reply {
        let (a, b) = meeting_span(&p.id)?;
        let ids: Vec<String> = rows()
            .iter()
            .filter(|r| (a..=b).contains(&r["t"].as_f64().unwrap_or(0.0)))
            .map(|r| str_of(r, "id").to_string())
            .collect();
        delete(&ids)
    }

    #[tool(
        description = "Transcript lines in time order, as JSON: id, time, speaker (and whether a tag, the voice or the transcriber set it), text (fixed if fixed; heard is the original), note, junk (an old line Whisper invented on noise; the panel hides these).",
        annotations(read_only_hint = true)
    )]
    async fn list_lines(&self, Parameters(p): Parameters<LineQuery>) -> Reply {
        let span = p.meeting.as_deref().map(meeting_span).transpose()?;
        let t = |r: &Row| r["t"].as_f64().unwrap_or(0.0);
        let mut out = views(|r| {
            span.is_none_or(|(a, b)| (a..=b).contains(&t(r)))
                && p.since.is_none_or(|s| t(r) >= s)
                && p.until.is_none_or(|u| t(r) <= u)
        });
        if let Some(q) = p.query.map(|q| q.to_lowercase()) {
            out.retain(|v| {
                [&v["text"], &v["speaker"]]
                    .iter()
                    .any(|s| s.as_str().unwrap_or("").to_lowercase().contains(&q))
            });
        }
        let n = p.limit.unwrap_or(200);
        Ok(Value::from(out.split_off(out.len().saturating_sub(n))).to_string())
    }

    #[tool(
        description = "One transcript line by id.",
        annotations(read_only_hint = true)
    )]
    async fn get_line(&self, Parameters(p): Parameters<Id>) -> Reply {
        find(&p.id)?;
        Ok(views(|r| str_of(r, "id") == p.id).remove(0).to_string())
    }

    #[tool(
        description = "Add a note line to the transcript (a summary, an action item, a correction of the record). Returns the line."
    )]
    async fn create_line(&self, Parameters(p): Parameters<NewLine>) -> Reply {
        let t = p.t.unwrap_or_else(crate::now);
        let ms = (t * 1000.0) as i64;
        let ids: Vec<String> = rows().iter().map(|r| str_of(r, "id").to_string()).collect();
        let id = (0..)
            .map(|n| format!("{ms}-note-{n}"))
            .find(|id| !ids.contains(id))
            .unwrap();
        let row = json!({"id": id, "t": (t * 100.0).round() / 100.0, "src": "note",
                         "spk": p.speaker.unwrap_or("Note".into()), "text": p.text});
        append(row.as_object().unwrap().clone())?;
        Ok(views(|r| str_of(r, "id") == id).remove(0).to_string())
    }

    #[tool(
        description = "Fix a line's text and/or set who said it. Fixes teach the transcriber; speaker tags retrain voiceprints (takes a few seconds). Returns the line."
    )]
    async fn update_line(&self, Parameters(p): Parameters<LineEdit>) -> Reply {
        if p.text.is_none() && p.speaker.is_none() {
            return Err("give text, speaker or both".into());
        }
        let row = find(&p.id)?;
        if is_note(&row) {
            rewrite(|r| {
                if str_of(r, "id") != p.id {
                    return Edit::Keep;
                }
                let mut r = r.clone();
                if let Some(t) = &p.text {
                    r.insert("text".into(), json!(t));
                }
                if let Some(s) = &p.speaker {
                    r.insert("spk".into(), json!(s));
                }
                Edit::Set(r)
            })?;
        } else {
            if let Some(t) = &p.text {
                ozen(&["fix", &p.id, t])?;
            }
            if let Some(s) = &p.speaker {
                ozen(&["tag", &p.id, s])?;
            }
        }
        Ok(views(|r| str_of(r, "id") == p.id).remove(0).to_string())
    }

    #[tool(
        description = "Delete transcript lines, with their fixes, speaker tags and labels. All ids must exist.",
        annotations(destructive_hint = true)
    )]
    async fn delete_lines(&self, Parameters(p): Parameters<Ids>) -> Reply {
        delete(&p.ids)
    }

    #[tool(
        description = "People: lines tagged per name here (\"Ignored\" = voices to drop), and the voiceprints ozen matches with their sample counts. Add someone by tagging a line with update_line.",
        annotations(read_only_hint = true)
    )]
    async fn list_people(&self) -> Reply {
        let mut tagged = Map::new();
        for name in read(TAGS)
            .values()
            .filter_map(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            let n = tagged.get(name).and_then(Value::as_u64).unwrap_or(0);
            tagged.insert(name.into(), json!(n + 1));
        }
        Ok(json!({"tagged": tagged, "voices": read(STATS).get("people")}).to_string())
    }

    #[tool(description = "Rename a person: moves every line tagged `from` to `to`, then retrains.")]
    async fn rename_person(&self, Parameters(p): Parameters<Rename>) -> Reply {
        if p.to.trim().is_empty() {
            return Err("empty name: use delete_person".into());
        }
        retag(&p.from, &p.to)
    }

    #[tool(
        description = "Forget a person here: clears every tag with that name and retrains, which drops their voiceprint samples from these lines. Use \"Ignored\" to stop ignoring every ignored voice.",
        annotations(destructive_hint = true)
    )]
    async fn delete_person(&self, Parameters(p): Parameters<Name>) -> Reply {
        retag(&p.name, "")
    }

    #[tool(
        description = "Places: labeled locations that override the record mode while the Mac is there.",
        annotations(read_only_hint = true)
    )]
    async fn list_places(&self) -> Reply {
        Ok(Value::from(places()).to_string())
    }

    #[tool(
        description = "Create a place, or replace the one with the same label. A place without lat/lon is set from the Mac's location in the menu bar."
    )]
    async fn set_place(&self, Parameters(p): Parameters<Place>) -> Reply {
        if p.label.trim().is_empty() {
            return Err("empty label".into());
        }
        let mut all = places();
        let new = serde_json::to_value(&p).map_err(err)?;
        match all.iter_mut().find(|x| x["label"] == p.label.as_str()) {
            Some(old) => *old = new,
            None => all.push(new),
        }
        save_places(&all)
    }

    #[tool(
        description = "Delete a place by label.",
        annotations(destructive_hint = true)
    )]
    async fn delete_place(&self, Parameters(p): Parameters<Label>) -> Reply {
        let mut all = places();
        let n = all.len();
        all.retain(|x| x["label"] != p.label.as_str());
        if all.len() == n {
            return Err(format!("no place {:?}", p.label));
        }
        save_places(&all)
    }

    #[tool(
        description = "Vocabulary: names and terms the transcriber should spell right (it also learns words from fixes).",
        annotations(read_only_hint = true)
    )]
    async fn list_vocab(&self) -> Reply {
        Ok(json!(vocab()).to_string())
    }

    #[tool(
        description = "Add words to the vocabulary (existing ones are skipped). Returns the vocabulary."
    )]
    async fn add_vocab(&self, Parameters(p): Parameters<Words>) -> Reply {
        let mut all = vocab();
        for w in p
            .words
            .iter()
            .map(|w| w.trim())
            .filter(|w| !w.is_empty() && !w.contains(','))
        {
            if !all.iter().any(|x| x == w) {
                all.push(w.into());
            }
        }
        save_vocab(&all)
    }

    #[tool(
        description = "Remove words from the vocabulary. Returns the vocabulary.",
        annotations(destructive_hint = true)
    )]
    async fn remove_vocab(&self, Parameters(p): Parameters<Words>) -> Reply {
        let mut all = vocab();
        all.retain(|x| !p.words.iter().any(|w| w.trim() == x));
        save_vocab(&all)
    }
}

#[tool_handler(router = self.tool_router, instructions = "\
ozen records meetings on this Mac and transcribes them live. Data:
- lines: transcript lines (id, time, speaker, text). Meetings are runs of lines with under 10 minutes of silence between them; a meeting's id is its start time in seconds.
- speaker: a line's tag (set by you or the user) wins over the voiceprint guess. Tagging retrains the voiceprints. The name \"Ignored\" marks a voice to drop (a video playing nearby).
- text: fixing a line's text also teaches the transcriber the words and repeated corrections.
- notes: lines you create (src \"note\"); they have no voice, so editing one just rewrites it.
- places, vocab, mode: settings the menu bar app reads live.")]
impl ServerHandler for Ozen {}

pub fn serve() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
        .block_on(async {
            let server = Ozen {
                tool_router: Ozen::tool_router(),
            }
            .serve(rmcp::transport::stdio())
            .await
            .expect("mcp handshake");
            let _ = server.waiting().await;
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_drops_and_edits_under_the_lock_keeping_other_rows_byte_for_byte() {
        let _cwd = crate::CWD.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("ozen-mcp-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let keep = r#"{"id": "1-mic-0", "t": 1.0, "text": "שלום", "e": [1.0]}"#;
        fs::write(
            LINES,
            format!("{keep}\n{{\"id\": \"2-mic-0\", \"t\": 2.0}}\nnot json\n"),
        )
        .unwrap();
        append(
            json!({"id": "3-note-0", "t": 3.0, "text": "a"})
                .as_object()
                .unwrap()
                .clone(),
        )
        .unwrap();
        rewrite(|r| match str_of(r, "id") {
            "2-mic-0" => Edit::Drop,
            "3-note-0" => {
                let mut r = r.clone();
                r.insert("text".into(), json!("b"));
                Edit::Set(r)
            }
            _ => Edit::Keep,
        })
        .unwrap();
        let out = fs::read_to_string(LINES).unwrap();
        assert_eq!(
            out,
            format!(
                "{keep}\nnot json\n{}\n",
                r#"{"id":"3-note-0","t":3.0,"text":"b"}"#
            )
        );
        assert!(!is_note(&rows()[0]) && is_note(&rows()[1]));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn lines_the_transcriber_flagged_as_junk_say_so() {
        let _cwd = crate::CWD.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("ozen-mcp-junk-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        std::env::set_current_dir(&dir).unwrap();
        fs::write(
            LINES,
            "{\"id\": \"a\", \"t\": 1.0}\n{\"id\": \"b\", \"t\": 2.0}\n",
        )
        .unwrap();
        let flags = || {
            views(|_| true)
                .iter()
                .map(|v| v["junk"] == true)
                .collect::<Vec<_>>()
        };
        assert_eq!(flags(), [false, false]); // no junk.json yet
        fs::write(JUNK, r#"["b"]"#).unwrap();
        assert_eq!(flags(), [false, true]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_append_waiting_on_a_swapped_out_file_lands_in_the_new_one() {
        let _cwd = crate::CWD.lock().unwrap();
        let dir = std::env::temp_dir().join(format!("ozen-mcp-swap-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        std::env::set_current_dir(&dir).unwrap();
        fs::write(LINES, "old\n").unwrap();
        let held = locked().unwrap(); // like rewrite, mid-swap
        let writer = std::thread::spawn(|| append(Map::new()));
        std::thread::sleep(std::time::Duration::from_millis(100)); // writer is now blocked on the old file
        fs::write("new", "new\n").unwrap();
        fs::rename("new", LINES).unwrap();
        drop(held);
        writer.join().unwrap().unwrap();
        assert_eq!(fs::read_to_string(LINES).unwrap(), "new\n{}\n");
        fs::remove_dir_all(&dir).unwrap();
    }
}
