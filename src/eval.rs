//! `ozen eval`: score the learning settings (`fixes::Learn`) on a fixed set of spoken lines, to find the
//! sweet spot between Whisper spelling your terms right and inventing them where nobody said them.
//!
//!     ozen eval [--vocab 0,10,30,60] [--repeat 0,1,2,3] [--real] [--fresh]
//!
//! Cases are split in two. The "teach" lines play the part of your fixes: Whisper transcribes them with the
//! plain prompt, and each (heard, right text) pair becomes a fix. Each setting learns from those fixes only,
//! then is scored on the "test" lines it never saw, so a setting can't win by memorizing its own examples.
//! Test lines are of three kinds: lines with terms (did the term come out right), lines without terms (did
//! learning break normal speech) and quiet noise (did hint words get invented).
//!
//! Cases: eval/cases.jsonl, spoken by macOS's Hebrew voice into eval/audio/, or with --real your own fixes
//! (fixes/dataset.jsonl, split in half by a hash of each line). Whisper runs through asr.py, the code the live
//! transcriber uses, seeded per clip; results are cached in eval/cache.jsonl by audio, prompt and asr.py,
//! so reruns are identical and a sweep only transcribes what it hasn't seen. --fresh ignores the cache.
use crate::fixes::{LEARN, Learn, WORD, rules};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

#[derive(PartialEq)]
enum Kind {
    Term,    // has an English term inside Hebrew speech
    Control, // plain speech
    Noise,   // quiet noise: any word is invented
}

struct Case {
    id: String,
    teach: bool,
    audio: PathBuf,
    start: f64,
    duration: Option<f64>,
    text: String,
    kind: Kind,
}

/// FNV-1a: a stable hash (std's may change between Rust versions), for cache keys and the --real split.
fn fnv(bytes: &[u8], mut h: u64) -> u64 {
    for b in bytes {
        h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    h
}
const FNV0: u64 = 0xcbf29ce484222325;

fn tokens(s: &str) -> Vec<String> {
    WORD.find_iter(&s.to_lowercase())
        .map(|m| m.as_str().to_string())
        .collect()
}

fn kind(text: &str) -> Kind {
    if text.is_empty() {
        Kind::Noise
    } else if text.chars().any(|c| c.is_ascii_alphabetic()) {
        Kind::Term
    } else {
        Kind::Control
    }
}

/// Quiet noise just above the transcriber's silence gate (RMS 0.003), where Whisper hallucinates.
fn write_noise(path: &Path, secs: f64) -> Result<(), String> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| e.to_string())?;
    let mut x: u32 = 1; // fixed-seed LCG: the same noise every time
    for _ in 0..(secs * 16000.0) as usize {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        w.write_sample(((x >> 16) as i32 % 455 - 227) as i16)
            .map_err(|e| e.to_string())?; // uniform ±227: RMS ≈ 0.004
    }
    w.finalize().map_err(|e| e.to_string())
}

fn synthetic() -> Result<Vec<Case>, String> {
    fs::create_dir_all("eval/audio").map_err(|e| e.to_string())?;
    let mut cases = vec![];
    for row in fs::read_to_string("eval/cases.jsonl")
        .map_err(|e| format!("eval/cases.jsonl: {e}"))?
        .lines()
    {
        let r: Map<String, Value> = serde_json::from_str(row).map_err(|e| e.to_string())?;
        let id = r["id"].as_str().unwrap_or_default().to_string();
        let audio = PathBuf::from(format!("eval/audio/{id}.wav"));
        if !audio.exists() {
            if let Some(secs) = r.get("noise").and_then(Value::as_f64) {
                write_noise(&audio, secs)?;
            } else if !crate::ok(
                crate::cmd("say")
                    .args(["-v", "Carmit", "--data-format=LEI16@16000", "-o"])
                    .arg(&audio)
                    .arg(r["say"].as_str().unwrap_or_default()),
            ) {
                return Err("`say -v Carmit` failed: install the Hebrew voice in System Settings > Accessibility > Spoken Content".into());
            }
        }
        let text = r["text"].as_str().unwrap_or_default().to_string();
        cases.push(Case {
            teach: r["split"] == "teach",
            kind: kind(&text),
            id,
            audio,
            start: 0.0,
            duration: None,
            text,
        });
    }
    Ok(cases)
}

fn real() -> Result<Vec<Case>, String> {
    let rows = fs::read_to_string("fixes/dataset.jsonl")
        .map_err(|_| "no fixes/dataset.jsonl yet: fix some lines in the panel first")?;
    Ok(rows
        .lines()
        .filter_map(|row| {
            let r: Map<String, Value> = serde_json::from_str(row).ok()?;
            let (audio, start) = (
                r["audio"].as_str()?.to_string(),
                r["start"].as_f64().unwrap_or(0.0),
            );
            let id = format!("{audio}@{start}");
            let text = r["text"].as_str()?.to_string();
            Some(Case {
                teach: fnv(id.as_bytes(), FNV0).is_multiple_of(2),
                kind: kind(&text),
                audio: Path::new("fixes").join(audio),
                start,
                duration: r["duration"].as_f64(),
                text,
                id,
            })
        })
        .collect())
}

/// Runs asr.py once over all rows (one model load), in order.
pub(crate) fn worker(rows: &[Value]) -> Result<Vec<Value>, String> {
    if rows.is_empty() {
        return Ok(vec![]);
    }
    let mut child = crate::cmd("uv")
        .args(["run", "-q", "asr.py"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("uv: {e}"))?;
    let input: String = rows.iter().map(|r| r.to_string() + "\n").collect();
    let mut stdin = child.stdin.take().unwrap();
    let feeder = std::thread::spawn(move || stdin.write_all(input.as_bytes())); // no pipe deadlock on big batches
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = feeder.join();
    let replies: Vec<Value> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if !out.status.success() || replies.len() != rows.len() {
        return Err(format!(
            "asr.py answered {} of {} clips",
            replies.len(),
            rows.len()
        ));
    }
    Ok(replies)
}

struct Whisper {
    salt: u64,
    audio_hash: HashMap<PathBuf, u64>,
    cache: HashMap<u64, String>,
    ran: usize,
}

impl Whisper {
    fn new(fresh: bool) -> Result<Self, String> {
        let salt = fnv(&fs::read("asr.py").map_err(|e| e.to_string())?, FNV0); // a decode change invalidates the cache
        let cache = if fresh {
            HashMap::new()
        } else {
            fs::read_to_string("eval/cache.jsonl")
                .unwrap_or_default()
                .lines()
                .filter_map(|l| serde_json::from_str::<Value>(l).ok())
                .filter_map(|v| {
                    Some((
                        v["key"].as_str()?.parse().ok()?,
                        v["heard"].as_str()?.to_string(),
                    ))
                })
                .collect()
        };
        Ok(Whisper {
            salt,
            audio_hash: HashMap::new(),
            cache,
            ran: 0,
        })
    }

    fn key(&mut self, c: &Case, words: &[String]) -> Result<u64, String> {
        if !self.audio_hash.contains_key(&c.audio) {
            let h = fnv(
                &fs::read(&c.audio).map_err(|e| format!("{}: {e}", c.audio.display()))?,
                FNV0,
            );
            self.audio_hash.insert(c.audio.clone(), h);
        }
        let spec = format!(
            "{}|{}|{:?}|{}",
            self.audio_hash[&c.audio],
            c.start,
            c.duration,
            words.join("\u{1f}")
        );
        Ok(fnv(spec.as_bytes(), self.salt))
    }

    /// What Whisper hears for each (case, hint words), transcribing only what isn't cached.
    fn hear(&mut self, jobs: &[(&Case, &Vec<String>)]) -> Result<Vec<String>, String> {
        let keys = jobs
            .iter()
            .map(|(c, w)| self.key(c, w))
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = std::collections::HashSet::new();
        let todo: Vec<usize> = (0..jobs.len())
            .filter(|&i| !self.cache.contains_key(&keys[i]) && seen.insert(keys[i]))
            .collect();
        if !todo.is_empty() {
            eprintln!("transcribing {} clips…", todo.len());
            let rows: Vec<Value> = todo.iter().map(|&i| {
                let (c, w) = jobs[i];
                json!({"audio": c.audio, "start": c.start, "duration": c.duration, "words": w, "lang": "he"})
            }).collect();
            let mut log = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("eval/cache.jsonl")
                .map_err(|e| e.to_string())?;
            for (&i, reply) in todo.iter().zip(worker(&rows)?) {
                let heard = reply["heard"].as_str().unwrap_or_default().to_string();
                writeln!(
                    log,
                    "{}",
                    json!({"key": keys[i].to_string(), "heard": heard})
                )
                .map_err(|e| e.to_string())?;
                self.cache.insert(keys[i], heard);
                self.ran += 1;
            }
        }
        Ok(keys.iter().map(|k| self.cache[k].clone()).collect())
    }
}

#[derive(Default)]
struct Score {
    errors: usize, // word edits against the right text, over all spoken test lines
    words: usize,  // words in the right text of those lines
    control_errors: usize,
    control_words: usize,
    terms_hit: usize,
    terms: usize,
    invented: usize, // words on noise clips
}

fn score(cases: &[&Case], texts: &[String]) -> Score {
    let mut s = Score::default();
    for (c, got) in cases.iter().zip(texts) {
        let (want, got) = (tokens(&c.text), tokens(got));
        let e = strsim::generic_levenshtein(&want, &got);
        match c.kind {
            Kind::Noise => s.invented += got.len(),
            Kind::Control => {
                (s.control_errors, s.control_words) =
                    (s.control_errors + e, s.control_words + want.len())
            }
            Kind::Term => {
                for t in want
                    .iter()
                    .filter(|t| t.chars().any(|ch| ch.is_ascii_alphabetic()))
                {
                    s.terms += 1;
                    s.terms_hit += got.contains(t) as usize;
                }
            }
        }
        if c.kind != Kind::Noise {
            s.errors += e;
            s.words += want.len();
        }
    }
    s
}

fn list(args: &[String], flag: &str, default: &[usize]) -> Result<Vec<usize>, String> {
    match args.iter().position(|a| a == flag) {
        None => Ok(default.to_vec()),
        Some(i) => args
            .get(i + 1)
            .ok_or(format!("{flag} needs values, e.g. {flag} 0,10,30"))?
            .split(',')
            .map(|v| {
                v.trim()
                    .parse()
                    .map_err(|_| format!("{flag}: not a number: {v}"))
            })
            .collect(),
    }
}

fn pct(a: usize, b: usize) -> String {
    if b == 0 {
        "-".into()
    } else {
        format!("{:.1}%", 100.0 * a as f64 / b as f64)
    }
}

pub fn run(args: &[String]) -> Result<(), String> {
    let vocabs = list(args, "--vocab", &[0, 10, 30, 60])?;
    let repeats = list(args, "--repeat", &[0, 1, 2, 3])?;
    let cases = if args.iter().any(|a| a == "--real") {
        real()?
    } else {
        synthetic()?
    };
    let (teach, test): (Vec<&Case>, Vec<&Case>) = cases.iter().partition(|c| c.teach);
    if teach.is_empty() || test.is_empty() {
        return Err(format!(
            "need both teach and test lines, have {} and {}",
            teach.len(),
            test.len()
        ));
    }
    // The prompt the live transcriber starts from: vocab.txt. (The previous line is left out: it changes from
    // meeting to meeting, and the eval must not.)
    let base: Vec<String> = fs::read_to_string("vocab.txt")
        .unwrap_or_default()
        .split([',', '\n'])
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect();
    let mut whisper = Whisper::new(args.iter().any(|a| a == "--fresh"))?;

    // Your fixes: what Whisper hears on the teach lines today, and what was really said.
    let heard = whisper.hear(&teach.iter().map(|c| (*c, &base)).collect::<Vec<_>>())?;
    let pairs: Vec<(&str, &str)> = heard
        .iter()
        .zip(&teach)
        .map(|(h, c)| (h.as_str(), c.text.as_str()))
        .collect();

    // Hint words change what Whisper hears, so transcribe the test lines once per learned vocabulary.
    let prompts: Vec<Vec<String>> = vocabs
        .iter()
        .map(|&v| {
            let learned = rules(
                &pairs,
                Learn {
                    vocab: v,
                    repeat: 0,
                },
            );
            base.iter()
                .cloned()
                .chain(
                    learned["vocab"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .filter_map(|w| w.as_str().map(String::from)),
                )
                .collect()
        })
        .collect();
    let jobs: Vec<(&Case, &Vec<String>)> = prompts
        .iter()
        .flat_map(|p| test.iter().map(move |c| (*c, p)))
        .collect();
    let test_heard = whisper.hear(&jobs)?;

    // Corrections only rewrite text: apply each setting's with the transcriber's own code (src/text.rs), no Whisper needed.
    let grid: Vec<(usize, usize, Learn)> = vocabs
        .iter()
        .enumerate()
        .flat_map(|(vi, &v)| {
            repeats.iter().map(move |&r| {
                (
                    vi,
                    v,
                    Learn {
                        vocab: v,
                        repeat: r,
                    },
                )
            })
        })
        .collect();
    let mut texts: Vec<String> = vec![];
    for &(vi, _, cfg) in &grid {
        let rules = rules(&pairs, cfg);
        let replace = rules["replace"].as_object().cloned().unwrap_or_default();
        texts.extend(
            test_heard[vi * test.len()..(vi + 1) * test.len()]
                .iter()
                .map(|h| crate::text::corrected(h, &replace)),
        );
    }

    let n = |k: Kind| test.iter().filter(|c| c.kind == k).count();
    println!(
        "{} teach lines (as fixes), test: {} with terms, {} plain, {} noise · {} clips transcribed, rest cached",
        teach.len(),
        n(Kind::Term),
        n(Kind::Control),
        n(Kind::Noise),
        whisper.ran
    );
    println!(
        "{:>5} {:>6}  {:>7}  {:>9}  {:>9}  {:>8}",
        "vocab", "repeat", "WER", "terms", "plain WER", "invented"
    );
    let mut results: Vec<(Score, usize, usize, Vec<String>)> = grid
        .iter()
        .enumerate()
        .map(|(gi, &(_, v, cfg))| {
            let t = texts[gi * test.len()..(gi + 1) * test.len()].to_vec();
            (score(&test, &t), v, cfg.repeat, t)
        })
        .collect();
    // Best first: fewest word errors, then fewest invented words.
    results.sort_by(|a, b| {
        (a.0.errors * b.0.words.max(1))
            .cmp(&(b.0.errors * a.0.words.max(1)))
            .then(a.0.invented.cmp(&b.0.invented))
    });
    let mut report = vec![];
    for (s, v, r, t) in &results {
        let now = if *v == LEARN.vocab && *r == LEARN.repeat {
            "  ← current"
        } else {
            ""
        };
        println!(
            "{v:>5} {r:>6}  {:>7}  {:>9}  {:>9}  {:>8}{now}",
            pct(s.errors, s.words),
            format!("{}/{}", s.terms_hit, s.terms),
            pct(s.control_errors, s.control_words),
            s.invented
        );
        let lines: Map<String, Value> = test
            .iter()
            .zip(t)
            .map(|(c, got)| (c.id.clone(), json!({"want": c.text, "got": got})))
            .collect();
        report.push(json!({"vocab": v, "repeat": r, "lines": lines}));
    }
    fs::write(
        "eval/report.json",
        serde_json::to_string_pretty(&report).unwrap() + "\n",
    )
    .map_err(|e| e.to_string())?;
    println!("each line's output per setting: eval/report.json");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(text: &str) -> Case {
        Case {
            id: String::new(),
            teach: false,
            audio: PathBuf::new(),
            start: 0.0,
            duration: None,
            text: text.into(),
            kind: kind(text),
        }
    }

    #[test]
    fn scores_words_terms_and_invented_words() {
        let (term, plain, noise) = (case("תפתח PR ל-Kev"), case("בוא נקבע פגישה"), case(""));
        let s = score(
            &[&term, &plain, &noise],
            &[
                "תפתח פי אר ל-Kev".into(),
                "בוא נקבע פגישה".into(),
                "PR תודה".into(),
            ],
        );
        assert_eq!((s.terms_hit, s.terms), (1, 2)); // Kev right, PR heard as פי אר
        assert_eq!((s.errors, s.words), (2, 7)); // PR -> פי אר: one substitution + one insertion
        assert_eq!((s.control_errors, s.control_words), (0, 3));
        assert_eq!(s.invented, 2); // noise clips never count toward WER
    }
}
