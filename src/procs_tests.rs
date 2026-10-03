use super::*;

#[test]
fn keeps_only_processes_in_this_checkout() {
    let tmp = std::env::temp_dir().canonicalize().unwrap();
    let here = tmp.join(format!("ozen-pids-{}", std::process::id()));
    let other = tmp.join(format!("ozen-pids-other-{}", std::process::id()));
    fs::create_dir_all(&here).unwrap();
    fs::create_dir_all(&other).unwrap();
    let spawn = |dir: &Path| {
        std::process::Command::new("sleep")
            .arg("4242")
            .current_dir(dir)
            .spawn()
            .unwrap()
    };
    let (mut mine, mut theirs) = (spawn(&here), spawn(&other));
    let sys = scan();
    let found: Vec<u32> = matching(sys.processes(), &Regex::new("^sleep 4242$").unwrap(), &here)
        .iter()
        .map(|p| p.pid().as_u32())
        .collect();
    let _ = (mine.kill(), theirs.kill(), mine.wait(), theirs.wait());
    assert_eq!(found, [mine.id()]);
    let _ = (fs::remove_dir(&here), fs::remove_dir(&other));
}

#[test]
fn signal_reaches_only_ours() {
    let tmp = std::env::temp_dir().canonicalize().unwrap();
    let other = tmp.join(format!("ozen-sig-other-{}", std::process::id()));
    fs::create_dir_all(&other).unwrap();
    let sleep = |dir: &Path| {
        std::process::Command::new("sleep")
            .arg("4343")
            .current_dir(dir)
            .spawn()
            .unwrap()
    };
    // "ours" is the process's working directory, shared by every test thread: hold it still
    let _cwd = crate::CWD.lock().unwrap();
    let (mut mine, mut theirs) = (sleep(&here()), sleep(&other));
    signal(Signal::Term, "^sleep 4343$");
    let mut killed = None;
    for _ in 0..50 {
        killed = mine.try_wait().unwrap();
        if killed.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let alive = theirs.try_wait().unwrap().is_none();
    let _ = (mine.kill(), mine.wait(), theirs.kill(), theirs.wait());
    assert!(killed.is_some_and(|s| !s.success()) && alive);
    let _ = fs::remove_dir(&other);
}
