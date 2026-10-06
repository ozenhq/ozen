//! Received records land through `ozen merge`'s own path (`merge::apply`); when that changed anything,
//! this Mac relearns its fixes and retrains its voiceprints, like after `ozen merge` or a tag. The
//! retrain runs as a separate `ozen retrain` process, so neither sync nor live transcription waits on
//! it, and a burst of received batches retrains at most twice: the one running, then one more.
use crate::merge::{self, Synced};
use std::sync::{Arc, Mutex};

/// Runs a job on its own thread, at most one at a time. Asking while it runs queues exactly one more
/// run, however many times it is asked.
#[derive(Clone)]
pub struct Coalesced {
    job: Arc<dyn Fn() + Send + Sync>,
    /// (running, queued)
    state: Arc<Mutex<(bool, bool)>>,
}

impl Coalesced {
    pub fn new(job: impl Fn() + Send + Sync + 'static) -> Self {
        Coalesced {
            job: Arc::new(job),
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
        Coalesced::new(|| {
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
            if s.0 {
                s.1 = true;
                return;
            }
            s.0 = true;
        }
        let me = self.clone();
        std::thread::spawn(move || {
            loop {
                // A job that panics (relearn on a full disk) must not leave `running` set for good, or
                // nothing would ever retrain again.
                // (the panic hook has already reported it)
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (me.job)()));
                let mut s = me.state.lock().unwrap();
                if !s.1 {
                    s.0 = false;
                    return;
                }
                s.1 = false;
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
