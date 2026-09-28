//! Captures three audio sources via Core Audio, as 15s WAV chunks <epoch_ms>-<tag>.wav:
//!   call  = meeting apps only (the other participants), transcribed
//!   mic   = your microphone, transcribed unless it is just echo of call/local audio
//!   local = every other process (e.g. a video, `say`), used only for echo detection
//! call and local are process taps (macOS 14.4+), under the System Audio Recording permission. ScreenCaptureKit,
//! used before, needs the Screen Recording permission, which macOS periodically asks to re-confirm (a missed prompt
//! blocked every start while Settings still showed Ozen allowed), and it stops whenever the screen sleeps or locks.
//! Taps need neither.
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2_core_audio::*;
use objc2_core_audio_types::{AudioBuffer, AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::{CFDictionary, CFRetained, CFString};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString};
use std::collections::HashMap;
use std::ffi::{CStr, c_void};
use std::fs::{self, File};
use std::io::BufWriter;
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::process::exit;
use std::ptr::{NonNull, null};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::{sleep, spawn};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CHUNK: Duration = Duration::from_secs(15);
// Matched as bundle ID or its prefix, so helpers count too (Chrome plays Meet audio from com.google.Chrome.helper).
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

const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;
const GLOBAL: u32 = kAudioObjectPropertyScopeGlobal;

type Wav = hound::WavWriter<BufWriter<File>>;
type Samples = (&'static str, u32, Vec<f32>); // tag, sample rate, mono samples

struct Chunk {
    wav: Wav,
    path: PathBuf,
    rate: u32,
    start: Instant,
}

struct State {
    files: HashMap<&'static str, Chunk>,
    last: HashMap<&'static str, Instant>, // devices deliver buffers continuously (silence too), so a gap means it died
    last_mic_sound: Instant, // a real mic always has a noise floor; all-zero samples mean a dead input (e.g. AirPods)
}

struct Recorder {
    out: PathBuf,
    tmp: PathBuf,
    state: Mutex<State>,
}

impl Recorder {
    fn new(out: &Path) -> Arc<Self> {
        Arc::new(Recorder {
            out: out.to_path_buf(),
            tmp: out.join(".partial"),
            state: Mutex::new(State {
                files: HashMap::new(),
                last: HashMap::new(),
                last_mic_sound: Instant::now(),
            }),
        })
    }

    fn write(&self, (tag, rate, samples): Samples) {
        let mut st = self.state.lock().unwrap();
        st.last.insert(tag, Instant::now());
        if tag == "mic" && samples.iter().any(|&x| x != 0.0) {
            st.last_mic_sound = Instant::now();
        }
        if st
            .files
            .get(tag)
            .is_some_and(|c| c.start.elapsed() >= CHUNK || c.rate != rate)
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
                channels: 1,
                sample_rate: rate,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            };
            let Ok(wav) = hound::WavWriter::create(&path, spec) else {
                return;
            };
            st.files.insert(
                tag,
                Chunk {
                    wav,
                    path,
                    rate,
                    start: Instant::now(),
                },
            );
        }
        let wav = &mut st.files.get_mut(tag).unwrap().wav;
        for s in samples {
            let _ = wav.write_sample(s);
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

    fn finish_tags(&self, tags: &[&str]) {
        let mut st = self.state.lock().unwrap();
        for t in tags {
            self.finish(&mut st, t);
        }
    }

    fn finish_all(&self) {
        self.finish_tags(&["call", "mic", "local"]);
    }

    /// Time since `tag` last delivered audio; restarting a source counts as delivering.
    fn age(&self, tag: &'static str) -> Duration {
        let st = self.state.lock().unwrap();
        st.last.get(tag).map_or(Duration::MAX, |t| t.elapsed())
    }
    fn touch(&self, tag: &'static str) {
        self.state.lock().unwrap().last.insert(tag, Instant::now());
    }
    fn mic_silence(&self) -> Duration {
        self.state.lock().unwrap().last_mic_sound.elapsed()
    }
    fn reset_silence(&self) {
        self.state.lock().unwrap().last_mic_sound = Instant::now();
    }
}

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// One fixed-size property value (a device id, a sample rate, a CFStringRef).
fn get<T: Copy>(object: AudioObjectID, selector: u32, scope: u32) -> Option<T> {
    let a = address(selector, scope);
    let mut value = MaybeUninit::<T>::uninit();
    let mut size = size_of::<T>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&a),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(value.as_mut_ptr().cast()),
        )
    };
    (status == 0).then(|| unsafe { value.assume_init() })
}

fn ids(object: AudioObjectID, selector: u32, scope: u32) -> Vec<AudioObjectID> {
    let a = address(selector, scope);
    let mut size = 0u32;
    if unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(&a),
            0,
            null(),
            NonNull::from(&mut size),
        )
    } != 0
    {
        return vec![];
    }
    let mut v = vec![0 as AudioObjectID; size as usize / size_of::<AudioObjectID>()];
    if v.is_empty() {
        return v;
    }
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&a),
            0,
            null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(v.as_mut_ptr().cast()),
        )
    };
    if status != 0 {
        return vec![];
    }
    v.truncate(size as usize / size_of::<AudioObjectID>());
    v
}

fn string(object: AudioObjectID, selector: u32) -> Option<String> {
    let s: *mut CFString = get(object, selector, GLOBAL)?; // the caller owns the returned CFString
    Some(unsafe { CFRetained::from_raw(NonNull::new(s)?) }.to_string())
}

struct Device {
    id: AudioObjectID,
    uid: String,
    name: String,
}

fn device(id: AudioObjectID) -> Option<Device> {
    (id != 0).then_some(())?;
    Some(Device {
        id,
        uid: string(id, kAudioDevicePropertyDeviceUID)?,
        name: string(id, kAudioObjectPropertyName).unwrap_or_default(),
    })
}

fn default_device(selector: u32) -> Option<Device> {
    device(get(SYSTEM, selector, GLOBAL)?)
}

fn input_by_uid(uid: &str) -> Option<Device> {
    ids(SYSTEM, kAudioHardwarePropertyDevices, GLOBAL)
        .into_iter()
        .filter(|&d| {
            !ids(
                d,
                kAudioDevicePropertyStreams,
                kAudioObjectPropertyScopeInput,
            )
            .is_empty()
        })
        .filter_map(device)
        .find(|d| d.uid == uid)
}

/// Core Audio's objects for the meeting apps' processes that currently use audio.
fn meeting_processes() -> Vec<AudioObjectID> {
    let mut v: Vec<AudioObjectID> = ids(SYSTEM, kAudioHardwarePropertyProcessObjectList, GLOBAL)
        .into_iter()
        .filter(|&p| {
            string(p, kAudioProcessPropertyBundleID).is_some_and(|b| {
                MEETING_APPS
                    .iter()
                    .any(|m| b == *m || b.strip_prefix(m).is_some_and(|rest| rest.starts_with('.')))
            })
        })
        .collect();
    v.sort_unstable();
    v
}

struct Source {
    tag: &'static str,
    rate: u32,
    last_only: bool, // an aggregate device lists its sub-device's inputs first; the (mono) tap is the last buffer
    tx: Sender<Samples>,
}

/// Runs on Core Audio's real-time thread: copy out a mono mix and leave the file work to the writer thread.
unsafe extern "C-unwind" fn io_proc(
    _: AudioObjectID,
    _: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _: NonNull<AudioTimeStamp>,
    _: NonNull<AudioBufferList>,
    _: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let src = unsafe { &*(client as *const Source) };
    let list = unsafe { input.as_ref() };
    let all: &[AudioBuffer] =
        unsafe { std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize) };
    let bufs = if src.last_only {
        &all[all.len().saturating_sub(1)..]
    } else {
        all
    };
    // HAL IO buffers carry 32-bit float samples; one buffer may interleave several channels.
    let parts: Vec<(&[f32], usize)> = bufs
        .iter()
        .filter(|b| !b.mData.is_null() && b.mNumberChannels > 0)
        .map(|b| {
            let s = unsafe {
                std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4)
            };
            (s, b.mNumberChannels as usize)
        })
        .collect();
    let channels: usize = parts.iter().map(|p| p.1).sum();
    let frames = parts.iter().map(|(s, c)| s.len() / c).min().unwrap_or(0);
    if frames > 0 {
        let mono = (0..frames)
            .map(|i| {
                parts
                    .iter()
                    .map(|(s, c)| s[i * c..(i + 1) * c].iter().sum::<f32>())
                    .sum::<f32>()
                    / channels as f32
            })
            .collect();
        let _ = src.tx.send((src.tag, src.rate, mono));
    }
    0
}

/// A running IOProc on a device, plus the tap and aggregate device behind it for call/local.
struct Stream {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    tap: Option<(AudioObjectID, AudioObjectID)>, // (tap, aggregate device), destroyed with the stream
    _source: Box<Source>,
}

impl Drop for Stream {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.device, self.proc_id);
            AudioDeviceDestroyIOProcID(self.device, self.proc_id);
            if let Some((tap, aggregate)) = self.tap {
                AudioHardwareDestroyAggregateDevice(aggregate);
                AudioHardwareDestroyProcessTap(tap);
            }
        }
    }
}

fn check(status: i32, what: &str) -> Result<(), String> {
    if status == 0 {
        Ok(())
    } else {
        let code = status.to_be_bytes();
        let four = if code.iter().all(|c| c.is_ascii_graphic()) {
            format!(" '{}'", String::from_utf8_lossy(&code))
        } else {
            String::new()
        };
        Err(format!("{what} failed: OSStatus {status}{four}"))
    }
}

fn run(
    device: AudioObjectID,
    tag: &'static str,
    last_only: bool,
    tap: Option<(AudioObjectID, AudioObjectID)>,
    tx: &Sender<Samples>,
) -> Result<Stream, String> {
    let rate =
        get::<f64>(device, kAudioDevicePropertyNominalSampleRate, GLOBAL).unwrap_or(48000.0) as u32;
    let source = Box::new(Source {
        tag,
        rate,
        last_only,
        tx: tx.clone(),
    });
    let mut proc_id: AudioDeviceIOProcID = None;
    let client = &*source as *const Source as *mut c_void;
    check(
        unsafe {
            AudioDeviceCreateIOProcID(device, Some(io_proc), client, NonNull::from(&mut proc_id))
        },
        "create IOProc",
    )?;
    // From here the stream owns the tap and aggregate device, so an early return still destroys them.
    let stream = Stream {
        device,
        proc_id,
        tap,
        _source: source,
    };
    check(unsafe { AudioDeviceStart(device, proc_id) }, "start device")?;
    Ok(stream)
}

fn ns(key: &CStr) -> Retained<NSString> {
    NSString::from_str(key.to_str().unwrap())
}

fn dict(pairs: Vec<(&CStr, Retained<NSObject>)>) -> Retained<NSDictionary<NSString, NSObject>> {
    let keys: Vec<Retained<NSString>> = pairs.iter().map(|(k, _)| ns(k)).collect();
    let keys: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let values: Vec<Retained<NSObject>> = pairs.into_iter().map(|(_, v)| v).collect();
    NSDictionary::from_retained_objects(&keys, &values)
}

/// A private mono tap of `processes` (or of everything except them), read through a private aggregate device
/// clocked by the current output device.
fn tap(
    tag: &'static str,
    processes: &[AudioObjectID],
    exclude: bool,
    output: &Device,
    tx: &Sender<Samples>,
) -> Result<Stream, String> {
    let numbers: Vec<Retained<NSNumber>> =
        processes.iter().map(|&p| NSNumber::new_u32(p)).collect();
    let list = NSArray::from_retained_slice(&numbers);
    let description = unsafe {
        if exclude {
            CATapDescription::initMonoGlobalTapButExcludeProcesses(CATapDescription::alloc(), &list)
        } else {
            CATapDescription::initMonoMixdownOfProcesses(CATapDescription::alloc(), &list)
        }
    };
    unsafe {
        description.setPrivate(true);
        description.setName(&NSString::from_str(&format!("ozen {tag}")));
    }
    let mut tap_id: AudioObjectID = 0;
    check(
        unsafe { AudioHardwareCreateProcessTap(Some(&description), &mut tap_id) },
        "create process tap",
    )?;
    let tap_uid = unsafe { description.UUID().UUIDString() };
    let yes = || NSNumber::new_bool(true).into_super().into_super();
    let sub_device = dict(vec![(
        kAudioSubDeviceUIDKey,
        NSString::from_str(&output.uid).into_super(),
    )]);
    let sub_tap = dict(vec![
        (kAudioSubTapUIDKey, tap_uid.into_super()),
        (kAudioSubTapDriftCompensationKey, yes()),
    ]);
    let description = dict(vec![
        (
            kAudioAggregateDeviceNameKey,
            NSString::from_str(&format!("ozen {tag}")).into_super(),
        ),
        (
            kAudioAggregateDeviceUIDKey,
            NSString::from_str(&format!("ozen-{tag}-{}", std::process::id())).into_super(),
        ),
        (
            kAudioAggregateDeviceMainSubDeviceKey,
            NSString::from_str(&output.uid).into_super(),
        ),
        (kAudioAggregateDeviceIsPrivateKey, yes()),
        (kAudioAggregateDeviceTapAutoStartKey, yes()),
        (
            kAudioAggregateDeviceSubDeviceListKey,
            NSArray::from_retained_slice(&[sub_device]).into_super(),
        ),
        (
            kAudioAggregateDeviceTapListKey,
            NSArray::from_retained_slice(&[sub_tap]).into_super(),
        ),
    ]);
    // NSDictionary is toll-free bridged to CFDictionary.
    let cf = unsafe { &*(Retained::as_ptr(&description) as *const CFDictionary) };
    let mut aggregate: AudioObjectID = 0;
    if let Err(e) = check(
        unsafe { AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut aggregate)) },
        "create aggregate device",
    ) {
        unsafe { AudioHardwareDestroyProcessTap(tap_id) };
        return Err(e);
    }
    run(aggregate, tag, true, Some((tap_id, aggregate)), tx)
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
    rec: Arc<Recorder>,
    tx: Sender<Samples>,
    taps: Vec<Stream>,
    meeting: Vec<AudioObjectID>, // the meeting processes the call tap was built for
    output: Option<String>,      // UID of the output device the taps are clocked by
    mic: Option<Stream>,
    mic_device: Option<String>, // None = the default input; the built-in mic's UID while the default is silent
}

impl Capture {
    /// (Re)build the call and local taps on the current output device and meeting processes.
    fn start_taps(&mut self, why: &str) -> Result<(), String> {
        println!("starting system audio: {why}");
        self.taps.clear();
        self.rec.finish_tags(&["call", "local"]);
        self.meeting = meeting_processes();
        let output = default_device(kAudioHardwarePropertyDefaultOutputDevice);
        self.output = output.as_ref().map(|d| d.uid.clone());
        let Some(output) = output else {
            return Ok(()); // no output device: nothing plays, so there is nothing to tap
        };
        if !self.meeting.is_empty() {
            self.taps
                .push(tap("call", &self.meeting, false, &output, &self.tx)?);
        }
        self.taps
            .push(tap("local", &self.meeting, true, &output, &self.tx)?);
        self.rec.touch("local");
        println!("recording to {}", self.rec.out.display());
        Ok(())
    }

    /// (Re)open the mic: the default input, or the built-in mic while falling back.
    fn start_mic(&mut self, why: &str) -> Result<(), String> {
        println!("starting mic: {why}");
        self.mic = None;
        self.rec.finish_tags(&["mic"]);
        self.rec.touch("mic");
        let input = match &self.mic_device {
            Some(uid) => input_by_uid(uid),
            None => default_device(kAudioHardwarePropertyDefaultInputDevice),
        };
        if let Some(d) = input {
            self.mic = Some(run(d.id, "mic", false, None, &self.tx)?);
        }
        Ok(())
    }
}

/// Anything the recorder can't set up ends it; the transcriber keeps what was recorded.
fn fatal(e: String) -> ! {
    eprintln!("capture failed: {e}");
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
    // A tool without a run loop: have the HAL run its own notification thread, or devices never start delivering.
    let no_run_loop: *const c_void = null();
    let a = address(kAudioHardwarePropertyRunLoop, GLOBAL);
    unsafe {
        AudioObjectSetPropertyData(
            SYSTEM,
            NonNull::from(&a),
            0,
            null(),
            size_of::<*const c_void>() as u32,
            NonNull::from(&no_run_loop).cast(),
        )
    };
    let rec = Recorder::new(&out);
    let (tx, rx) = channel::<Samples>();
    let writer = rec.clone();
    spawn(move || {
        for samples in rx {
            writer.write(samples);
        }
    });
    let mut cap = Capture {
        rec,
        tx,
        taps: vec![],
        meeting: vec![],
        output: None,
        mic: None,
        mic_device: None,
    };
    cap.start_taps("start").unwrap_or_else(|e| fatal(e));
    cap.start_mic("start").unwrap_or_else(|e| fatal(e));

    // Meeting apps start and stop playing audio, and the output device changes (AirPods): rebuild the taps then.
    // If the mic stops delivering (its device disappeared, e.g. AirPods/iPhone mic), rebuild it.
    // A mic that delivers only zeros gets one restart per silent spell; if that doesn't bring it back, recording
    // falls back to the built-in mic until the default input changes. With no working mic to fall back to, the
    // mic-silent flag asks the user to check the input device.
    let mut retried_silence = false;
    let mut silent_default: Option<String> = None; // UID of the default input we fell back from
    let mut tick = 0u32;
    let mut last_tick = Instant::now();
    loop {
        // Short sleeps so pause/stop (SIGINT) flush and exit promptly; the checks run every 2s.
        sleep(Duration::from_millis(200));
        if stop.load(Ordering::Relaxed) {
            cap.taps.clear();
            cap.mic = None;
            sleep(Duration::from_millis(100)); // let the writer take the last buffers
            cap.rec.finish_all();
            exit(0);
        }
        if last_tick.elapsed() < Duration::from_secs(2) {
            continue;
        }
        last_tick = Instant::now();
        tick += 1;
        let output = default_device(kAudioHardwarePropertyDefaultOutputDevice).map(|d| d.uid);
        if output != cap.output {
            cap.start_taps("output device changed")
                .unwrap_or_else(|e| fatal(e));
        } else if tick.is_multiple_of(5) && meeting_processes() != cap.meeting {
            cap.start_taps("meeting apps changed")
                .unwrap_or_else(|e| fatal(e));
        } else if cap.output.is_some() && cap.rec.age("local") > Duration::from_secs(30) {
            cap.start_taps("no system audio for 30s")
                .unwrap_or_else(|e| fatal(e));
        }
        if cap.rec.age("mic") > Duration::from_secs(30) {
            cap.start_mic("no mic audio for 30s")
                .unwrap_or_else(|e| fatal(e));
            continue;
        }
        let default = default_device(kAudioHardwarePropertyDefaultInputDevice);
        if cap.mic_device.is_some() && default.as_ref().map(|d| &d.uid) != silent_default.as_ref() {
            cap.mic_device = None; // the user picked another input: use it
            flag(FALLBACK_FLAG, None);
            let name = default.as_ref().map_or("none", |d| d.name.as_str());
            cap.start_mic(&format!("default input changed to {name}"))
                .unwrap_or_else(|e| fatal(e));
            continue;
        }
        let silence = cap.rec.mic_silence();
        let built_in = || input_by_uid(BUILT_IN_MIC);
        if silence < Duration::from_secs(30) {
            retried_silence = false;
            flag(SILENT_FLAG, None);
        } else if !retried_silence {
            retried_silence = true;
            cap.start_mic("mic delivered only silence for 30s")
                .unwrap_or_else(|e| fatal(e));
        } else if silence > Duration::from_secs(60)
            && cap.mic_device.is_none()
            && let Some(b) = built_in().filter(|b| default.as_ref().is_none_or(|d| d.uid != b.uid))
        {
            let from = default
                .as_ref()
                .map_or("the default input", |d| d.name.as_str())
                .to_string();
            cap.mic_device = Some(b.uid.clone());
            silent_default = default.map(|d| d.uid);
            flag(FALLBACK_FLAG, Some(&format!("{}\n{from}", b.name)));
            cap.rec.reset_silence(); // judge the new mic on its own
            cap.start_mic(&format!("{from} is silent, switching to {}", b.name))
                .unwrap_or_else(|e| fatal(e));
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
    }
}
