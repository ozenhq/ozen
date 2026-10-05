//! Summaries arriving in parts (protocol.rs). A summary is answered only once all its parts are in,
//! so parts wait here, bounded so a buggy or hostile peer can't exhaust this Mac's memory (OFE-65): at
//! most `SUMMARIES` summaries, `PARTS` parts and `ENTRIES` entries held in all. A real summary needs one
//! entry per record in the buckets that differ, so the caps sit far above any vault (a heavy user has
//! about a million records); over them the summary is dropped, and the other Mac's next exchange retries.
use std::collections::BTreeMap;

/// One summary line: (kind, key, v, hash).
pub type Entry = (String, String, u64, String);

/// Unfinished summaries kept; a summary whose last part never came (a lagging Mac) is forgotten.
const SUMMARIES: usize = 8;
pub const PARTS: usize = 4096;
pub const ENTRIES: usize = 2_000_000;

#[derive(Default)]
pub struct Partial {
    /// id -> (parts, part -> entries)
    by_id: BTreeMap<u64, (u32, BTreeMap<u32, Vec<Entry>>)>,
    parts: usize,
    entries: usize,
}

impl Partial {
    /// Removes summary `id`, returning its entries.
    fn take(&mut self, id: u64) -> Vec<Entry> {
        let (_, got) = self.by_id.remove(&id).unwrap_or_default();
        self.parts -= got.len();
        let all: Vec<Entry> = got.into_values().flatten().collect();
        self.entries -= all.len();
        all
    }

    /// Takes part `part` (of `parts`) of summary `id`: all its entries once the last part is in, None
    /// while parts are missing, Err (and the summary dropped) for a part over the bounds.
    pub fn add(
        &mut self,
        id: u64,
        part: u32,
        parts: u32,
        s: Vec<Entry>,
    ) -> Result<Option<Vec<Entry>>, String> {
        if part >= parts || parts as usize > PARTS {
            return Err(format!("summary part {part} of {parts}"));
        }
        if self.by_id.len() >= SUMMARIES && !self.by_id.contains_key(&id) {
            let oldest = *self.by_id.keys().next().expect("full"); // ids are send times
            self.take(oldest);
        }
        let n = s.len();
        let p = self.by_id.entry(id).or_insert((parts, BTreeMap::new()));
        if p.0 != parts {
            self.take(id);
            return Err(format!(
                "summary parts said {parts}, earlier ones another count"
            ));
        }
        let replaced = p.1.insert(part, s).map(|old| old.len()); // a repeated part replaces itself
        let complete = p.1.len() as u32 == p.0;
        match replaced {
            Some(old) => self.entries = self.entries - old + n,
            None => {
                self.parts += 1;
                self.entries += n;
            }
        }
        if self.parts > PARTS || self.entries > ENTRIES {
            self.take(id);
            return Err(format!(
                "summaries over {PARTS} parts or {ENTRIES} entries held"
            ));
        }
        Ok(complete.then(|| self.take(id)))
    }
}

#[cfg(test)]
#[path = "summaries_tests.rs"]
mod tests;
