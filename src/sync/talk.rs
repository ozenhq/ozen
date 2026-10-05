//! One authenticated connection to another Mac (local.rs): length-prefixed frames both ways, and the
//! loop that runs the summary protocol over them.
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use std::sync::mpsc;
use std::time::Duration;

/// The relay's frame cap too: a sealed frame is at most 60 KiB (seal.rs).
pub(super) const MAX_FRAME: usize = 64 << 10;

pub(super) fn write_frame(s: &mut TcpStream, f: &[u8]) -> Result<(), String> {
    if f.len() > MAX_FRAME {
        return Err(format!("frame of {} bytes is over {MAX_FRAME}", f.len()));
    }
    s.write_all(&(f.len() as u32).to_be_bytes())
        .and_then(|()| s.write_all(f))
        .map_err(|e| e.to_string())
}

/// The next frame, or None when the peer hung up.
pub(super) fn read_frame(s: &mut TcpStream) -> Result<Option<Vec<u8>>, String> {
    let mut n = [0; 4];
    match s.read_exact(&mut n) {
        Err(e) if e.kind() == ErrorKind::UnexpectedEof => return Ok(None),
        r => r.map_err(|e| e.to_string())?,
    }
    let n = u32::from_be_bytes(n) as usize;
    if n > MAX_FRAME {
        return Err(format!("frame of {n} bytes is over {MAX_FRAME}"));
    }
    let mut f = vec![0; n];
    s.read_exact(&mut f).map_err(|e| e.to_string())?;
    Ok(Some(f))
}

/// What `talk` hands its step: make our hello, handle a frame of theirs, or a timer tick (to send
/// local edits).
#[derive(Clone, Copy)]
pub enum Input<'a> {
    Hello,
    Frame(&'a [u8]),
    Tick,
}

/// Runs the summary protocol with one authenticated peer until it hangs up. `step` gets our hello
/// first, then each of their frames, and a `Tick` whenever `every` passes with no frame
/// (protocol::Session); each returns frames to send. Frames are read and written on their own threads,
/// so two Macs sending big hellos at once never wait on each other's reads.
pub fn talk(
    s: TcpStream,
    every: Duration,
    mut step: impl FnMut(Input) -> Result<Vec<Vec<u8>>, String>,
) -> Result<(), String> {
    let mut w = s.try_clone().map_err(|e| e.to_string())?;
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let writer = std::thread::spawn(move || {
        for f in rx {
            if let Err(e) = write_frame(&mut w, &f) {
                let _ = w.shutdown(std::net::Shutdown::Both); // unblocks the reader too
                return Err(e);
            }
        }
        Ok::<(), String>(())
    });
    let mut r = s.try_clone().map_err(|e| e.to_string())?;
    let (ftx, frames) = mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let f = read_frame(&mut r);
            let last = !matches!(f, Ok(Some(_)));
            if ftx.send(f).is_err() || last {
                return;
            }
        }
    });
    let send = |fs: Vec<Vec<u8>>| fs.into_iter().all(|f| tx.send(f).is_ok());
    let mut out = Ok(());
    if send(step(Input::Hello)?) {
        out = loop {
            let fs = match frames.recv_timeout(every) {
                Ok(Ok(Some(f))) => step(Input::Frame(&f)),
                Ok(Ok(None)) => break Ok(()),
                Ok(Err(e)) => break Err(e),
                Err(mpsc::RecvTimeoutError::Timeout) => step(Input::Tick),
                Err(mpsc::RecvTimeoutError::Disconnected) => break Ok(()),
            };
            match fs {
                Ok(fs) => {
                    if !send(fs) {
                        break Ok(()); // the writer stopped: its error is below
                    }
                }
                Err(e) => break Err(e),
            }
        };
    }
    let _ = s.shutdown(std::net::Shutdown::Both); // ends the reader and a blocked writer
    drop(tx);
    let wrote = writer.join().unwrap_or(Err("writer panicked".into()));
    out.and(wrote)
}
