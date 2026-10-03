#[test]
fn drops_only_idle_script_envs() {
    let ps = "/u/.cache/uv/environments-v2/transcribe-1b646a70201e1ec6/bin/python3 transcribe.py chunks\n";
    assert!(super::stale_env("transcribe-04996fcebd6a04ca", ps));
    assert!(super::stale_env("asr-2871e1033d644c4b", ps));
    assert!(!super::stale_env("transcribe-1b646a70201e1ec6", ps)); // running
    assert!(!super::stale_env("train-02cfad274e3027bf", ps)); // not one of our scripts
    assert!(!super::stale_env("transcribe-notahash", ps));
}

#[test]
fn rotates_only_a_log_past_one_mib() {
    let _cwd = super::CWD.lock().unwrap();
    let dir = std::env::temp_dir().join(format!("ozen-rotate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_current_dir(&dir).unwrap();
    std::fs::write("start.log", vec![b'x'; 1000]).unwrap();
    super::rotate_log();
    assert!(
        std::path::Path::new("start.log").exists(),
        "small log stays"
    );
    std::fs::write("start.log", vec![b'x'; (1 << 20) + 1]).unwrap();
    super::rotate_log();
    assert!(!std::path::Path::new("start.log").exists());
    assert_eq!(
        std::fs::metadata("start.log.1").unwrap().len(),
        (1 << 20) + 1
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

use super::recorder_blocked;

#[test]
fn blocked_only_when_the_last_recorder_start_failed() {
    let failed = "recording to /x/chunks\nFatal error: ... declined TCCs ...\n";
    assert!(recorder_blocked(failed));
    assert!(recorder_blocked(&format!(
        "{failed}transcribing chunks -> transcript.txt\nFetching 4 files\n"
    )));
    assert!(!recorder_blocked(&format!(
        "{failed}recording to /x/chunks\n"
    )));
    assert!(!recorder_blocked(""));
}
