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

    /// Relearn, then retrain in a child `ozen retrain` (this binary), waiting for it on the job's thread.
    pub fn retrain() -> Self {
        Coalesced::new(|| {
            if let Err(e) = crate::fixes::relearn() {
                eprintln!("sync: relearn: {e}");
            }
            let exe = std::env::current_exe().unwrap_or_else(|_| "ozen".into());
            if let Err(e) = std::process::Command::new(exe).arg("retrain").status() {
                eprintln!("sync: retrain: {e}");
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
                (me.job)();
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

/// Merges received records here; if any record changed, asks `after` for a relearn and retrain.
pub fn received(theirs: &Synced, after: &Coalesced) -> Result<bool, String> {
    let changed = merge::apply(theirs)?.changed;
    if changed {
        after.request();
    }
    Ok(changed)
}

#[cfg(test)]
#[path = "apply_tests.rs"]
mod tests;
