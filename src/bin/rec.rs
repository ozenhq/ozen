//! Captures three audio sources via ScreenCaptureKit, as 15s WAV chunks <epoch_ms>-<tag>.wav:
//!   call  = meeting apps only (the other participants), transcribed
//!   mic   = your microphone, transcribed unless it is just echo of call/local audio
//!   local = every other app (e.g. a video, `say`), used only for echo detection. Speak Selection's
//!           system voice isn't captured by ScreenCaptureKit, so it isn't here.
use screencapturekit::error::SCStreamErrorCode;
use screencapturekit::prelude::*;
use screencapturekit::stream::delegate_trait::SCStreamDelegateTrait;
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CHUNK: Duration = Duration::from_secs(15);
const MEETING_APPS: [&str; 7] = [
    "us.zoom.xos",
    "com.google.Chrome",
    "com.microsoft.teams2",
    "com.microsoft.teams",
    "com.tinyspeck.slackmacgap",
    "com.apple.FaceTime",
    "com.hnc.Discord",
];
// ponytail: CoreAudio's UID for the Mac's own mic; a Mac without one just never falls back.
const BUILT_IN_MIC: &str = "BuiltInMicrophoneDevice";
// Flags in the working directory that `ozen health` turns into menu bar warnings.
const SILENT_FLAG: &str = "mic-silent";
const FALLBACK_FLAG: &str = "mic-fallback";
const NO_DISPLAY_FLAG: &str = "no-display";
const CAPTURE_ERROR_FLAG: &str = "capture-error"; // a rebuild failed; holds the error while retrying

type Wav = hound::WavWriter<BufWriter<File>>;

struct Chunk {
    wav: Wav,
    path: PathBuf,
    start: Instant,
}

struct State {
    files: HashMap<&'static str, Chunk>,
    last_mic: Instant, // the mic delivers buffers continuously (silence too), so a gap means it died
    last_mic_sound: Instant, // a real mic always has a noise floor; all-zero samples mean a dead input (e.g. AirPods)
    stopped: bool, // capture stopped by itself (display slept/locked, interrupted); the loop rebuilds
}

struct Recorder {
    audio_tag: &'static str,
    out: PathBuf,
    tmp: PathBuf,
    state: Mutex<State>,
}

impl Recorder {
    fn new(audio_tag: &'static str, out: &Path) -> Arc<Self> {
        let now = Instant::now();
        Arc::new(Recorder {
            audio_tag,
            out: out.to_path_buf(),
            tmp: out.join(".partial"),
            state: Mutex::new(State {
                files: HashMap::new(),
                last_mic: now,
                last_mic_sound: now,
                stopped: false,
            }),
        })
    }

    fn write(&self, sample: &CMSampleBuffer, tag: &'static str) {
        let Some(fd) = sample.format_description() else {
            return;
        };
        let (Some(rate), Some(channels)) = (fd.audio_sample_rate(), fd.audio_channel_count())
        else {
            return;
        };
        let Ok(list) = sample.audio_buffer_list() else {
            return;
        };
        let buffers: Vec<&[u8]> = list.iter().map(|b| b.data()).collect();
        let float = fd.audio_is_float();
        let bits = fd.audio_bits_per_channel().unwrap_or(32) as u16;
        if !(float && bits == 32 || !float && bits == 16) {
            return; // ScreenCaptureKit delivers 32-bit float; anything else isn't worth guessing at
        }
        let mut st = self.state.lock().unwrap();
        if tag == "mic" && buffers.iter().any(|b| b.iter().any(|&x| x != 0)) {
            st.last_mic_sound = Instant::now();
        }
        if st
            .files
            .get(tag)
            .is_some_and(|c| c.start.elapsed() >= CHUNK)
        {
            self.finish(&mut st, tag);
        }
        if !st.files.contains_key(tag) {
            let ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let path = self.tmp.join(format!("{ms}-{tag}.wav"));
            let spec = hound::WavSpec {
                channels: channels as u16,
                sample_rate: rate as u32,
                bits_per_sample: bits,
                sample_format: if float {
                    hound::SampleFormat::Float
                } else {
                    hound::SampleFormat::Int
                },
            };
            let Ok(wav) = hound::WavWriter::create(&path, spec) else {
                return;
            };
            st.files.insert(
                tag,
                Chunk {
                    wav,
                    path,
                    start: Instant::now(),
                },
            );
        }
        let wav = &mut st.files.get_mut(tag).unwrap().wav;
        // One buffer = mono or interleaved; several = one per channel, interleaved here.
        let width = (bits / 8) as usize;
        let frames = buffers.iter().map(|b| b.len() / width).min().unwrap_or(0);
        for i in 0..frames {
            for b in &buffers {
                let s = &b[i * width..(i + 1) * width];
                let _ = if float {
                    wav.write_sample(f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
                } else {
                    wav.write_sample(i16::from_le_bytes([s[0], s[1]]))
                };
            }
        }
    }

    // Finalizing the WAV writes its header; then publish it atomically.
    fn finish(&self, st: &mut State, tag: &str) {
        if let Some(c) = st.files.remove(tag)
            && c.wav.finalize().is_ok()
        {
            let _ = fs::rename(&c.path, self.out.join(c.path.file_name().unwrap()));
        }
    }

    fn finish_all(&self) {
        let mut st = self.state.lock().unwrap();
        let tags: Vec<&'static str> = st.files.keys().copied().collect();
        for t in tags {
            self.finish(&mut st, t);
        }
    }

    fn mic_age(&self) -> Duration {
        self.state.lock().unwrap().last_mic.elapsed()
    }
    fn mic_silence(&self) -> Duration {
        self.state.lock().unwrap().last_mic_sound.elapsed()
    }
    fn reset_mic(&self) {
        self.state.lock().unwrap().last_mic = Instant::now();
    }
    fn reset_silence(&self) {
        self.state.lock().unwrap().last_mic_sound = Instant::now();
    }
    fn is_stopped(&self) -> bool {
        self.state.lock().unwrap().stopped
    }
    fn clear_stopped(&self) {
        self.state.lock().unwrap().stopped = false;
    }
}

struct Output(Arc<Recorder>);

impl SCStreamOutputTrait for Output {
    fn did_output_sample_buffer(&self, sample: CMSampleBuffer, of_type: SCStreamOutputType) {
        match of_type {
            SCStreamOutputType::Audio => self.0.write(&sample, self.0.audio_tag),
            SCStreamOutputType::Microphone => {
                self.0.state.lock().unwrap().last_mic = Instant::now();
                self.0.write(&sample, "mic");
            }
            SCStreamOutputType::Screen => {}
        }
    }
}

struct Delegate(Arc<Recorder>);

impl SCStreamDelegateTrait for Delegate {
    fn did_stop_with_error(&self, error: SCError) {
        eprintln!("stream stopped: {error}");
        self.0.state.lock().unwrap().stopped = true;
    }
}

enum StartError {
    NoDisplay, // screen asleep or locked: wait for it rather than exit
    Other(SCError),
}

fn filters() -> Result<(SCContentFilter, SCContentFilter), StartError> {
    let content = SCShareableContent::get().map_err(StartError::Other)?;
    let display = content
        .displays()
        .into_iter()
        .next()
        .ok_or(StartError::NoDisplay)?;
    let apps = content.applications();
    let meeting: Vec<&SCRunningApplication> = apps
        .iter()
        .filter(|a| MEETING_APPS.contains(&a.bundle_identifier().as_str()))
        .collect();
    let build = |b: screencapturekit::stream::content_filter::SCContentFilterBuilder| {
        b.build().map_err(StartError::Other)
    };
    Ok((
        build(
            SCContentFilter::create()
                .with_display(&display)
                .with_including_applications(&meeting, &[]),
        )?,
        build(
            SCContentFilter::create()
                .with_display(&display)
                .with_excluding_applications(&meeting, &[]),
        )?,
    ))
}

fn config(mic: Option<&Option<String>>) -> Result<SCStreamConfiguration, SCError> {
    let mut cfg = SCStreamConfiguration::new()
        .with_captures_audio(true)
        .with_excludes_current_process_audio(true)
        .with_sample_rate(48000)
        .with_channel_count(1)
        .with_width(2)
        .with_height(2)
        .with_minimum_frame_interval(&CMTime::new(1, 1));
    if let Some(device) = mic {
        cfg = cfg.with_captures_microphone(true)?;
        if let Some(id) = device {
            cfg = cfg.with_microphone_capture_device_id(id)?;
        }
    }
    Ok(cfg)
}

fn flag(name: &str, contents: Option<&str>) {
    match contents {
        Some(c) => {
            let _ = fs::write(name, c);
        }
        None => {
            let _ = fs::remove_file(name);
        }
    }
}

struct Capture {
    call: Arc<Recorder>,
    local: Arc<Recorder>,
    streams: Vec<SCStream>,
    mic_device: Option<String>, // None = the default input; the built-in mic while the default is silent
}

impl Capture {
    /// (Re)build both streams on the current displays, meeting apps and input. Ok(false) = no display yet.
    fn start(&mut self, why: &str) -> Result<bool, SCError> {
        println!("starting capture: {why}");
        for s in self.streams.drain(..) {
            let _ = s.stop_capture();
        }
        self.call.finish_all();
        self.local.finish_all();
        let (call_filter, local_filter) = match filters() {
            Ok(f) => f,
            Err(StartError::NoDisplay) => {
                flag(NO_DISPLAY_FLAG, Some(""));
                return Ok(false);
            }
            Err(StartError::Other(e)) => return Err(e),
        };
        let mut call = SCStream::new_with_delegate(
            &call_filter,
            &config(Some(&self.mic_device))?,
            Delegate(self.call.clone()),
        )?;
        call.add_output_handler(Output(self.call.clone()), SCStreamOutputType::Audio)?;
        call.add_output_handler(Output(self.call.clone()), SCStreamOutputType::Microphone)?;
        let mut local = SCStream::new_with_delegate(
            &local_filter,
            &config(None)?,
            Delegate(self.local.clone()),
        )?;
        local.add_output_handler(Output(self.local.clone()), SCStreamOutputType::Audio)?;
        call.start_capture()?;
        local.start_capture()?;
        self.streams = vec![call, local];
        self.call.clear_stopped();
        self.local.clear_stopped();
        self.call.reset_mic();
        flag(NO_DISPLAY_FLAG, None);
        flag(CAPTURE_ERROR_FLAG, None);
        println!("recording to {}", self.call.out.display());
        Ok(true)
    }

    fn refresh_filters(&self) {
        if let (Ok((call, local)), [c, l]) = (filters(), self.streams.as_slice()) {
            let _ = c.update_content_filter(&call);
            let _ = l.update_content_filter(&local);
        }
    }
}

/// Whether a failed capture rebuild is worth retrying. A missing permission or entitlement never fixes itself;
/// everything else (audio not ready after wake, a mic that vanished, an interrupted service) usually does.
fn retryable(e: &SCError) -> bool {
    !matches!(
        e.stream_error_code(),
        Some(SCStreamErrorCode::UserDeclined | SCStreamErrorCode::MissingEntitlements)
    )
}

/// A missing permission ends the recorder; `ozen health` recognizes that case by its text.
fn fatal(e: SCError) -> ! {
    if e.stream_error_code() == Some(SCStreamErrorCode::UserDeclined) {
        eprintln!("recording permission missing: declined TCCs ({e})");
    } else {
        eprintln!("capture failed: {e}");
    }
    exit(1);
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "chunks".into()));
    fs::create_dir_all(out.join(".partial")).expect("create chunk dir");
    // A recorder killed hard leaves its in-progress chunks in .partial. ffmpeg (the transcriber's loader)
    // reads them in full without the final header sizes, so publish them instead of losing that audio.
    for e in fs::read_dir(out.join(".partial"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let _ = fs::rename(e.path(), out.join(e.file_name()));
    }
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        signal_hook::flag::register(sig, stop.clone()).expect("signal handler");
    }
    flag(FALLBACK_FLAG, None);
    let mut cap = Capture {
        call: Recorder::new("call", &out),
        local: Recorder::new("local", &out),
        streams: vec![],
        mic_device: None,
    };
    let mut live = cap.start("start").unwrap_or_else(|e| fatal(e));

    // Meeting apps opened after start must join the call filter, so refresh the filters periodically.
    // If the mic stops delivering (its device disappeared, e.g. AirPods/iPhone mic) or capture stopped, rebuild.
    // A mic that delivers only zeros gets one restart per silent spell; if that doesn't bring it back, recording
    // falls back to the built-in mic until the default input changes. With no working mic to fall back to, the
    // mic-silent flag asks the user to check the input device.
    let mut retried_silence = false;
    let mut silent_default: Option<String> = None; // id of the default input we fell back from
    let mut tick = 0u32;
    let mut last_tick = Instant::now();
    loop {
        // Short sleeps so pause/stop (SIGINT) flush and exit promptly; the checks run every 2s.
        sleep(Duration::from_millis(200));
        if stop.load(Ordering::Relaxed) {
            cap.call.finish_all();
            cap.local.finish_all();
            flag(NO_DISPLAY_FLAG, None);
            flag(CAPTURE_ERROR_FLAG, None);
            exit(0);
        }
        if last_tick.elapsed() < Duration::from_secs(2) {
            continue;
        }
        last_tick = Instant::now();
        tick += 1;
        // Once recording has worked, a failed rebuild is usually transient (right after wake, "Stream failed to
        // start audio" while audio comes back), so retry it like a missing display instead of exiting and leaving
        // the meeting unrecorded. Only a missing permission is final.
        let restart = |cap: &mut Capture, why: &str| {
            cap.start(why).unwrap_or_else(|e| {
                if !retryable(&e) {
                    fatal(e);
                }
                eprintln!("capture failed, retrying: {e}");
                flag(CAPTURE_ERROR_FLAG, Some(&e.to_string()));
                false
            })
        };
        let stopped = cap.call.is_stopped() || cap.local.is_stopped();
        if !live || stopped {
            if tick.is_multiple_of(5) || stopped {
                live = restart(
                    &mut cap,
                    if live {
                        "capture stopped"
                    } else {
                        "retrying (no display, or the last start failed)"
                    },
                );
            }
            continue;
        }
        if cap.call.mic_age() > Duration::from_secs(30) {
            live = restart(&mut cap, "no mic audio for 30s");
            continue;
        }
        let default = AudioInputDevice::default_device();
        if cap.mic_device.is_some() && default.as_ref().map(|d| &d.id) != silent_default.as_ref() {
            cap.mic_device = None; // the user picked another input: use it
            flag(FALLBACK_FLAG, None);
            let name = default.as_ref().map_or("none", |d| d.name.as_str());
            live = restart(&mut cap, &format!("default input changed to {name}"));
            continue;
        }
        let silence = cap.call.mic_silence();
        let built_in = || {
            AudioInputDevice::list()
                .into_iter()
                .find(|d| d.id == BUILT_IN_MIC)
        };
        if silence < Duration::from_secs(30) {
            retried_silence = false;
            flag(SILENT_FLAG, None);
        } else if !retried_silence {
            retried_silence = true;
            live = restart(&mut cap, "mic delivered only silence for 30s");
            continue;
        } else if silence > Duration::from_secs(60)
            && cap.mic_device.is_none()
            && let Some(b) = built_in().filter(|b| default.as_ref().is_none_or(|d| d.id != b.id))
        {
            let from = default
                .as_ref()
                .map_or("the default input", |d| d.name.as_str())
                .to_string();
            cap.mic_device = Some(b.id.clone());
            silent_default = default.map(|d| d.id);
            flag(FALLBACK_FLAG, Some(&format!("{}\n{from}", b.name)));
            cap.call.reset_silence(); // judge the new mic on its own
            live = restart(
                &mut cap,
                &format!("{from} is silent, switching to {}", b.name),
            );
            continue;
        } else if silence > Duration::from_secs(60) {
            let using = if cap.mic_device.is_some() {
                built_in()
            } else {
                default
            };
            flag(
                SILENT_FLAG,
                Some(
                    using
                        .as_ref()
                        .map_or("the input device", |d| d.name.as_str()),
                ),
            );
        }
        if tick.is_multiple_of(5) {
            cap.refresh_filters();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::retryable;
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
}
