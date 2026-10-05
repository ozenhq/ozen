//! Which protocol version this Mac reads and writes (OFE-82). A user's Macs update ozen days apart, and
//! with a relay that keeps nothing, two Macs that can't read each other's frames simply stop syncing
//! until both update. So each hello names the sender's highest version, and once a Mac has heard the
//! others it writes the lowest version any of them speaks, as long as it still can. A build reads and
//! writes its own version and the one before; frames come in the version of the Mac that wrote them.
//!
//! The relay sends each frame to every Mac of the vault, so there is one version for all of them, not
//! one per pair. Hellos go out in the oldest version this Mac speaks: whoever can talk to it reads them.

/// The newest protocol version, the first byte of every frame's plaintext.
/// - 1: the first (OFE-10).
/// - 2: lines go without the fields only their Mac means, `crdt::LOCAL_LINE_FIELDS` (OFE-56); the
///   messages are unchanged.
pub const VERSION: u8 = 2;

/// The versions one Mac speaks, and what the other Macs of the vault have said they speak.
#[derive(Clone, Copy, Debug)]
pub struct Versions {
    own: u8,
    /// The lowest highest-version any other Mac has announced in a hello.
    lowest_peer: Option<u8>,
}

/// Whether a frame of some version can be read here.
#[derive(Debug, PartialEq)]
pub enum Read {
    Yes,
    /// Newer than this Mac speaks: the user is told to update ozen.
    Newer,
    /// Older than this Mac still reads (or 0, no frame of ours).
    Older,
}

impl Default for Versions {
    fn default() -> Self {
        Versions::new(VERSION)
    }
}

impl Versions {
    /// A Mac whose newest version is `own` (tests simulate other builds with it).
    pub fn new(own: u8) -> Self {
        Versions {
            own,
            lowest_peer: None,
        }
    }

    /// The newest version this Mac speaks, announced in its hellos.
    pub fn own(&self) -> u8 {
        self.own
    }

    /// The oldest version this Mac still reads and writes: its own and the one before.
    pub fn oldest(&self) -> u8 {
        self.own.saturating_sub(1).max(1)
    }

    /// Records that another Mac speaks versions up to `max`.
    pub fn heard(&mut self, max: u8) {
        self.lowest_peer = Some(self.lowest_peer.map_or(max, |v| v.min(max)));
    }

    /// The version to write frames in: the lowest any other Mac speaks, if this Mac still can; else its
    /// own (a Mac too old to read it is told to update).
    pub fn speak(&self) -> u8 {
        match self.lowest_peer {
            Some(v) if v >= self.oldest() => v.min(self.own),
            _ => self.own,
        }
    }

    pub fn read(&self, v: u8) -> Read {
        if v > self.own {
            Read::Newer
        } else if v < self.oldest() {
            Read::Older
        } else {
            Read::Yes
        }
    }
}

#[cfg(test)]
#[path = "version_tests.rs"]
mod tests;
