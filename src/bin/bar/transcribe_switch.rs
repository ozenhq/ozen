//! The CLI commands that apply the Transcribe switch, run one after another.
use crate::{APP, cli};

/// The CLI commands, run in turn, that apply the Transcribe switch right away. Off stops the transcriber and the
/// audio waits in chunks/ (the recording goes on); on transcribes what's waiting, then keeps going live if recording.
pub(super) fn transcribe_switched(
    on: bool,
    state: &str,
    queued: i64,
) -> &'static [&'static [&'static str]] {
    match (on, state) {
        (false, "recording") => &[&["record"], &["process", "stop"]],
        (false, "paused" | "processing") => &[&["process", "stop"]],
        (true, "recording") => &[&["start"]],
        (true, _) if queued > 0 => &[&["process"]],
        _ => &[],
    }
}

pub(super) fn run_in_turn(commands: &'static [&'static [&'static str]]) {
    match commands.split_first() {
        Some((first, rest)) => cli::run(first, move |_, _, _| run_in_turn(rest)),
        None => APP.with(|a| a.get().unwrap().refresh_state()),
    }
}
