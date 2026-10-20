//! Whether this Mac slept or moved to another network since the last look, so the relay link (link.rs)
//! reconnects at once instead of noticing its dead connection on a failed send or its backoff timer,
//! minutes after the lid opened. No AppKit run loop is needed in `ozen sync run`: `Instant` is
//! CLOCK_UPTIME_RAW on macOS (Rust std), which stops while the Mac sleeps, and the wall clock doesn't,
//! so a gap between the two means it slept; and the set of IPv4 addresses changes when it joins another
//! network. Ozen.app also passes on macOS's own wake notification: on `NSWorkspaceDidWakeNotification` it
//! runs `ozen sync wake`, which touches `WAKE`.
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime};

/// Touched by `ozen sync wake` (Ozen.app's wake handler).
pub const WAKE: &str = ".sync-wake";

/// `ozen sync wake`: tells a running `ozen sync run` the Mac just woke.
pub fn touch() -> Result<(), String> {
    std::fs::File::options()
        .create(true)
        .append(true)
        .open(WAKE)
        .and_then(|f| f.set_modified(SystemTime::now()))
        .map_err(|e| format!("{WAKE}: {e}"))
}

fn modified(p: &PathBuf) -> Option<SystemTime> {
    std::fs::metadata(p).and_then(|m| m.modified()).ok()
}

/// A wall-clock lead this big over the monotonic clock is a sleep, not drift or a small clock adjustment.
const SLEPT: Duration = Duration::from_secs(5);

pub struct Watch {
    wall: SystemTime,
    mono: Instant,
    ips: Vec<IpAddr>,
    /// `WAKE` (absolute: the link's thread doesn't move with tests' working directories) and its time.
    wake: PathBuf,
    woke: Option<SystemTime>,
}

impl Watch {
    /// Watching this folder's `WAKE` (`ozen sync run` runs in the ozen folder).
    pub fn new() -> Self {
        let wake = std::env::current_dir().unwrap_or_default().join(WAKE);
        Watch {
            wall: SystemTime::now(),
            mono: Instant::now(),
            ips: addrs(),
            woke: modified(&wake),
            wake,
        }
    }

    /// True once after the Mac slept, its addresses changed or Ozen.app said it woke, since the last call.
    pub fn changed(&mut self) -> bool {
        let woke = modified(&self.wake);
        let told = woke != self.woke;
        self.woke = woke;
        self.changed_at(SystemTime::now(), Instant::now(), addrs()) || told
    }

    fn changed_at(&mut self, wall: SystemTime, mono: Instant, ips: Vec<IpAddr>) -> bool {
        let ran = wall.duration_since(self.wall).unwrap_or_default();
        let slept = ran.saturating_sub(mono.saturating_duration_since(self.mono)) > SLEPT;
        let moved = ips != self.ips;
        (self.wall, self.mono, self.ips) = (wall, mono, ips);
        slept || moved
    }
}

/// This Mac's IPv4 addresses other than loopback, sorted. IPv6 is left out: temporary addresses rotate on
/// their own, and joining another network changes the IPv4 address anyway.
fn addrs() -> Vec<IpAddr> {
    let mut out = vec![];
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list` with a linked list we only read, then free with freeifaddrs
    unsafe {
        if libc::getifaddrs(&mut list) != 0 {
            return out;
        }
        let mut p = list;
        while !p.is_null() {
            let a = (*p).ifa_addr;
            if !a.is_null() && i32::from((*a).sa_family) == libc::AF_INET {
                let sin = &*(a as *const libc::sockaddr_in);
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if !ip.is_loopback() {
                    out.push(IpAddr::V4(ip));
                }
            }
            p = (*p).ifa_next;
        }
        libc::freeifaddrs(list);
    }
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
#[path = "wake_tests.rs"]
mod tests;
