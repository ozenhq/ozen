//! Past meetings, cut out of lines.jsonl, and context folders built from them for Claude Code or Hermes.
use chrono::{DateTime, Local};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};

pub const GAP: f64 = 600.0; // this much silence ends a meeting
const KEV: &str = "http://127.0.0.1:8009/v1/systemone"; // local Kev (System One API), see ~/dev/kev; OZEN_KEV overrides
const EXCERPT: usize = 1500; // chars of each candidate meeting Kev reads: its start and end
const PICKED_BUDGET: usize = 4000; // chars of the picked meetings shared by every question, split between them
const BATCH: usize = 8; // candidate meetings per Kev request, so prompts stay short however many meetings exist
const RELATED: f64 = 0.5;

pub struct Line {
    t: f64,
    text: String,
}

pub struct Meeting {
    pub id: String, // start time in whole seconds: stable while lines are only appended
    lines: Vec<Line>,
}

fn local(t: f64) -> DateTime<Local> {
    DateTime::from_timestamp(t as i64, 0)
        .unwrap_or_default()
        .with_timezone(&Local)
}

fn read_json(path: &str) -> Value {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null)
}

/// Every transcript line, time ordered, rendered like `ozen show`: speakers corrected by your tags.
fn lines() -> Vec<Line> {
    let raw = fs::read_to_string("lines.jsonl").unwrap_or_default();
    let (tags, labels) = (
        Value::Object(crate::crdt::read_map("tags.json")),
        read_json("labels.json"),
    );
    render(&raw, &tags, &labels, &read_json("junk.json"))
}

/// `lines()` on given contents. Skips the old Whisper echoes the transcriber listed in junk.json (the panel hides
/// them too), so an agent asked about a meeting doesn't read them as things people said.
fn render(raw: &str, tags: &Value, labels: &Value, junk: &Value) -> Vec<Line> {
    let junk: Vec<&str> = junk
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let mut out: Vec<Line> = raw
        .lines()
        .filter_map(|row| serde_json::from_str::<Value>(row).ok())
        .filter_map(|r| {
            let (id, t) = (r["id"].as_str()?, r["t"].as_f64()?);
            if junk.contains(&id) {
                return None;
            }
            let tag = tags[id].as_str().filter(|s| !s.is_empty());
            let spk = tag
                .or(labels[id]["spk"].as_str())
                .or(r["spk"].as_str())
                .unwrap_or("?");
            let mark = if tag.is_some() {
                " ✓"
            } else if labels[id]["unsure"] == true {
                " ?"
            } else {
                ""
            };
            let when = local(t).format("%H:%M:%S");
            let (src, text) = (
                r["src"].as_str().unwrap_or(""),
                r["text"].as_str().unwrap_or(""),
            );
            Some(Line {
                t,
                text: format!("[{when}] {spk}{mark} ({src}): {text}"),
            })
        })
        .collect();
    out.sort_by(|a, b| a.t.total_cmp(&b.t)); // call and mic chunks finish at different times
    out
}

pub fn split(lines: Vec<Line>) -> Vec<Meeting> {
    let mut out: Vec<Meeting> = vec![];
    for l in lines {
        match out.last_mut() {
            Some(m) if l.t - m.lines.last().unwrap().t <= GAP => m.lines.push(l),
            _ => out.push(Meeting {
                id: (l.t as i64).to_string(),
                lines: vec![l],
            }),
        }
    }
    out
}

pub fn all() -> Vec<Meeting> {
    split(lines())
}

impl Meeting {
    fn start(&self) -> f64 {
        self.lines[0].t
    }
    /// First and last line times: every line in between belongs to this meeting.
    pub fn span(&self) -> (f64, f64) {
        (self.start(), self.lines.last().unwrap().t)
    }
    fn file(&self) -> String {
        local(self.start()).format("%Y-%m-%d %H%M.md").to_string()
    }
    fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
    /// Start and end of the transcript within `budget` chars: what a meeting is about and where it landed.
    fn excerpt(&self, budget: usize) -> String {
        let chars: Vec<char> = self.text().chars().collect();
        if chars.len() <= budget {
            return chars.into_iter().collect();
        }
        let half = budget / 2;
        let (head, tail): (String, String) = (
            chars[..half].iter().collect(),
            chars[chars.len() - half..].iter().collect(),
        );
        format!("{head}\n[…]\n{tail}")
    }
    /// Tab separated for the menu bar: id, start, minutes, lines, first words.
    pub fn row(&self) -> String {
        let mins = ((self.lines.last().unwrap().t - self.start()) / 60.0).ceil() as i64;
        let first = self.lines[0].text.split_once("): ").map_or("", |(_, t)| t);
        let preview: String = first
            .chars()
            .take(80)
            .collect::<String>()
            .replace('\t', " ");
        let when = local(self.start()).format("%a %d %b %H:%M");
        format!(
            "{}\t{when}\t{mins}\t{}\t{preview}",
            self.id,
            self.lines.len()
        )
    }
}

/// Ask Kev which of `others` belong with `picked`: one yes/no question per meeting, `BATCH` per request.
fn kev_related(
    url: &str,
    picked: &[&Meeting],
    others: &[&Meeting],
) -> Result<Vec<(String, f64)>, String> {
    let share = (PICKED_BUDGET / picked.len().max(1)).max(300);
    let state = picked
        .iter()
        .map(|m| m.excerpt(share))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n");
    let mut scores = vec![];
    for batch in others.chunks(BATCH) {
        let questions: Map<String, Value> = batch
            .iter()
            .map(|m| {
                (m.id.clone(), json!({
                    "type": "noul",
                    "instructions": format!("The state holds the transcripts of meetings the user picked. Is this other meeting part of the same project, topic or thread of work, so its transcript would help answer questions about the picked ones?\n\nOther meeting:\n{}", m.excerpt(EXCERPT)),
                    "criteria": {"true": "Same project, topic, decision or people working on the same thing", "false": "A different subject; only shares small talk or common words"},
                }))
            })
            .collect();
        let answers = ask(
            url,
            &json!({"state": format!("Picked meetings:\n\n{state}"), "questions": questions}),
        )?;
        scores.extend(batch.iter().filter_map(|m| {
            answers["answers"][&m.id]["noul"]
                .as_f64()
                .map(|p| (m.id.clone(), p))
        }));
    }
    Ok(scores)
}

fn ask(url: &str, body: &Value) -> Result<Value, String> {
    let mut curl = Command::new("curl")
        .args([
            "-sS",
            "--fail-with-body",
            "-m",
            "300",
            "-H",
            "content-type: application/json",
            "--data-binary",
            "@-",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    curl.stdin
        .take()
        .unwrap()
        .write_all(body.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    let out = curl.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "Kev isn't answering at {url} ({}{}). Start it: cd ~/dev/kev && uv run --extra serve python -m kev.serve --run jaredpalmer/kev-4b --port 8009",
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        ));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("Kev reply: {e}"))
}

/// Write the picked meetings (plus Kev's related ones) into context/<now>/; returns the folder and the files.
pub fn gather(ids: &[String], kev: bool) -> Result<(String, Vec<String>), String> {
    let all = all();
    let by_id: HashMap<&str, &Meeting> = all.iter().map(|m| (m.id.as_str(), m)).collect();
    let picked: Vec<&Meeting> = ids
        .iter()
        .map(|id| {
            by_id
                .get(id.as_str())
                .copied()
                .ok_or(format!("no meeting {id}; see `ozen meetings`"))
        })
        .collect::<Result<_, _>>()?;
    if picked.is_empty() {
        return Err("pick at least one meeting".into());
    }
    let mut added = vec![];
    if kev {
        let others: Vec<&Meeting> = all.iter().filter(|m| !ids.contains(&m.id)).collect();
        let url = std::env::var("OZEN_KEV").unwrap_or_else(|_| KEV.into());
        for (id, p) in kev_related(&url, &picked, &others)? {
            eprintln!("kev: {} {p:.2}", by_id[id.as_str()].file());
            if p >= RELATED {
                added.push((by_id[id.as_str()], p));
            }
        }
    }
    let dir = format!(
        "{}/context/{}",
        crate::root(),
        Local::now().format("%Y-%m-%d-%H%M%S")
    );
    write_folder(&dir, &picked, kev.then_some(&added[..]), "")?;
    let files = picked
        .iter()
        .chain(added.iter().map(|(m, _)| m))
        .map(|m| m.file())
        .collect();
    Ok((dir, files))
}

/// The meeting still going at `now`: the latest one, if its last line is within `GAP`.
fn current(all: &[Meeting], now: f64) -> Option<&Meeting> {
    all.last()
        .filter(|m| now - m.lines.last().unwrap().t <= GAP)
}

/// Write the meeting happening now to context/live/ and return that folder. Each call refreshes the same
/// folder, so an agent started there can rerun it for the latest lines.
pub fn live(now: f64) -> Result<String, String> {
    let all = all();
    let m = current(&all, now).ok_or(
        "No meeting in the last 10 minutes. Start recording, or pick a past meeting in the Meetings tab",
    )?;
    let dir = format!("{}/context/live", crate::root());
    let ozen = std::env::current_exe().map_err(|e| e.to_string())?;
    let ozen = ozen.display();
    let note = format!(
        "\n\nThis meeting is still going. Its file is rewritten with the latest lines every 15 seconds, so reread it \
before answering; `{ozen} live` refreshes it right away, and `{ozen} look` gives a screenshot of the user's screen plus \
the last lines."
    );
    write_folder(&dir, &[m], None, &note)?;
    Ok(dir)
}

/// Start `what` (claude, hermes, or finder) in a folder `gather` or `live` wrote.
pub fn open(dir: &str, what: &str) -> Result<(), String> {
    let target = match what {
        "claude" | "hermes" => format!("{dir}/{what}.command"),
        "finder" => dir.to_string(),
        _ => return Err(format!("can't open {what}: use claude, hermes or finder")),
    };
    if !Path::new(&target).exists() {
        return Err(format!(
            "{target} doesn't exist; run `ozen gather` or `ozen live` first"
        ));
    }
    match Command::new("open").arg(&target).status() {
        Ok(s) if s.success() => Ok(()),
        r => Err(format!("open {target} failed: {r:?}")),
    }
}

/// The folder an agent starts in: one file per meeting, AGENTS.md/CLAUDE.md saying what they are, launchers.
fn write_folder(
    dir: &str,
    picked: &[&Meeting],
    added: Option<&[(&Meeting, f64)]>,
    note: &str,
) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // Refreshing a folder (live) replaces its transcripts; the folder itself stays, since an agent may be running in it.
    for e in fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.ends_with(".md") && name.starts_with(|c: char| c.is_ascii_digit()) {
            fs::remove_file(e.path()).map_err(|e| e.to_string())?;
        }
    }
    for m in picked
        .iter()
        .chain(added.unwrap_or_default().iter().map(|(m, _)| m))
    {
        fs::write(format!("{dir}/{}", m.file()), m.text() + "\n").map_err(|e| e.to_string())?;
    }
    let list = |ms: Vec<String>| {
        if ms.is_empty() {
            "none".into()
        } else {
            ms.join(", ")
        }
    };
    let kev_line = if let Some(added) = added {
        format!(
            "\nAdded by Kev as related (probability): {}",
            list(
                added
                    .iter()
                    .map(|(m, p)| format!("`{}` ({p:.2})", m.file()))
                    .collect()
            )
        )
    } else {
        String::new()
    };
    fs::write(format!("{dir}/AGENTS.md"), format!(
        "# Meeting transcripts\n\nTranscripts recorded by ozen, one meeting per `.md` file. Each line is \
`[time] speaker (source): text`. Source `room` is this Mac's microphone, `call` is the meeting app's audio. \
Speakers come from voiceprints: ✓ means the user confirmed it, ? means ozen is unsure.\n\n\
Picked by the user: {}{kev_line}{note}\n\nRead these transcripts before answering questions about the meetings.\n",
        list(picked.iter().map(|m| format!("`{}`", m.file())).collect()),
    )).map_err(|e| e.to_string())?;
    fs::write(format!("{dir}/CLAUDE.md"), "@AGENTS.md\n").map_err(|e| e.to_string())?; // Claude Code reads CLAUDE.md, Hermes AGENTS.md
    // Double-click (or Ozen's buttons) opens Terminal here with the agent running. Opening a .command file
    // needs no Automation permission, unlike scripting Terminal. Login + interactive so PATH finds the tools.
    for tool in ["claude", "hermes"] {
        let path = format!("{dir}/{tool}.command");
        fs::write(&path, command_script(tool)).map_err(|e| e.to_string())?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn command_script(tool: &str) -> String {
    // Hermes reads project context from TERMINAL_CWD when that's set (e.g. by the user's shell), not the launch dir
    format!("#!/bin/zsh -il\ncd \"${{0:A:h}}\" && export TERMINAL_CWD=\"$PWD\" && exec {tool}\n")
}

#[cfg(test)]
#[path = "meetings_tests.rs"]
mod tests;
