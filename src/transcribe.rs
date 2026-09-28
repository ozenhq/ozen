//! `ozen transcribe CHUNKS OUT`: the live transcriber. Watches the chunk dir; transcribes call + mic chunks with
//! Whisper (src/whisper.rs), drops mic echo of call/local (computer) audio, labels each line by speaker (ECAPA
//! voiceprints, src/ecapa.rs), splits people talking at once (src/overlap.rs), and appends to OUT and lines.jsonl
//! (with voiceprints, for tagging in the menu bar panel). Ported from transcribe.py.
use crate::ecapa::Ecapa;
use crate::ignore::{IGNORE, MARGIN as IGNORE_MARGIN};
use crate::overlap::{self, SILENCE_RMS, SR, dot, frame_rms, percentile, rms};
use crate::separate::Separator;
use crate::text;
use crate::whisper::Whisper;
use indexmap::IndexMap;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant, SystemTime};

const VOCAB: &str = "vocab.txt"; // names/terms Whisper should spell right (Kev, PR, ...); one per line or comma-separated
const LEARNED: &str = "learned.json"; // from your transcript fixes (ozen fix): words to hint, corrections to apply
const ECHO_OVERLAP: f64 = 0.6; // mic turn mostly overlapping speaker output = echo, not a person in the room
const ECHO_PAD: f64 = 0.3; // seconds; slack for capture-latency differences between streams
const DEFAULT_SAME: f32 = 0.4; // cosine similarity cutoff; replaced by the one src/train.rs calibrates from your tags
const MIN_EMBED: usize = SR; // 1s: shorter clips give unreliable voiceprints: they never create or update a voice
const SHORT_MARGIN: f32 = 0.1; // a short clip needs the cutoff + this to take an existing label (else "?")
const REGISTRY: &str = "voices"; // clone of the voices registry, rebuilt by src/train.rs from your tags
const RECENT: &str = "recent"; // last KEEP_AUDIO transcribed chunks (computer audio in recent/local), local only
const PACE: &str = "pace.jsonl"; // one row per chunk done: when, how long it took; `ozen timebar` reads it
const PACE_DAYS: f64 = 14.0; // rows kept (~0.6 MB per 8h recording day); older history shows from lines.jsonl
const LINES: &str = "lines.jsonl"; // every transcript line with its voiceprint; the panel tags these
const IGNORES: &str = "ignore.json"; // prints of voices you ignored; `ozen retrain` (src/ignore.rs) writes them
const PULL_EVERY: Duration = Duration::from_secs(300); // registry pulls, so tags made on other Macs arrive
const LANG: &str = "he"; // until a source's first line picks one

fn source(tag: &str) -> &str {
    match tag {
        "mic" => "room",
        t => t,
    }
}

fn anon(label: &str) -> bool {
    label.len() > 1 && label.starts_with('S') && label[1..].bytes().all(|b| b.is_ascii_digit())
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

fn round(x: f64, digits: i32) -> f64 {
    let k = 10f64.powi(digits);
    (x * k).round() / k
}

fn unit(v: &[f32]) -> Vec<f32> {
    let n = dot(v, v).sqrt();
    v.iter().map(|x| x / n).collect()
}

struct Speaker {
    label: String,
    e: Vec<f32>,
    count: f64,
}

#[derive(Default, serde::Deserialize)]
struct Learned {
    #[serde(default)]
    vocab: Vec<String>,
    #[serde(default)]
    replace: IndexMap<String, Value>, // in file order, as the fixes wrote them
}

fn learned() -> Learned {
    fs::read(LEARNED)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The vocabulary + words from your fixes, given to Whisper so it spells them. Not the known people's names: on
/// room noise Whisper read them back as lines ("אורן דן, בן נחושתן, תודה רבה."); add a name to vocab.txt if it
/// keeps coming out wrong.
fn hint_words() -> Vec<String> {
    let vocab = fs::read_to_string(VOCAB).unwrap_or_default();
    vocab
        .split([',', '\n'])
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(String::from)
        .chain(learned().vocab)
        .collect()
}

/// 16 kHz mono f32 through ffmpeg (the chunks are 48 kHz float), as asr.py and mlx_whisper did.
pub fn load_audio(path: &Path) -> Result<Vec<f32>, String> {
    let out = crate::cmd("ffmpeg")
        .args(["-nostdin", "-i"])
        .arg(path)
        .args([
            "-threads",
            "0",
            "-f",
            "s16le",
            "-ac",
            "1",
            "-acodec",
            "pcm_s16le",
            "-ar",
            "16000",
            "-",
        ])
        .output()
        .map_err(|e| format!("ffmpeg: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "Failed to load audio: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(out
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b) as f32 / 32768.0)
        .collect())
}

/// lines.jsonl for appending, locked. `ozen mcp` deletes lines by swapping in a rewritten file while it holds this
/// lock, so a lock won on the swapped-out file is retried on the current one.
fn open_lines() -> std::io::Result<File> {
    loop {
        let f = OpenOptions::new().create(true).append(true).open(LINES)?;
        f.lock()?;
        if fs::metadata(LINES).is_ok_and(|m| m.ino() == f.metadata().map_or(0, |m| m.ino())) {
            return Ok(f);
        }
    }
}

/// Free bytes on the disk holding `path`, from `df -k`.
fn free_bytes(path: &Path) -> Option<u64> {
    let out = crate::cmd("df").arg("-k").arg(path).output().ok()?;
    let kb: u64 = String::from_utf8_lossy(&out.stdout)
        .lines()
        .nth(1)?
        .split_whitespace()
        .nth(3)?
        .parse()
        .ok()?;
    Some(kb * 1024)
}

struct Transcriber {
    whisper: Whisper,
    ecapa: Ecapa,
    separator: Arc<OnceLock<Separator>>, // loaded in the background; clips stay whole until then
    speakers: Vec<Speaker>,              // named ones come from the registry
    ignored: Vec<Vec<f32>>,
    same: f32,
    registry_mtime: SystemTime,
    last_pull: Option<Instant>,
    unknown: usize,
    run: u64, // S1, S2... are per run: lines carry it so the panel can group a label's lines
    last_lang: HashMap<String, String>, // per source; short clips reuse it
    last_text: HashMap<String, String>, // per source; previous line, given to Whisper as context
    active: Vec<(f64, f64, Option<Vec<f32>>)>, // when call or local audio played, with its print (None under 1s)
    covered: HashMap<&'static str, f64>, // end time of the latest chunk seen per reference stream
    floors: HashMap<String, Vec<f32>>,   // per source: quietest-frame level of recent chunks
}

impl Transcriber {
    fn embed(&self, clip: &[f32]) -> Vec<f32> {
        match self.ecapa.embed(clip) {
            Ok(e) => unit(&e),
            Err(e) => panic!("ecapa: {e}"), // a broken model: ozen restarts the transcriber
        }
    }

    /// (Re)load named voiceprints; src/train.rs rewrites them after every tag, so pick up changes live.
    fn load_registry(&mut self) {
        if self.last_pull.is_none_or(|t| t.elapsed() > PULL_EVERY) {
            self.last_pull = Some(Instant::now());
            // ponytail: offline or diverged just keeps the local prints; src/train.rs reconciles on its next push
            if let Ok(mut c) = crate::cmd("git")
                .args(["-C", REGISTRY, "pull", "--ff-only", "-q"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                let t = Instant::now();
                while matches!(c.try_wait(), Ok(None)) && t.elapsed() < Duration::from_secs(30) {
                    std::thread::sleep(Duration::from_millis(100));
                }
                let _ = c.kill();
                let _ = c.wait();
            }
        }
        let dir = Path::new(REGISTRY).join("voices");
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        let config = Path::new(REGISTRY).join("config.json");
        let mtime = files
            .iter()
            .chain([&config, &PathBuf::from(IGNORES)])
            .filter_map(|f| fs::metadata(f).and_then(|m| m.modified()).ok())
            .max()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if mtime == self.registry_mtime {
            return;
        }
        self.registry_mtime = mtime;
        let read = |p: &Path| {
            fs::read(p)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        };
        if let Some(s) = read(&config).and_then(|c| c["same_speaker"].as_f64()) {
            self.same = s as f32;
        }
        if let Some(Value::Array(prints)) = read(Path::new(IGNORES)) {
            self.ignored = prints
                .iter()
                .filter_map(|p| p.as_array())
                .map(|p| {
                    unit(
                        &p.iter()
                            .filter_map(Value::as_f64)
                            .map(|x| x as f32)
                            .collect::<Vec<_>>(),
                    )
                })
                .collect();
        }
        for f in &files {
            let Some(v) = read(f) else { continue };
            if v["model"] != crate::train::ECAPA {
                continue;
            }
            let (Some(name), Some(e)) = (v["name"].as_str(), v["embedding"].as_array()) else {
                continue;
            };
            let e = unit(
                &e.iter()
                    .filter_map(Value::as_f64)
                    .map(|x| x as f32)
                    .collect::<Vec<_>>(),
            );
            let count = v["count"].as_f64().unwrap_or(1.0);
            match self
                .speakers
                .iter_mut()
                .find(|s| s.label == name && !anon(name))
            {
                Some(s) => (s.e, s.count) = (e, count),
                None => self.speakers.push(Speaker {
                    label: name.into(),
                    e,
                    count,
                }),
            }
        }
        let known: Vec<&str> = self
            .speakers
            .iter()
            .filter(|s| !anon(&s.label))
            .map(|s| s.label.as_str())
            .collect();
        println!(
            "known voices: {known:?}, {} ignored lines, threshold {}",
            self.ignored.len(),
            self.same
        );
    }

    /// The speaker whose print is closest to e, the first on ties.
    fn best<'a>(
        &self,
        e: &[f32],
        among: impl Iterator<Item = &'a Speaker>,
    ) -> Option<(usize, f32)> {
        among
            .enumerate()
            .map(|(i, s)| (i, dot(&s.e, e)))
            .fold(None, |b, (i, s)| {
                if b.is_none_or(|(_, bs)| s > bs) {
                    Some((i, s))
                } else {
                    b
                }
            })
    }

    fn who(&mut self, clip: &[f32], e: &[f32]) -> String {
        // ponytail: nearest ignored line, O(ignored lines) per utterance; fine for thousands
        let near = self.ignored.iter().map(|p| dot(p, e)).fold(-1f32, f32::max);
        let named = self
            .speakers
            .iter()
            .filter(|s| !anon(&s.label))
            .map(|s| dot(&s.e, e))
            .fold(-1f32, f32::max);
        if !self.ignored.is_empty() && near >= self.same + IGNORE_MARGIN && near > named {
            return IGNORE.into(); // closer to a voice you ignored than to anyone you know
        }
        if clip.len() < MIN_EMBED {
            // A short clip's print is too noisy to found or reshape a voice: label it only on a strong match.
            return match self.best(e, self.speakers.iter()) {
                Some((i, s)) if s >= self.same + SHORT_MARGIN => self.speakers[i].label.clone(),
                _ => "?".into(),
            };
        }
        if let Some((i, s)) = self.best(e, self.speakers.iter())
            && s >= self.same
        {
            let b = &mut self.speakers[i];
            if anon(&b.label) {
                // named prints change only through tagging (src/train.rs)
                let c: Vec<f32> =
                    b.e.iter()
                        .zip(e)
                        .map(|(x, y)| x * b.count as f32 + y)
                        .collect();
                (b.e, b.count) = (unit(&c), b.count + 1.0);
            }
            return b.label.clone();
        }
        self.unknown += 1;
        let label = format!("S{}", self.unknown);
        self.speakers.push(Speaker {
            label: label.clone(),
            e: e.to_vec(),
            count: 1.0,
        });
        label
    }

    /// How unsure src/train.rs would be about this line (same formula), so Review can ask before the next retrain.
    fn doubt(&self, e: &[f32]) -> Option<f64> {
        let mut sims: Vec<f32> = self
            .speakers
            .iter()
            .filter(|s| !anon(&s.label))
            .map(|s| dot(&s.e, e))
            .collect();
        sims.sort_by(|a, b| b.total_cmp(a));
        let top = *sims.first()?;
        let margin = if sims.len() > 1 {
            top - sims[1]
        } else {
            top - self.same
        };
        Some(round((top - self.same).abs().min(margin) as f64, 3))
    }

    fn transcribe(&mut self, clip: &[f32], tag: &str) -> Result<String, String> {
        let words = hint_words(); // the prompt adds the previous line as context
        let prompt = text::prompt(&words, self.last_text.get(tag).map_or("", String::as_str));
        let lang = self
            .last_lang
            .get(tag)
            .map_or(LANG, String::as_str)
            .to_string();
        let (heard, lang, _) = self
            .whisper
            .decode(clip, &prompt, &lang, None)
            .map_err(|e| e.to_string())?;
        let t = text::kept(&heard, &words);
        self.last_lang.insert(tag.into(), lang);
        if !t.is_empty() {
            self.last_text.insert(tag.into(), t.clone());
        }
        Ok(t)
    }

    /// How much of a mic turn the call/computer audio overlaps, and how close the turn's voice is to the voices
    /// playing then (None when none of them has a print). Echo sounds like its source (~0.8 on speaker bleed);
    /// you talking over a remote speaker doesn't.
    fn echo(&self, t0: f64, t1: f64, e: Option<&[f32]>) -> (f64, Option<f32>) {
        let hits: Vec<(f64, &Option<Vec<f32>>)> = self
            .active
            .iter()
            .map(|(a, b, p)| (0f64.max(t1.min(b + ECHO_PAD) - t0.max(a - ECHO_PAD)), p))
            .collect();
        let near = e.and_then(|e| {
            hits.iter()
                .filter(|(d, _)| *d > 0.0)
                .filter_map(|(_, p)| p.as_ref().map(|p| dot(p, e)))
                .reduce(f32::max)
        });
        (
            hits.iter().fold(0.0, |s, (d, _)| s + d) / (t1 - t0).max(1e-6), // 0.0, not sum()'s -0.0
            near,
        )
    }

    /// The source's background level: median over its last 20 chunks of each chunk's 10th-percentile frame. Pauses
    /// between words keep that percentile at room level even in a busy chunk, and the median rides out chunks that
    /// are speech from end to end.
    fn noise_floor(&mut self, tag: &str, audio: &[f32]) -> f32 {
        let r = frame_rms(audio);
        let hist = self.floors.entry(tag.into()).or_default();
        if !r.is_empty() {
            hist.push(percentile(&r, 10.0));
            let n = hist.len();
            hist.drain(..n.saturating_sub(20));
        }
        if hist.is_empty() {
            0.0
        } else {
            percentile(hist, 50.0)
        }
    }

    /// One chunk: its lines written, or an error (ENOSPC: keep the chunk for later).
    fn chunk(
        &mut self,
        f: &Path,
        ms: &str,
        tag: &str,
        chunks: &Path,
        out: &Path,
    ) -> Result<(f64, usize), std::io::Error> {
        let t_chunk = ms.parse::<f64>().unwrap_or(0.0) / 1000.0;
        let other = |e: String| std::io::Error::other(e);
        let audio = load_audio(f).map_err(other)?;
        let sec = audio.len() as f64 / SR as f64;
        if let Some(c) = self.covered.get_mut(tag) {
            *c = c.max(t_chunk + sec);
        }
        if audio.is_empty() || rms(&audio) <= SILENCE_RMS {
            return Ok((sec, 0));
        }
        // (start, speaker, text, print sum, end), merged while the speaker repeats
        let mut lines: Vec<(f64, String, String, Vec<f32>, f64)> = vec![];
        let mut prev: Option<String> = None;
        let separate = tag != "local" && wavs(chunks).len() <= separate_backlog();
        let floor = self.noise_floor(tag, &audio);
        let sep = Arc::clone(&self.separator);
        for (u, c) in overlap::utterances(&audio, floor, 0.3) {
            let pieces = if separate {
                let same = self.same;
                let ecapa = &self.ecapa;
                let mut embed = |x: &[f32]| unit(&ecapa.embed(x).expect("ecapa"));
                overlap::voices(c, sep.get(), &mut embed, same)
            } else {
                vec![(0.0, c.to_vec())]
            };
            for (o, clip) in pieces {
                let start = u + o;
                let (t0, t1) = (
                    t_chunk + start,
                    t_chunk + start + clip.len() as f64 / SR as f64,
                );
                let long = clip.len() >= MIN_EMBED; // shorter prints are too noisy to judge echo by
                let e = (long || tag != "local").then(|| self.embed(&clip));
                if tag != "mic" {
                    self.active.push((t0, t1, e.clone().filter(|_| long)));
                }
                if tag == "local" {
                    continue; // computer's own audio: reference only, never transcribed
                }
                let e = e.expect("print for call and mic turns");
                if tag == "mic" {
                    let (share, near) = self.echo(t0, t1, long.then_some(e.as_slice()));
                    if share >= ECHO_OVERLAP && near.is_none_or(|n| n >= self.same) {
                        println!("echo dropped {:.1}s", t1 - t0);
                        continue; // speakers leaking into the mic
                    }
                    // Evidence for a missed echo: how much computer audio overlapped, how close its voice was, and
                    // whether the local reference even reached this far (negative = it hadn't been read yet).
                    println!(
                        "mic kept {:.1}s in {}: echo overlap {share:.2}, voice similarity {}, local reference {:+.1}s past it",
                        t1 - t0,
                        f.file_name().unwrap_or_default().to_string_lossy(),
                        near.map_or("-".into(), |n| format!("{n:.2}")),
                        self.covered["local"] - t1
                    );
                }
                let text = self.transcribe(&clip, tag).map_err(other)?;
                if text.is_empty() {
                    continue;
                }
                let mut spk = self.who(&clip, &e);
                if spk == IGNORE {
                    println!("ignored voice dropped {:.1}s", t1 - t0);
                    continue;
                }
                if spk == "?"
                    && let Some(p) = &prev
                {
                    spk = p.clone(); // short clip mid-turn: most likely the same person continuing
                }
                let w = if long {
                    clip.len() as f32 / SR as f32
                } else {
                    0.01
                }; // short clips barely count
                let end = start + clip.len() as f64 / SR as f64;
                match lines.last_mut() {
                    Some(l) if l.1 == spk => {
                        l.2 = format!("{} {text}", l.2);
                        l.3.iter_mut().zip(&e).for_each(|(a, b)| *a += w * b);
                        l.4 = end;
                    }
                    _ => lines.push((
                        start,
                        spk.clone(),
                        text,
                        e.iter().map(|x| w * x).collect(),
                        end,
                    )),
                }
                prev = Some(spk);
            }
        }
        // room for every line or none, so a retry never repeats one
        if free_bytes(
            out.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )
        .is_some_and(|b| b < 16 << 20)
        {
            return Err(std::io::Error::from_raw_os_error(28)); // ENOSPC
        }
        let mut fh = OpenOptions::new().create(true).append(true).open(out)?;
        let mut lj = open_lines()?;
        let replace = learned().replace;
        let mut wrote = 0;
        for (i, (start, spk, heard, esum, end)) in lines.into_iter().enumerate() {
            if text::looped(&heard) {
                continue;
            }
            let text = text::corrected(&heard, &replace);
            let ts = chrono::DateTime::from_timestamp_millis(((t_chunk + start) * 1000.0) as i64)
                .map(|t| {
                    t.with_timezone(&chrono::Local)
                        .format("%H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default();
            let line = format!("[{ts}] {spk} ({}): {text}", source(tag));
            writeln!(fh, "{line}")?;
            println!("{line}");
            let e = unit(&esum);
            let mut rec = json!({
                "id": format!("{ms}-{tag}-{i}"), "t": round(t_chunk + start, 2), "d": round(end - start, 2),
                "src": source(tag), "run": self.run, "spk": spk, "text": text,
                "e": e.iter().map(|x| (x * 1e5).round() / 1e5).collect::<Vec<f32>>(),
            });
            // under MIN_EMBED the print is noise: tagging such a line teaches nothing, so never ask
            if end - start >= 1.0
                && let Some(d) = self.doubt(&e)
            {
                rec["doubt"] = json!(d);
            }
            if text != heard {
                rec["heard"] = json!(heard); // what Whisper said; fixes learn from this, not the correction
            }
            writeln!(lj, "{rec}")?;
            wrote += 1;
        }
        Ok((sec, wrote))
    }
}

/// Separating people talking at once costs ~0.75x real time per such utterance: skip it while more than this many
/// chunks wait (two 15s windows), so it never makes the transcript fall behind; those lines stay merged.
fn separate_backlog() -> usize {
    std::env::var("OZEN_SEPARATE_BACKLOG")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(6)
}

fn start_ms(f: &Path) -> i64 {
    f.file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| s.split('-').next())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn wavs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect()
}

/// Newest first, so the live meeting is transcribed before any backlog. The mic chunk of a 15s window sorts after
/// that window's call/local chunks (started ms apart), which its echo check needs.
fn pending(dir: &Path) -> Vec<PathBuf> {
    let key = |f: &PathBuf| {
        start_ms(f)
            - if f.to_string_lossy().ends_with("-mic.wav") {
                1000
            } else {
                0
            }
    };
    let mut v = wavs(dir);
    v.sort_by_key(|f| std::cmp::Reverse(key(f)));
    v
}

/// Drop rows older than PACE_DAYS, once per transcriber start, so the Timebar stays quick to load.
fn trim_pace() {
    let cutoff = now() - PACE_DAYS * 86400.0;
    let Ok(raw) = fs::read_to_string(PACE) else {
        return;
    };
    let rows: Vec<&str> = raw.lines().collect();
    let keep: Vec<&str> = rows
        .iter()
        .copied()
        .filter(|r| {
            r.starts_with('{')
                && serde_json::from_str::<Value>(r)
                    .is_ok_and(|v| v["done"].as_f64().unwrap_or(0.0) > cutoff)
        })
        .collect();
    if keep.len() < rows.len() {
        let tmp = format!("{PACE}.tmp");
        if fs::write(
            &tmp,
            keep.iter().map(|r| format!("{r}\n")).collect::<String>(),
        )
        .is_ok()
        {
            let _ = fs::rename(&tmp, PACE);
        }
    }
}

pub fn run(chunks: &Path, out: &Path) -> Result<(), String> {
    let keep_audio: usize = std::env::var("OZEN_KEEP_AUDIO")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let separator = Arc::new(OnceLock::new());
    let bg = Arc::clone(&separator);
    // A first run's download takes minutes, and the live transcript must not wait for it.
    std::thread::spawn(move || match Separator::load() {
        Ok(s) => {
            let _ = bg.set(s);
        }
        Err(e) => println!("voice separation unavailable: {e}"), // offline on first run: transcribe without it
    });
    let mut t = Transcriber {
        whisper: Whisper::load()?,
        ecapa: Ecapa::load()?,
        separator,
        speakers: vec![],
        ignored: vec![],
        same: DEFAULT_SAME,
        registry_mtime: SystemTime::UNIX_EPOCH,
        last_pull: None,
        unknown: 0,
        run: now() as u64,
        last_lang: HashMap::new(),
        last_text: HashMap::new(),
        active: vec![],
        covered: HashMap::from([("call", 0.0), ("local", 0.0)]),
        floors: HashMap::new(),
    };
    t.load_registry();
    trim_pace();
    println!("transcribing {} -> {}", chunks.display(), out.display());
    let mut disk_full = false; // logged once per full-disk spell, not on every retry
    loop {
        for f in pending(chunks) {
            let stem = f
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let Some((ms, tag)) = stem.split_once('-') else {
                continue;
            };
            let t_chunk = ms.parse::<f64>().unwrap_or(0.0) / 1000.0;
            // Mic echo check needs the call/local audio for the same time window first.
            let age = fs::metadata(&f)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|m| m.elapsed().ok());
            if tag == "mic"
                && t.covered.values().copied().fold(f64::INFINITY, f64::min) < t_chunk + 14.0
                && age.is_some_and(|a| a < Duration::from_secs(40))
            {
                continue;
            }
            let began = now();
            let (mut sec, mut wrote, mut error) = (0.0, 0, None);
            match t.chunk(&f, ms, tag, chunks, out) {
                Ok((s, w)) => (sec, wrote) = (s, w),
                Err(e) if e.raw_os_error() == Some(28) => {
                    // Disk full: keep the audio and try again once there's room, rather than drop it.
                    if !disk_full {
                        eprintln!(
                            "disk full, keeping {} and the rest for later: {e}",
                            f.display()
                        );
                    }
                    disk_full = true;
                    std::thread::sleep(Duration::from_secs(10));
                    break;
                }
                Err(e) => {
                    // one bad chunk must not kill the live transcript
                    eprintln!(
                        "skip {}: {e}",
                        f.file_name().unwrap_or_default().to_string_lossy()
                    );
                    error = Some(e.to_string());
                }
            }
            disk_full = false;
            let mut row = json!({"ms": ms.parse::<i64>().unwrap_or(0), "tag": tag, "sec": round(sec, 2),
                                 "done": round(now(), 2), "took": round(now() - began, 2), "lines": wrote});
            if let Some(e) = error {
                row["error"] = json!(e);
            }
            // stats only: never stop transcribing over them
            let _ = OpenOptions::new()
                .create(true)
                .append(true)
                .open(PACE)
                .and_then(|mut p| writeln!(p, "{row}"));
            if keep_audio > 0 && f.exists() {
                // mic/call: recent audio for comparing models (ozen compare). local: computer audio only, kept apart
                // so a missed echo can be replayed with the reference the transcriber had.
                let keep = if tag == "local" {
                    Path::new(RECENT).join("local")
                } else {
                    PathBuf::from(RECENT)
                };
                let kept = fs::create_dir_all(&keep)
                    .and_then(|_| fs::rename(&f, keep.join(f.file_name().unwrap_or_default())));
                match kept {
                    Ok(()) => {
                        let mut old = wavs(&keep);
                        old.sort();
                        old.iter()
                            .take(old.len().saturating_sub(keep_audio))
                            .for_each(|o| {
                                let _ = fs::remove_file(o);
                            });
                    }
                    // e.g. disk full: the copy is optional, the transcriber must keep going
                    Err(e) => eprintln!("not keeping {} in recent/: {e}", f.display()),
                }
            }
            let _ = fs::remove_file(&f);
            if wavs(chunks).iter().any(|g| start_ms(g) > start_ms(&f)) {
                break; // newer audio arrived: transcribe it before going further back
            }
        }
        // Keep echo windows as far back as the oldest chunk still waiting, so backlog mic chunks keep theirs.
        let oldest = wavs(chunks)
            .iter()
            .map(|g| start_ms(g) as f64 / 1000.0)
            .fold(now(), f64::min);
        t.active.retain(|x| x.1 > oldest - 120.0);
        t.load_registry();
        std::thread::sleep(Duration::from_secs(1));
    }
}
