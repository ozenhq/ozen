//! Every transcript line's voiceprint, as retraining reads it (src/train.rs, src/ignore.rs). With sync, a
//! retrain runs after every received tag, and lines.jsonl is MBs of mostly 192-float prints (100 MB at
//! 50k lines), so parsing it each time is most of a retrain. A binary cache beside it (`CACHE`, local, never
//! synced) holds the prints of the file's first `offset` bytes: lines.jsonl only grows in place (the
//! transcriber and notes append; every rewrite swaps in a new file, `crdt::write_atomic`, mcp `rewrite`), so
//! only what was appended since is parsed and added. Another file (new inode), or a cache that doesn't read
//! back whole, is rebuilt from the full file. The cache is derived data: losing it costs one full parse.
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::sync::{Arc, Mutex, PoisonError};

const LINES: &str = "lines.jsonl";
pub(crate) const CACHE: &str = ".prints.cache";
/// Format tag; a cache with another one is rebuilt.
const MAGIC: &[u8; 8] = b"ozprint1";
/// MAGIC, inode, offset covered, record count.
const HEADER: usize = 8 + 8 + 8 + 8;

/// A transcript line's voiceprint.
#[derive(serde::Deserialize, Debug, PartialEq)]
pub(crate) struct Print {
    pub id: String,
    /// Seconds of speech; None when missing or not a number.
    #[serde(default, deserialize_with = "number_or_none")]
    pub d: Option<f64>,
    pub e: Vec<f64>,
}

fn number_or_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    Ok(<Value as serde::Deserialize>::deserialize(d)?.as_f64())
}

/// The prints in JSONL `text`, in order. A line without an id or print (or not a number list) is skipped.
fn parse(text: &[u8]) -> Vec<Print> {
    text.split(|b| *b == b'\n')
        .filter_map(|l| serde_json::from_slice::<Print>(l).ok())
        .collect()
}

fn encode(p: &Print, out: &mut Vec<u8>) {
    out.extend((p.id.len() as u32).to_le_bytes());
    out.extend(p.id.as_bytes());
    out.extend(p.d.unwrap_or(f64::NAN).to_le_bytes());
    out.extend((p.e.len() as u32).to_le_bytes());
    for x in &p.e {
        out.extend(x.to_le_bytes());
    }
}

/// Reads `n` records from `b`; None if they don't all fit or don't decode.
fn decode(mut b: &[u8], n: u64) -> Option<(Vec<Print>, usize)> {
    let start = b.len();
    let mut take = |k: usize| -> Option<&[u8]> {
        let (h, t) = b.split_at_checked(k)?;
        b = t;
        Some(h)
    };
    let mut out = Vec::with_capacity(n.min(1 << 20) as usize);
    for _ in 0..n {
        let len = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        let id = String::from_utf8(take(len)?.to_vec()).ok()?;
        let d = f64::from_le_bytes(take(8)?.try_into().ok()?);
        let k = u32::from_le_bytes(take(4)?.try_into().ok()?) as usize;
        let e = take(k.checked_mul(8)?)?
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| f64::from_le_bytes(*c))
            .collect();
        out.push(Print {
            id,
            d: (!d.is_nan()).then_some(d),
            e,
        });
    }
    Some((out, start - b.len()))
}

/// (inode, offset, count, records) from the cache, if it reads back whole.
fn load() -> Option<(u64, u64, Vec<Print>, u64)> {
    let mut b = vec![];
    File::open(CACHE).ok()?.read_to_end(&mut b).ok()?;
    let h = b.get(..HEADER)?;
    if &h[..8] != MAGIC {
        return None;
    }
    let num = |i: usize| u64::from_le_bytes(h[i..i + 8].try_into().unwrap());
    let (ino, offset, n) = (num(8), num(16), num(24));
    let (records, used) = decode(&b[HEADER..], n)?;
    Some((ino, offset, records, (HEADER + used) as u64))
}

/// Writes a whole new cache (temp file, then rename: a reader sees the old cache or the new one).
fn rebuild(ino: u64, offset: u64, prints: &[Print]) -> std::io::Result<()> {
    let mut b = Vec::with_capacity(HEADER + prints.len() * (40 + 192 * 8));
    b.extend(MAGIC);
    for x in [ino, offset, prints.len() as u64] {
        b.extend(x.to_le_bytes());
    }
    for p in prints {
        encode(p, &mut b);
    }
    let tmp = format!("{CACHE}.{}.tmp", std::process::id());
    fs::write(&tmp, b)?;
    fs::rename(tmp, CACHE)
}

/// Appends `new` after the cache's `end` byte, then updates its header. A crash in between leaves a header
/// that still describes the old records exactly: the next read parses those appends again.
fn extend(end: u64, offset: u64, count: u64, new: &[Print]) -> std::io::Result<()> {
    let mut f = OpenOptions::new().write(true).open(CACHE)?;
    f.set_len(end)?; // drops records a crashed append left past the header's count
    f.seek(SeekFrom::Start(end))?;
    let mut b = vec![];
    for p in new {
        encode(p, &mut b);
    }
    f.write_all(&b)?;
    f.seek(SeekFrom::Start(16))?;
    f.write_all(&offset.to_le_bytes())?;
    f.write_all(&(count + new.len() as u64).to_le_bytes())
}

/// The prints from the cache plus whatever was appended to lines.jsonl since, keeping the cache up to
/// date; a full parse (and a fresh cache) when the cache is missing, stale or broken.
fn read() -> Vec<Print> {
    let Ok(mut f) = File::open(LINES) else {
        return vec![];
    };
    let Ok(m) = f.metadata() else {
        return vec![];
    };
    if let Some((ino, offset, mut prints, end)) = load()
        && ino == m.ino()
        && offset <= m.len()
    {
        let mut tail = vec![];
        if f.seek(SeekFrom::Start(offset)).is_ok() && f.read_to_end(&mut tail).is_ok() {
            // a last line without its newline counts now, but is cached only once it's whole
            let whole = tail.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
            let new = parse(&tail[..whole]);
            if whole > 0 {
                let count = prints.len() as u64;
                let _ = extend(end, offset + whole as u64, count, &new);
            }
            prints.extend(new);
            prints.extend(parse(&tail[whole..]));
            return prints;
        }
    }
    let mut text = vec![];
    if f.read_to_end(&mut text).is_err() {
        return vec![];
    }
    let whole = text.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let mut prints = parse(&text[..whole]);
    let _ = rebuild(m.ino(), whole as u64, &prints);
    prints.extend(parse(&text[whole..])); // a last line without its newline still counts now
    prints
}

/// Every line with a voiceprint, in file order: from the cache (`read`), and once per process while the
/// file is unchanged, since one `ozen retrain` trains and then matches ignored voices on the same lines.
pub(crate) fn prints() -> Arc<Vec<Print>> {
    type Stamp = Option<(u64, i64, i64, u64)>;
    static LAST: Mutex<Option<(Stamp, Arc<Vec<Print>>)>> = Mutex::new(None);
    let stamp: Stamp = fs::metadata(LINES)
        .ok()
        .map(|m| (m.len(), m.mtime(), m.mtime_nsec(), m.ino()));
    let mut last = LAST.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((s, p)) = last.as_ref()
        && *s == stamp
    {
        return p.clone();
    }
    let p = Arc::new(read());
    *last = Some((stamp, p.clone()));
    p
}

#[cfg(test)]
#[path = "prints_tests.rs"]
mod tests;
