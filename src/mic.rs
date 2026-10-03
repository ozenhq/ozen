//! Which meeting app is using the microphone right now, from Core Audio's per-process objects (macOS 14.2+).
//! The menu bar's Meetings mode records while one is. ozen's own capture shows up as com.apple.replayd, so it
//! never counts as a meeting.
use std::ffi::c_void;

/// Meeting apps by bundle id prefix. Chrome covers Google Meet; FaceTime calls capture via avconferenced.
const MEETING_APPS: [(&str, &str); 7] = [
    ("us.zoom", "Zoom"),
    ("com.google.Chrome", "Chrome"),
    ("com.microsoft.teams", "Teams"),
    ("com.tinyspeck.slackmacgap", "Slack"),
    ("com.apple.FaceTime", "FaceTime"),
    ("com.apple.avconferenced", "FaceTime"),
    ("com.hnc.Discord", "Discord"),
];

const fn fourcc(s: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*s)
}
const SYSTEM_OBJECT: u32 = 1;
const SCOPE_GLOBAL: u32 = fourcc(b"glob");
const PROCESS_OBJECT_LIST: u32 = fourcc(b"prs#");
const IS_RUNNING_INPUT: u32 = fourcc(b"piri");
const BUNDLE_ID: u32 = fourcc(b"pbid");
const UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8

#[repr(C)]
struct Address {
    selector: u32,
    scope: u32,
    element: u32, // kAudioObjectPropertyElementMain
}

#[link(name = "CoreAudio", kind = "framework")]
unsafe extern "C" {
    fn AudioObjectGetPropertyDataSize(
        obj: u32,
        addr: *const Address,
        qsize: u32,
        q: *const c_void,
        size: *mut u32,
    ) -> i32;
    fn AudioObjectGetPropertyData(
        obj: u32,
        addr: *const Address,
        qsize: u32,
        q: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringGetCString(s: *const c_void, buf: *mut u8, size: isize, encoding: u32) -> u8;
    fn CFRelease(obj: *const c_void);
}

fn get<T>(obj: u32, selector: u32, out: &mut T) -> bool {
    let addr = Address {
        selector,
        scope: SCOPE_GLOBAL,
        element: 0,
    };
    let mut size = size_of::<T>() as u32;
    // SAFETY: `out` is a valid T and `size` is its size; Core Audio writes at most that many bytes.
    unsafe {
        AudioObjectGetPropertyData(
            obj,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            out as *mut T as *mut c_void,
        ) == 0
    }
}

fn bundle_id(process: u32) -> String {
    let mut s: *const c_void = std::ptr::null();
    if !get(process, BUNDLE_ID, &mut s) || s.is_null() {
        return String::new();
    }
    let mut buf = [0u8; 512];
    // SAFETY: `s` is a CFString Core Audio returned retained (+1); we copy it out, then release it once.
    let ok = unsafe { CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, UTF8) } != 0;
    unsafe { CFRelease(s) };
    if !ok {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// The meeting app (Zoom, Chrome, ...) that has a process capturing the microphone, if any.
pub fn meeting_app() -> Option<&'static str> {
    let addr = Address {
        selector: PROCESS_OBJECT_LIST,
        scope: SCOPE_GLOBAL,
        element: 0,
    };
    let mut size = 0u32;
    // SAFETY: plain out-parameters.
    if unsafe {
        AudioObjectGetPropertyDataSize(SYSTEM_OBJECT, &addr, 0, std::ptr::null(), &mut size)
    } != 0
    {
        return None;
    }
    let mut ids = vec![0u32; size as usize / size_of::<u32>()];
    // SAFETY: `ids` holds `size` bytes.
    let got = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            ids.as_mut_ptr() as *mut c_void,
        )
    };
    if got != 0 {
        return None;
    }
    ids.truncate(size as usize / size_of::<u32>());
    ids.into_iter()
        .filter(|&id| {
            let mut running = 0u32;
            get(id, IS_RUNNING_INPUT, &mut running) && running != 0
        })
        .find_map(|id| app_of(&bundle_id(id)))
}

fn app_of(bundle: &str) -> Option<&'static str> {
    MEETING_APPS
        .iter()
        .find(|(prefix, _)| bundle.starts_with(prefix))
        .map(|(_, app)| *app)
}

#[cfg(test)]
#[path = "mic_tests.rs"]
mod tests;
