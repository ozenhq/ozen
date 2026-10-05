//! Runs a job in one process at a time across the whole checkout, coalescing the rest: a caller that
//! finds the job running leaves a mark and returns, and whoever holds the lock runs it once more. So
//! two retrains (one from sync, one from a tag) never overlap, and the later one's inputs still get
//! trained on (OFE-93). Within a process, sync's `Coalesced` already does the same.
use std::fs::{self, File, TryLockError};
use std::path::Path;

/// Run `job` under `lock`, again while `again` is marked. Returns at once if another holder will run it.
pub fn run(lock: &str, again: &str, mut job: impl FnMut()) {
    let f = File::create(lock).unwrap_or_else(|e| panic!("create {lock}: {e}"));
    loop {
        match f.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) => {
                // Mark, then look again: the holder may have released between our try and the mark.
                let _ = fs::write(again, "");
                if let Err(TryLockError::WouldBlock) = f.try_lock() {
                    return; // the holder sees the mark after its run and runs once more
                }
            }
            // ponytail: no working lock (odd filesystem): run unguarded rather than drop the request
            Err(TryLockError::Error(e)) => eprintln!("{lock}: {e}; running without it"),
        }
        loop {
            let _ = fs::remove_file(again);
            job();
            if !Path::new(again).exists() {
                break;
            }
        }
        let _ = f.unlock();
        // A mark left between our last check and the unlock would otherwise be lost: take it back.
        if !Path::new(again).exists() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::run;
    use std::sync::atomic::{AtomicUsize, Ordering::SeqCst};
    use std::sync::mpsc::{Receiver, channel};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn concurrent_callers_never_overlap_and_the_last_input_is_trained_on() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join(".lock").to_str().unwrap().to_string();
        let again = dir.path().join(".again").to_str().unwrap().to_string();
        let running = Arc::new(AtomicUsize::new(0));
        let runs = Arc::new(AtomicUsize::new(0));
        let tags = Arc::new(Mutex::new(Vec::<&str>::new())); // what's on disk
        let seen = Arc::new(Mutex::new(Vec::<&str>::new())); // what the last run trained on
        let (started, sync_started) = channel();
        // `wait`: start only once sync's retrain is running, as a tag made mid-retrain does.
        let caller = |tag: &'static str, wait: Option<Receiver<()>>| {
            let (lock, again) = (lock.clone(), again.clone());
            let (running, runs, tags, seen) =
                (running.clone(), runs.clone(), tags.clone(), seen.clone());
            let started = started.clone();
            std::thread::spawn(move || {
                if let Some(w) = wait {
                    w.recv().unwrap();
                }
                tags.lock().unwrap().push(tag); // the tag is written, then retrain is called
                run(&lock, &again, || {
                    assert_eq!(running.fetch_add(1, SeqCst), 0, "two runs overlapped");
                    let input = tags.lock().unwrap().clone();
                    let _ = started.send(());
                    std::thread::sleep(Duration::from_millis(200));
                    *seen.lock().unwrap() = input;
                    runs.fetch_add(1, SeqCst);
                    running.fetch_sub(1, SeqCst);
                });
            })
        };
        let sync = caller("from sync", None);
        let tag = caller("from ozen tag", Some(sync_started)); // arrives while sync's retrain runs
        sync.join().unwrap();
        tag.join().unwrap();
        assert_eq!(runs.load(SeqCst), 2);
        assert_eq!(*seen.lock().unwrap(), ["from sync", "from ozen tag"]);
    }
}
