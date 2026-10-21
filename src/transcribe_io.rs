//! File and disk helpers the live transcriber uses: decode a chunk to 16 kHz f32, open lines.jsonl under its lock,
//! and read free disk space before a write.
use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

const LINES: &str = "lines.jsonl"; // every transcript line with its voiceprint; the panel tags these

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
pub(crate) fn open_lines() -> std::io::Result<File> {
    loop {
        let f = OpenOptions::new().create(true).append(true).open(LINES)?;
        f.lock()?;
        if fs::metadata(LINES).is_ok_and(|m| m.ino() == f.metadata().map_or(0, |m| m.ino())) {
            return Ok(f);
        }
    }
}

/// Free bytes on the disk holding `path`, from `df -k`.
pub(crate) fn free_bytes(path: &Path) -> Option<u64> {
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
