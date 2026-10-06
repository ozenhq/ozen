//! Received records land through `ozen merge`'s own path (`merge::apply`); when that changed anything,
//! this Mac relearns its fixes and retrains its voiceprints, like after `ozen merge` or a tag. The
//! retrain runs as a separate `ozen retrain` process, so neither sync nor live transcription waits on
//! it. A first exchange arrives as many batches; the retrain waits until they stop (`QUIET`), so joining
//! a Mac with months of meetings retrains once, not once per batch (OFE-20).
use crate::merge::{self, Synced};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A retrain starts once received batches have stopped for this long.
pub const QUIET: Duration = Duration::from_secs(2);

#[derive(Default)]
struct State {
    /// A worker thread exists (waiting out `quiet` or running the job).
    running: bool,
    /// It is in the job itself, not still waiting.
    in_job: bool,
    /// Asked again while in the job: one more run after it.
    queued: bool,
    /// The last request.
    last: Option<Instant>,
}

/// Runs a job on its own thread, at most one at a time, once requests have stopped for `quiet`. Asking
/// while it waits just extends the wait; asking while it runs queues exactly one more run, however many
/// times it is asked.
#[derive(Clone)]
pub struct Coalesced {
    job: Arc<dyn Fn() + Send + Sync>,
    quiet: Duration,
    state: Arc<Mutex<State>>,
}

impl Coalesced {
    pub fn new(job: impl Fn() + Send + Sync + 'static) -> Self {
        Coalesced::after_quiet(Duration::ZERO, job)
    }

    /// `new`, waiting until requests stop for `quiet` before each run.
    pub fn after_quiet(quiet: Duration, job: impl Fn() + Send + Sync + 'static) -> Self {
        Coalesced {
            job: Arc::new(job),
            quiet,
            state: Arc::default(),
        }
    }

    /// Relearn, then retrain in a child `ozen retrain`, waiting for it on the job's thread. The child is
    /// this binary: sync runs inside `ozen`, whose working directory is already the ozen checkout.
    pub fn retrain() -> Self {
        // In a test, "this binary" is the test runner: `retrain` would run every test named so.
        if cfg!(test) {
            return Coalesced::new(|| {});
        }
        Coalesced::after_quiet(QUIET, || {
            if let Err(e) = crate::fixes::relearn() {
                eprintln!("sync: relearn: {e}");
            }
            let exe = std::env::current_exe().unwrap_or_else(|_| "ozen".into());
            match std::process::Command::new(exe).arg("retrain").status() {
                Ok(s) if s.success() => {}
                Ok(s) => eprintln!("sync: ozen retrain exited with {s}"),
                Err(e) => eprintln!("sync: ozen retrain: {e}"),
            }
        })
    }

    pub fn request(&self) {
        {
            let mut s = self.state.lock().unwrap();
            s.last = Some(Instant::now());
            if s.running {
                s.queued |= s.in_job;
                return;
            }
            s.running = true;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            loop {
                // wait until requests have stopped for `quiet`
                loop {
                    let left = {
                        let s = me.state.lock().unwrap();
                        let since = s.last.map_or(me.quiet, |t| t.elapsed());
                        me.quiet.saturating_sub(since)
                    };
                    if left.is_zero() {
                        break;
                    }
                    std::thread::sleep(left);
                }
                me.state.lock().unwrap().in_job = true;
                // A job that panics (relearn on a full disk) must not leave `running` set for good, or
                // nothing would ever retrain again.
                // (the panic hook has already reported it)
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (me.job)()));
                let mut s = me.state.lock().unwrap();
                s.in_job = false;
                if !s.queued {
                    s.running = false;
                    return;
                }
                s.queued = false;
            }
        });
    }
}

/// Merges received records here; if any record changed, asks `after` for a relearn and retrain. A big
/// batch gets a restore point first, and nothing merges while sync is paused (restore.rs).
pub fn received(theirs: &Synced, after: &Coalesced) -> Result<bool, String> {
    let _merging = super::restore::before(theirs)?;
    let changed = merge::apply(theirs)?.changed;
    if changed {
        after.request();
    }
    Ok(changed)
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
