//! This checkout's own processes (recorder, transcriber, helpers), found in-process with sysinfo: a process is ours
//! when its command line matches a pattern and its working directory is this checkout. The same commands run from
//! another checkout, worktree or test copy (`ozen transcribe chunks` anywhere) are someone else's: never count or
//! kill them. Patterns are `regex` crate syntax, matched like `pgrep -f`: unanchored, against argv joined by spaces.
use regex::Regex;
use std::fs;
use std::path::Path;
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

pub use sysinfo::Signal;

/// Processes whose command line matches `pattern` and whose working directory is `dir`, never this one.
fn matching<'a>(
    procs: impl IntoIterator<Item = (&'a Pid, &'a Process)>,
    pattern: &Regex,
    dir: &Path,
) -> Vec<&'a Process> {
    let me = sysinfo::get_current_pid().ok();
    procs
        .into_iter()
        .filter(|(pid, p)| {
            Some(**pid) != me
                && pattern.is_match(&p.cmd().join(" ".as_ref()).to_string_lossy())
                && p.cwd().and_then(|c| fs::canonicalize(c).ok()).as_deref() == Some(dir)
        })
        .map(|(_, p)| p)
        .collect()
}

/// Every process with just its command line and working directory loaded.
fn scan() -> System {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::Always)
            .with_cwd(UpdateKind::Always),
    );
    sys
}

fn here() -> std::path::PathBuf {
    std::env::current_dir()
        .and_then(fs::canonicalize)
        .unwrap_or_default()
}

/// PIDs of this checkout's processes matching `pattern`.
pub fn ours(pattern: &str) -> Vec<u32> {
    let sys = scan();
    let re = Regex::new(pattern).expect("process pattern");
    matching(sys.processes(), &re, &here())
        .iter()
        .map(|p| p.pid().as_u32())
        .collect()
}

pub fn running(pattern: &str) -> bool {
    !ours(pattern).is_empty()
}

pub fn signal(sig: Signal, pattern: &str) {
    let sys = scan();
    let re = Regex::new(pattern).expect("process pattern");
    for p in matching(sys.processes(), &re, &here()) {
        p.kill_with(sig);
    }
}

#[cfg(test)]
#[path = "procs_tests.rs"]
mod tests;
