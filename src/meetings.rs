//! Past meetings, cut out of lines.jsonl, and context folders built from them for Claude Code or Hermes.
use chrono::{DateTime, Local};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

const GAP: f64 = 600.0; // this much silence ends a meeting
const KEV: &str = "http://127.0.0.1:8009/v1/systemone"; // local Kev (System One API), see ~/dev/kev
const EXCERPT: usize = 1500; // chars of each meeting Kev reads; ponytail: head only, summarize first if it misjudges long meetings
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
    DateTime::from_timestamp(t as i64, 0).unwrap_or_default().with_timezone(&Local)
}

fn read_json(path: &str) -> Value {
    fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null)
}

/// Every transcript line, time ordered, rendered like `train.py show`: speakers corrected by your tags.
fn lines() -> Vec<Line> {
    let (tags, labels) = (read_json("tags.json"), read_json("labels.json"));
    let raw = fs::read_to_string("lines.jsonl").unwrap_or_default();
    let mut out: Vec<Line> = raw
        .lines()
        .filter_map(|row| serde_json::from_str::<Value>(row).ok())
        .filter_map(|r| {
            let (id, t) = (r["id"].as_str()?, r["t"].as_f64()?);
            let tag = tags[id].as_str().filter(|s| !s.is_empty());
            let spk = tag.or(labels[id]["spk"].as_str()).or(r["spk"].as_str()).unwrap_or("?");
            let mark = if tag.is_some() { " ✓" } else if labels[id]["unsure"] == true { " ?" } else { "" };
            let when = local(t).format("%H:%M:%S");
            let (src, text) = (r["src"].as_str().unwrap_or(""), r["text"].as_str().unwrap_or(""));
            Some(Line { t, text: format!("[{when}] {spk}{mark} ({src}): {text}") })
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
            _ => out.push(Meeting { id: (l.t as i64).to_string(), lines: vec![l] }),
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
    fn file(&self) -> String {
        local(self.start()).format("%Y-%m-%d %H%M.md").to_string()
    }
    fn text(&self) -> String {
        self.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    }
    fn excerpt(&self) -> String {
        self.text().chars().take(EXCERPT).collect()
    }
    /// Tab separated for the menu bar: id, start, minutes, lines, first words.
    pub fn row(&self) -> String {
        let mins = ((self.lines.last().unwrap().t - self.start()) / 60.0).ceil() as i64;
        let first = self.lines[0].text.split_once("): ").map_or("", |(_, t)| t);
        let preview: String = first.chars().take(80).collect::<String>().replace('\t', " ");
        let when = local(self.start()).format("%a %d %b %H:%M");
        format!("{}\t{when}\t{mins}\t{}\t{preview}", self.id, self.lines.len())
    }
}

/// Ask Kev which of `others` belong with `picked`: one yes/no question per meeting, answered in one call.
fn kev_related(picked: &[&Meeting], others: &[&Meeting]) -> Result<Vec<(String, f64)>, String> {
    if others.is_empty() {
        return Ok(vec![]);
    }
    let state = picked.iter().map(|m| m.excerpt()).collect::<Vec<_>>().join("\n\n---\n\n");
    let questions: Map<String, Value> = others
        .iter()
        .map(|m| {
            (m.id.clone(), json!({
                "type": "noul",
                "instructions": format!("The state holds the transcripts of meetings the user picked. Is this other meeting part of the same project, topic or thread of work, so its transcript would help answer questions about the picked ones?\n\nOther meeting:\n{}", m.excerpt()),
                "criteria": {"true": "Same project, topic, decision or people working on the same thing", "false": "A different subject; only shares small talk or common words"},
            }))
        })
        .collect();
    let body = json!({"state": format!("Picked meetings:\n\n{state}"), "questions": questions}).to_string();
    let mut curl = Command::new("curl")
        .args(["-sS", "--fail-with-body", "-m", "300", "-H", "content-type: application/json", "--data-binary", "@-", KEV])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    curl.stdin.take().unwrap().write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    let out = curl.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "Kev isn't answering at {KEV} ({}{}). Start it: cd ~/dev/kev && uv run --extra serve python -m kev.serve --run jaredpalmer/kev-4b --port 8009",
            String::from_utf8_lossy(&out.stderr).trim(),
            String::from_utf8_lossy(&out.stdout).trim()
        ));
    }
    let answers: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("Kev reply: {e}"))?;
    Ok(others
        .iter()
        .filter_map(|m| answers["answers"][&m.id]["noul"].as_f64().map(|p| (m.id.clone(), p)))
        .collect())
}

/// Write the picked meetings (plus Kev's related ones) into context/<now>/ and return that folder.
pub fn gather(ids: &[String], kev: bool) -> Result<String, String> {
    let all = all();
    let by_id: HashMap<&str, &Meeting> = all.iter().map(|m| (m.id.as_str(), m)).collect();
    let picked: Vec<&Meeting> = ids
        .iter()
        .map(|id| by_id.get(id.as_str()).copied().ok_or(format!("no meeting {id}; see `ozen meetings`")))
        .collect::<Result<_, _>>()?;
    if picked.is_empty() {
        return Err("pick at least one meeting".into());
    }
    let mut added = vec![];
    if kev {
        let others: Vec<&Meeting> = all.iter().filter(|m| !ids.contains(&m.id)).collect();
        for (id, p) in kev_related(&picked, &others)? {
            eprintln!("kev: {} {p:.2}", by_id[id.as_str()].file());
            if p >= RELATED {
                added.push((by_id[id.as_str()], p));
            }
        }
    }
    let dir = format!("{}/context/{}", env!("CARGO_MANIFEST_DIR"), Local::now().format("%Y-%m-%d-%H%M%S"));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for m in picked.iter().chain(added.iter().map(|(m, _)| m)) {
        fs::write(format!("{dir}/{}", m.file()), m.text() + "\n").map_err(|e| e.to_string())?;
    }
    let list = |ms: Vec<String>| if ms.is_empty() { "none".into() } else { ms.join(", ") };
    let kev_line = if kev {
        format!("\nAdded by Kev as related (probability): {}", list(added.iter().map(|(m, p)| format!("`{}` ({p:.2})", m.file())).collect()))
    } else {
        String::new()
    };
    fs::write(format!("{dir}/AGENTS.md"), format!(
        "# Meeting transcripts\n\nTranscripts recorded by ozen, one meeting per `.md` file. Each line is \
`[time] speaker (source): text`. Source `room` is this Mac's microphone, `call` is the meeting app's audio. \
Speakers come from voiceprints: ✓ means the user confirmed it, ? means ozen is unsure.\n\n\
Picked by the user: {}{kev_line}\n\nRead these transcripts before answering questions about the meetings.\n",
        list(picked.iter().map(|m| format!("`{}`", m.file())).collect()),
    )).map_err(|e| e.to_string())?;
    fs::write(format!("{dir}/CLAUDE.md"), "@AGENTS.md\n").map_err(|e| e.to_string())?; // Claude Code reads CLAUDE.md, Hermes AGENTS.md
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_silence_starts_a_new_meeting() {
        let l = |t: f64| Line { t, text: String::new() };
        let ms = split(vec![l(100.0), l(100.0 + GAP), l(101.0 + 2.0 * GAP), l(102.0 + 2.0 * GAP)]);
        assert_eq!(ms.iter().map(|m| (m.id.as_str(), m.lines.len())).collect::<Vec<_>>(), [("100", 2), ("1301", 2)]);
    }
}
