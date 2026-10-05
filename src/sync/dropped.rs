//! Counts of what another Mac sent that this one couldn't use (protocol.rs), for `ozen sync status`.
#![allow(dead_code)] // ponytail: shown once the connection runs (OFE-7, OFE-41)
/// What happened to frames this Mac couldn't use, for `ozen sync status`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Dropped {
    /// Frames that didn't open (another key, altered) or didn't parse: dropped, not merged.
    pub bad: u64,
    /// Frames from a newer protocol version: not merged until this Mac updates ozen.
    pub newer: u64,
    /// Records in frames that opened fine but failed validation (valid.rs): dropped, not merged.
    pub records: u64,
    pub last_error: Option<String>,
}

impl Dropped {
    /// The line to show the user, if any.
    pub fn advice(&self) -> Option<&'static str> {
        (self.newer > 0).then_some("another Mac runs a newer ozen: update ozen to sync with it")
    }
}
