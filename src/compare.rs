//! `ozen compare [N]`: transcribe the last N chunks with speech kept in recent/ (OZEN_KEEP_AUDIO) with stock
//! Whisper, the Hebrew model, and the Hebrew model with the vocabulary hint, so a model or prompt change is
//! judged on your own speech, not on synthetic audio. Whisper runs with the live transcriber's
//! filters, so each setup shows what would land in the transcript, or "(dropped: …)" with Whisper's raw text
//! when the filters threw all of it away (usually a hallucination).
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const SPEECH_RMS: f64 = 0.006; // quieter chunks are silence (the transcriber's SILENCE_RMS)
// (label, Whisper model, with the vocabulary hint)
const SETUPS: [(&str, &str, bool); 3] = [
    ("stock", "stock", false),
    ("hebrew", "hebrew", false),
    ("hebrew+vocab", "hebrew", true),
];

pub fn run(args: &[String]) -> Result<(), String> {
    let n: usize = match args.first() {
        Some(a) => a.parse().map_err(|_| format!("not a count: {a}"))?,
        None => 6,
    };
    let mut chunks: Vec<PathBuf> = fs::read_dir("recent")
        .map_err(|e| format!("recent/: {e}"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    chunks.sort(); // <epoch_ms>-<tag>.wav: oldest first
    chunks.retain(|p| rms(p).is_some_and(|r| r > SPEECH_RMS));
    let speech = &chunks[chunks.len().saturating_sub(n)..];
    if speech.is_empty() {
        return Err("no speech in recent/ yet: talk for a bit, then rerun".into());
    }
    let vocab = &crate::mcp::vocab();
    let rows: Vec<Value> = speech
        .iter()
        .flat_map(|p| {
            SETUPS.iter().map(move |(_, model, hint)| {
                json!({"audio": p, "lang": "he", "model": model,
                       "words": if *hint { vocab.clone() } else { vec![] }})
            })
        })
        .collect();
    let replies = crate::eval::worker(&rows)?;
    for (p, r) in speech.iter().zip(replies.chunks(SETUPS.len())) {
        println!("== {}", p.file_name().unwrap_or_default().to_string_lossy());
        for ((label, ..), reply) in SETUPS.iter().zip(r) {
            println!("  {label:13}{}", line(reply));
        }
    }
    Ok(())
}

/// What the transcript would get, or what the filters dropped when that's nothing.
fn line(reply: &Value) -> String {
    let (heard, raw) = (
        reply["heard"].as_str().unwrap_or(""),
        reply["raw"].as_str().unwrap_or(""),
    );
    if heard.is_empty() && !raw.is_empty() {
        format!("(dropped: {raw})")
    } else {
        heard.to_string()
    }
}

/// Root mean square of a chunk's samples, float or 16-bit, as the recorder writes them.
fn rms(path: &Path) -> Option<f64> {
    let mut wav = hound::WavReader::open(path).ok()?;
    let samples: Vec<f64> = match wav.spec().sample_format {
        hound::SampleFormat::Float => wav
            .samples::<f32>()
            .map(|s| s.map(f64::from))
            .collect::<Result<_, _>>(),
        hound::SampleFormat::Int => wav
            .samples::<i16>()
            .map(|s| s.map(|s| f64::from(s) / 32768.0))
            .collect(),
    }
    .ok()?;
    (!samples.is_empty())
        .then(|| (samples.iter().map(|s| s * s).sum::<f64>() / samples.len() as f64).sqrt())
}

#[cfg(test)]
#[path = "compare_tests.rs"]
mod tests;
