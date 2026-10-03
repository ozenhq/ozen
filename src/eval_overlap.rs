//! `ozen eval-overlap [ami] [he] [call] [--n 20]`: score people-talking-at-once separation (src/overlap.rs) on real
//! speech. Windows where two people talk at once, and solo windows, are transcribed by the live transcriber's Whisper
//! with utterances kept whole vs separated by `overlap::voices`.
//!
//! - ami:  real meetings, one far-field room mic, real overlaps (AMI, meetings EN2002a-c)
//! - he:   real Hebrew speakers (FLEURS he_il), pairs of different people mixed 1s apart
//! - call: real English speakers (LibriSpeech), pairs mixed 1s apart, through Opus 24kbps like a video call
//!
//! Each set downloads once to eval/data/ (250-450MB). Prints word recall and extra words vs the reference text per
//! set: separation should raise recall on overlaps and leave solos alone. Windows are picked by a fixed seed and
//! Whisper's text is cached per clip (eval/data/asr-cache-rs.jsonl), so a rerun prints the same table and, after
//! tuning src/overlap.rs, only transcribes the pieces that changed.
use crate::ecapa::Ecapa;
use crate::overlap::{self, SR};
use crate::separate::Separator;
use crate::text;
use crate::whisper::Whisper;
use parquet::file::reader::{FileReader, SerializedFileReader};
use parquet::record::Field;
use regex::Regex;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Stdio;

const DATA: &str = "eval/data";
const CACHE: &str = "eval/data/asr-cache-rs.jsonl"; // not the Python eval's: a different Whisper wrote that one
const SETS: [(&str, &str, &str); 3] = [
    (
        "ami",
        "https://huggingface.co/datasets/edinburghcstr/ami/resolve/main/sdm/test-00000-of-00004.parquet",
        "en",
    ),
    (
        "he",
        "https://huggingface.co/api/datasets/google/fleurs/parquet/he_il/test/0.parquet",
        "he",
    ),
    (
        "call",
        "https://huggingface.co/api/datasets/openslr/librispeech_asr/parquet/all/test.clean/0.parquet",
        "en",
    ),
];

/// One dataset row: its audio file's bytes (WAV or FLAC), reference text, and AMI's timing and speaker.
#[derive(Clone, Default)]
struct Row {
    audio: Vec<u8>,
    text: String,
    meeting: String,
    speaker: String,
    begin: f64,
    end: f64,
}

fn rows(name: &str, url: &str) -> Result<Vec<Row>, String> {
    let f = Path::new(DATA).join(format!("{name}.parquet"));
    if !f.exists() {
        fs::create_dir_all(DATA).map_err(|e| e.to_string())?;
        println!("downloading {name} set to {}", f.display());
        let part = f.with_extension("part");
        let ok = crate::cmd("curl")
            .args(["-fsSL", "-o"])
            .arg(&part)
            .arg(url)
            .status();
        if !ok.is_ok_and(|s| s.success()) {
            return Err(format!("download {url} failed"));
        }
        fs::rename(&part, &f).map_err(|e| e.to_string())?;
    }
    let reader = SerializedFileReader::new(File::open(&f).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let mut out = vec![];
    for row in reader.get_row_iter(None).map_err(|e| e.to_string())? {
        let mut r = Row::default();
        for (col, v) in row.map_err(|e| e.to_string())?.get_column_iter() {
            match (col.as_str(), v) {
                ("audio", Field::Group(g)) => {
                    for (k, b) in g.get_column_iter() {
                        if let ("bytes", Field::Bytes(b)) = (k.as_str(), b) {
                            r.audio = b.data().to_vec();
                        }
                    }
                }
                // FLEURS names it transcription; the others text
                ("text" | "transcription", Field::Str(s)) => r.text = s.clone(),
                ("meeting_id", Field::Str(s)) => r.meeting = s.clone(),
                ("speaker_id", Field::Str(s)) => r.speaker = s.clone(),
                ("speaker_id", Field::Long(n)) => r.speaker = n.to_string(),
                ("begin_time", Field::Float(t)) => r.begin = *t as f64,
                ("end_time", Field::Float(t)) => r.end = *t as f64,
                _ => {}
            }
        }
        out.push(r);
    }
    Ok(out)
}

/// 16 kHz mono float samples of an audio file's bytes (the sets are 16 kHz already), through ffmpeg.
fn decode(bytes: &[u8], args: &[&str]) -> Vec<f32> {
    let mut child = crate::cmd("ffmpeg")
        .args(["-loglevel", "error", "-i", "pipe:0"])
        .args(args)
        .args(["-ar", "16000", "-ac", "1", "-f", "f32le", "pipe:1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let data = bytes.to_vec();
    let feed = std::thread::spawn(move || stdin.write_all(&data)); // ffmpeg writes while it reads
    let out = child.wait_with_output().expect("ffmpeg");
    let _ = feed.join();
    out.stdout
        .as_chunks::<4>()
        .0
        .iter()
        .map(|b| f32::from_le_bytes(*b))
        .collect()
}

fn audio(r: &Row) -> Vec<f32> {
    decode(&r.audio, &[])
}

/// What a call's codec does to the audio: through Opus at 24 kbps and back.
fn opus(x: &[f32]) -> Vec<f32> {
    let pcm: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
    let mut child = crate::cmd("ffmpeg")
        .args([
            "-loglevel",
            "error",
            "-f",
            "f32le",
            "-ar",
            "16000",
            "-ac",
            "1",
            "-i",
            "pipe:0",
        ])
        .args([
            "-c:a",
            "libopus",
            "-b:a",
            "24k",
            "-application",
            "voip",
            "-f",
            "ogg",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("ffmpeg");
    let mut stdin = child.stdin.take().unwrap();
    let feed = std::thread::spawn(move || stdin.write_all(&pcm));
    let ogg = child.wait_with_output().expect("ffmpeg").stdout;
    let _ = feed.join();
    decode(&ogg, &[])
}

/// b starts 1s into a, at a's level.
fn mix(a: &[f32], b: &[f32]) -> Vec<f32> {
    let peak = |x: &[f32]| x.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-9);
    let k = peak(a) / peak(b);
    let n = a.len().max(b.len() + SR);
    let mut out = vec![0f32; n];
    for (i, v) in a.iter().enumerate() {
        out[i] += v;
    }
    for (i, v) in b.iter().enumerate() {
        out[SR + i] += v * k;
    }
    out
}

/// AMI segment a, then b's audio past a's end (same mic), and both texts.
fn joined(a: &Row, b: &Row) -> (Vec<f32>, String) {
    let mut x = audio(a);
    if b.end > a.end {
        let skip = ((a.end - b.begin) * SR as f64) as usize;
        x.extend(audio(b).into_iter().skip(skip));
    }
    (x, format!("{} {}", a.text, b.text))
}

/// splitmix64: a fixed seed picks the same windows every run.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_add(0x9e3779b97f4a7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
        ((z ^ (z >> 31)) % n as u64) as usize
    }

    /// k of `v` without repeats, in picked order (like Python's random.sample).
    fn sample<T: Clone>(&mut self, v: &[T], k: usize) -> Vec<T> {
        let mut idx: Vec<usize> = (0..v.len()).collect();
        (0..k.min(v.len()))
            .map(|i| {
                let j = i + self.below(idx.len() - i);
                idx.swap(i, j);
                v[idx[i]].clone()
            })
            .collect()
    }
}

type Case = (Vec<f32>, String);

/// (overlap windows, solo windows) as (audio, reference text).
fn windows(
    name: &str,
    url: &str,
    n: usize,
    embed: &mut dyn FnMut(&[f32]) -> Vec<f32>,
) -> Result<(Vec<Case>, Vec<Case>), String> {
    let mut rs = rows(name, url)?;
    let mut rng = Rng(0);
    if name == "ami" {
        // real overlaps: two speakers' segments overlapping >= 1s, nobody else in the window
        rs.sort_by(|a, b| {
            (&a.meeting, a.begin)
                .partial_cmp(&(&b.meeting, b.begin))
                .unwrap()
        });
        let (mut pairs, mut solos) = (vec![], vec![]);
        for i in 0..rs.len() {
            let a = &rs[i];
            let near: Vec<usize> = (i.saturating_sub(30)..(i + 30).min(rs.len()))
                .filter(|&j| {
                    j != i
                        && rs[j].meeting == a.meeting
                        && rs[j].begin < a.end
                        && rs[j].end > a.begin
                })
                .collect();
            if near.is_empty() && (3.0..=10.0).contains(&(a.end - a.begin)) {
                solos.push(i);
            }
            for &j in &near {
                let b = &rs[j];
                let (lo, hi) = (a.begin, a.end.max(b.end));
                let others = near
                    .iter()
                    .any(|&c| c != j && rs[c].begin < hi && rs[c].end > lo);
                if b.begin >= lo
                    && b.speaker != a.speaker
                    && !others
                    && a.end.min(b.end) - b.begin >= 1.0
                    && (3.0..=12.0).contains(&(hi - lo))
                {
                    pairs.push((i, j));
                }
            }
        }
        let pairs = rng
            .sample(&pairs, n)
            .into_iter()
            .map(|(i, j)| joined(&rs[i], &rs[j]))
            .collect();
        let solos = rng
            .sample(&solos, n)
            .into_iter()
            .map(|i| (audio(&rs[i]), rs[i].text.clone()))
            .collect();
        return Ok((pairs, solos)); // decode only the sampled ones
    }
    // read speech: pair utterances by different people (voiceprints far apart; FLEURS has no speaker ids)
    let every5: Vec<Row> = rs.into_iter().step_by(5).collect();
    let pool: Vec<Case> = rng
        .sample(&every5, 4 * n)
        .iter()
        .map(|r| (audio(r), r.text.clone()))
        .filter(|(x, _)| x.len() > 3 * SR && x.len() < 12 * SR)
        .collect();
    let prints: Vec<Vec<f32>> = pool.iter().map(|(x, _)| embed(x)).collect();
    let (mut used, mut pairs) = (vec![false; pool.len()], vec![]);
    for i in 0..pool.len() {
        let j =
            (i + 1..pool.len()).find(|&j| !used[j] && overlap::dot(&prints[i], &prints[j]) < 0.2);
        if let (false, Some(j), true) = (used[i], j, pairs.len() < n) {
            pairs.push((
                mix(&pool[i].0, &pool[j].0),
                format!("{} {}", pool[i].1, pool[j].1),
            ));
            used[i] = true;
            used[j] = true;
        }
    }
    let mut solos: Vec<Case> = pool
        .into_iter()
        .zip(used)
        .filter(|(_, u)| !u)
        .map(|(c, _)| c)
        .take(n)
        .collect();
    if name == "call" {
        pairs = pairs.into_iter().map(|(x, t)| (opus(&x), t)).collect();
        solos = solos.into_iter().map(|(x, t)| (opus(&x), t)).collect();
    }
    Ok((pairs, solos))
}

fn words(s: &str) -> HashMap<String, usize> {
    let re = Regex::new(r"[^\w' ]").unwrap();
    let mut out = HashMap::new();
    for w in re.replace_all(&s.to_lowercase(), " ").split_whitespace() {
        *out.entry(w.to_string()).or_default() += 1;
    }
    out
}

/// Whisper's text for a clip, cached by the clip's samples, language and hint words.
struct Asr {
    whisper: Whisper,
    vocab: Vec<String>,
    cache: HashMap<String, String>,
}

impl Asr {
    fn recognize(&mut self, clip: &[f32], lang: &str) -> Result<String, String> {
        if clip.len() < SR * 3 / 10 {
            return Ok(String::new());
        }
        let bytes: Vec<u8> = clip.iter().flat_map(|v| v.to_le_bytes()).collect();
        let key = format!(
            "{:016x}",
            crate::eval::fnv(
                format!("{lang}|{:?}", self.vocab).as_bytes(),
                crate::eval::fnv(&bytes, crate::eval::FNV0)
            )
        );
        if let Some(t) = self.cache.get(&key) {
            return Ok(t.clone());
        }
        let prompt = text::prompt(&self.vocab, "");
        let (heard, _, _) = self
            .whisper
            .decode(clip, &prompt, lang, None)
            .map_err(|e| e.to_string())?;
        let t = text::kept(&heard, &self.vocab);
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(CACHE)
            .map_err(|e| e.to_string())?;
        writeln!(f, "{}", serde_json::json!([key, t])).map_err(|e| e.to_string())?;
        self.cache.insert(key, t.clone());
        Ok(t)
    }
}

/// (recalled, reference, extra) words for clips kept whole and for separated pieces, and how many windows split.
type Tally = ([usize; 3], [usize; 3], usize);

fn score(
    cases: &[Case],
    lang: &str,
    asr: &mut Asr,
    sep: &Separator,
    embed: &mut dyn FnMut(&[f32]) -> Vec<f32>,
    same: f32,
) -> Result<Tally, String> {
    let (mut whole, mut separated, mut split) = ([0; 3], [0; 3], 0);
    for (x, reference) in cases {
        let peak = x.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-9);
        let x: Vec<f32> = x.iter().map(|v| v / peak * 0.5).collect(); // a close mic's level; the sets' gains are arbitrary
        let clips: Vec<&[f32]> = overlap::utterances(&x, 0.0, 0.3)
            .into_iter()
            .map(|(_, c)| c)
            .collect();
        let pieces: Vec<Vec<f32>> = clips
            .iter()
            .flat_map(|c| overlap::voices(c, Some(sep), &mut *embed, same))
            .map(|(_, p)| p)
            .collect();
        split += (pieces.len() > clips.len()) as usize;
        let want = words(reference);
        for (t, parts) in [
            (
                &mut whole,
                clips.iter().map(|c| c.to_vec()).collect::<Vec<_>>(),
            ),
            (&mut separated, pieces),
        ] {
            let mut heard = vec![];
            for p in &parts {
                heard.push(asr.recognize(p, lang)?);
            }
            let got = words(&heard.join(" "));
            t[0] += want
                .iter()
                .map(|(w, k)| got.get(w).copied().unwrap_or(0).min(*k))
                .sum::<usize>();
            t[1] += want.values().sum::<usize>();
            t[2] += got
                .iter()
                .map(|(w, k)| k.saturating_sub(want.get(w).copied().unwrap_or(0)))
                .sum::<usize>();
        }
    }
    Ok((whole, separated, split))
}

pub fn run(args: &[String]) -> Result<(), String> {
    let n = args
        .iter()
        .position(|a| a == "--n")
        .and_then(|i| args.get(i + 1)?.parse().ok())
        .unwrap_or(20);
    let names: Vec<&str> = SETS
        .iter()
        .map(|s| s.0)
        .filter(|s| args.iter().any(|a| a == s))
        .collect();
    let sets: Vec<_> = SETS
        .iter()
        .filter(|s| names.is_empty() || names.contains(&s.0))
        .collect();
    let same = fs::read_to_string("voices/config.json")
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()?["same_speaker"].as_f64())
        .unwrap_or(0.26) as f32; // the calibrated cutoff when the eval was written
    let vocab: Vec<String> = fs::read_to_string("vocab.txt")
        .unwrap_or_default()
        .split([',', '\n'])
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(String::from)
        .collect();
    let cache = fs::read_to_string(CACHE)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<(String, String)>(l).ok())
        .collect();
    let mut asr = Asr {
        whisper: Whisper::load()?,
        vocab,
        cache,
    };
    let ecapa = Ecapa::load()?;
    let mut embed = |x: &[f32]| {
        let e = ecapa.embed(x).expect("ecapa");
        let norm = e.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-9);
        e.iter().map(|v| v / norm).collect::<Vec<f32>>()
    };
    let sep = Separator::load()?;
    println!(
        "{:6} {:9} {:>3} {:>5}  recall whole -> separated   extra words whole -> separated",
        "set", "windows", "n", "split"
    );
    for (name, url, lang) in sets {
        let (pairs, solos) = windows(name, url, n, &mut embed)?;
        for (kind, cases) in [("overlap", &pairs), ("solo", &solos)] {
            let ((hw, nw, ew), (hs, ns, es), split) = {
                let (w, s, split) = score(cases, lang, &mut asr, &sep, &mut embed, same)?;
                ((w[0], w[1], w[2]), (s[0], s[1], s[2]), split)
            };
            let r = |a: usize, b: usize| a as f64 / b.max(1) as f64;
            println!(
                "{name:6} {kind:9} {:3} {split:5}  {:.2} -> {:.2}{:15}{ew} -> {es}",
                cases.len(),
                r(hw, nw),
                r(hs, ns),
                ""
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "eval_overlap_tests.rs"]
mod tests;
