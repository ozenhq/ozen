//! Asking the ozen CLI (src/main.rs), which owns the recorder and transcriber and decides what the panel shows.
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;

static DIR: OnceLock<PathBuf> = OnceLock::new();

/// The ozen checkout the app works on: the first argument, else ~/ozen.
pub fn set_dir(dir: PathBuf) {
    let _ = DIR.set(dir);
}

pub fn dir() -> &'static PathBuf {
    DIR.get().expect("set_dir first")
}

fn command(args: &[&str]) -> Command {
    let mut c = Command::new(dir().join("target/release/ozen"));
    c.args(args).stdin(Stdio::null());
    c
}

/// The CLI's JSON answer. Blocks: use it for commands that only read a few files.
pub fn json(args: &[&str]) -> Value {
    command(args)
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice(&o.stdout).ok())
        .unwrap_or(Value::Null)
}

/// Runs the CLI off the main thread, then `done(stdout, stderr, exit code)` on it (trimmed).
pub fn run(args: &[&str], done: impl FnOnce(String, String, i32) + Send + 'static) {
    run_input(args, String::new(), done);
}

/// `run`, with `input` on its stdin.
pub fn run_input(
    args: &[&str],
    input: String,
    done: impl FnOnce(String, String, i32) + Send + 'static,
) {
    let mut c = command(args);
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    std::thread::spawn(move || {
        let ran = c.spawn().and_then(|mut child| {
            let mut stdin = child.stdin.take().expect("piped");
            std::io::Write::write_all(&mut stdin, input.as_bytes())?;
            drop(stdin); // EOF
            child.wait_with_output()
        });
        let (out, err, code) = match ran {
            Ok(o) => (
                String::from_utf8_lossy(&o.stdout).trim().to_string(),
                String::from_utf8_lossy(&o.stderr).trim().to_string(),
                o.status.code().unwrap_or(-1),
            ),
            Err(e) => (
                String::new(),
                format!(
                    "can't run {}: {e}",
                    dir().join("target/release/ozen").display()
                ),
                -1,
            ),
        };
        dispatch2::DispatchQueue::main().exec_async(move || done(out, err, code));
    });
}
