use super::{checkout_gone, fs, publish_partials, retryable};
use screencapturekit::error::{SCError, SCStreamErrorCode as C};

#[test]
fn only_permission_errors_end_the_recorder() {
    let e = SCError::from_stream_error_code;
    assert!(!retryable(&e(C::UserDeclined)));
    assert!(!retryable(&e(C::MissingEntitlements)));
    // Seen live on wake from display sleep: "Stream failed to start audio".
    assert!(retryable(&e(C::FailedToStartAudioCapture)));
    assert!(retryable(&e(C::FailedToStartMicrophoneCapture)));
    assert!(retryable(&e(C::NoCaptureSource)));
    assert!(retryable(&e(C::InternalError)));
}

#[test]
fn publishes_leftover_chunks_and_drops_empty_ones() {
    let out = std::env::temp_dir().join(format!("ozen-partials-{}", std::process::id()));
    fs::create_dir_all(out.join(".partial")).unwrap();
    fs::write(out.join(".partial/1-mic.wav"), b"RIFF....WAVE").unwrap();
    fs::write(out.join(".partial/2-call.wav"), b"").unwrap();
    publish_partials(&out);
    assert!(out.join("1-mic.wav").exists());
    assert!(!out.join("2-call.wav").exists() && !out.join(".partial/2-call.wav").exists());
    fs::remove_dir_all(&out).unwrap();
}

#[test]
fn notices_a_deleted_checkout() {
    let root = std::env::temp_dir().join(format!("ozen-gone-{}", std::process::id()));
    let out = root.join("chunks");
    fs::create_dir_all(out.join(".partial")).unwrap();
    assert!(!checkout_gone(&out));
    fs::remove_dir_all(&root).unwrap(); // like `git worktree remove`
    assert!(checkout_gone(&out));
}
