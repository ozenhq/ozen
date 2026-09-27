// Captures three audio sources via ScreenCaptureKit, as 15s WAV chunks <epoch_ms>-<tag>.wav:
//   call  = meeting apps only (the other participants), transcribed
//   mic   = your microphone, transcribed unless it is just echo of call/local audio
//   local = every other app (e.g. Speak Selection reading text aloud), used only for echo detection
import AVFoundation
import Foundation
import ScreenCaptureKit

let outDir = URL(fileURLWithPath: CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "chunks")
let tmpDir = outDir.appendingPathComponent(".partial")
try FileManager.default.createDirectory(at: tmpDir, withIntermediateDirectories: true)
let chunkSeconds = 15.0
let meetingApps: Set<String> = [
    "us.zoom.xos", "com.google.Chrome", "com.microsoft.teams2", "com.microsoft.teams",
    "com.tinyspeck.slackmacgap", "com.apple.FaceTime", "com.hnc.Discord",
]

final class Recorder: NSObject, SCStreamOutput, SCStreamDelegate {
    let audioTag: String
    var files: [String: (file: AVAudioFile, url: URL, start: Date)] = [:]
    var lastMic = Date()  // the mic delivers buffers continuously (silence too), so a gap means it died
    var lastMicSound = Date()  // a real mic always has a noise floor; all-zero samples mean a dead input (e.g. AirPods)
    let lock = NSLock()
    init(audioTag: String) { self.audioTag = audioTag }

    func stream(_ s: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        let tag: String
        switch type {
        case .audio: tag = audioTag
        case .microphone: tag = "mic"; lock.lock(); lastMic = Date(); lock.unlock()
        default: return
        }
        guard let desc = sb.formatDescription,
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(desc),
              let fmt = AVAudioFormat(streamDescription: asbd) else { return }
        lock.lock(); defer { lock.unlock() }
        if let cur = files[tag], Date().timeIntervalSince(cur.start) >= chunkSeconds { finish(tag) }
        if files[tag] == nil {
            let url = tmpDir.appendingPathComponent("\(Int(Date().timeIntervalSince1970 * 1000))-\(tag).wav")
            guard let f = try? AVAudioFile(forWriting: url, settings: fmt.settings,
                                           commonFormat: fmt.commonFormat, interleaved: fmt.isInterleaved) else { return }
            files[tag] = (f, url, Date())
        }
        try? sb.withAudioBufferList { abl, _ in
            if tag == "mic", abl.contains(where: { b in
                UnsafeRawBufferPointer(start: b.mData, count: Int(b.mDataByteSize)).contains { $0 != 0 }
            }) { lastMicSound = Date() }
            guard let pcm = AVAudioPCMBuffer(pcmFormat: fmt, bufferListNoCopy: abl.unsafePointer) else { return }
            try? files[tag]?.file.write(from: pcm)
        }
    }

    // Closing the AVAudioFile (dropping the ref) finalizes the header; then publish it atomically.
    func finish(_ tag: String) {
        guard let cur = files.removeValue(forKey: tag) else { return }
        try? FileManager.default.moveItem(at: cur.url, to: outDir.appendingPathComponent(cur.url.lastPathComponent))
    }

    func resetMic() { lock.lock(); lastMic = Date(); lock.unlock() }
    var micAge: TimeInterval { lock.lock(); defer { lock.unlock() }; return Date().timeIntervalSince(lastMic) }
    var micSilence: TimeInterval { lock.lock(); defer { lock.unlock() }; return Date().timeIntervalSince(lastMicSound) }

    func finishAll() { lock.lock(); files.keys.forEach(finish); lock.unlock() }

    var stopped = false  // set when capture stops by itself (display slept/locked, interrupted); the loop rebuilds
    var isStopped: Bool { lock.lock(); defer { lock.unlock() }; return stopped }
    func clearStopped() { lock.lock(); stopped = false; lock.unlock() }

    func stream(_ s: SCStream, didStopWithError error: Error) {
        FileHandle.standardError.write("stream stopped: \(error)\n".data(using: .utf8)!)
        lock.lock(); stopped = true; lock.unlock()
    }
}

struct NoDisplay: Error {}  // screen asleep or locked: wait for it rather than exit

func filters() async throws -> (call: SCContentFilter, local: SCContentFilter) {
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    guard let display = content.displays.first else { throw NoDisplay() }
    let meeting = content.applications.filter { meetingApps.contains($0.bundleIdentifier) }
    return (SCContentFilter(display: display, including: meeting, exceptingWindows: []),
            SCContentFilter(display: display, excludingApplications: meeting, exceptingWindows: []))
}

func config(mic: Bool) -> SCStreamConfiguration {
    let cfg = SCStreamConfiguration()
    cfg.capturesAudio = true
    cfg.captureMicrophone = mic
    cfg.excludesCurrentProcessAudio = true
    cfg.sampleRate = 48000
    cfg.channelCount = 1
    cfg.width = 2
    cfg.height = 2
    cfg.minimumFrameInterval = CMTime(value: 1, timescale: 1)
    return cfg
}

let callRec = Recorder(audioTag: "call"), localRec = Recorder(audioTag: "local")
let q = DispatchQueue(label: "audio")
var callStream: SCStream?, localStream: SCStream?
// While there is no display to capture, this flag tells the menu bar (via `ozen health`) that recording is on hold.
let noDisplayFlag = URL(fileURLWithPath: "no-display")

// (Re)build both streams on the current displays, meeting apps and default input. A missing display is
// reported as false (retry later); any other error, e.g. the recording permission, is thrown.
@MainActor func startStreams(_ why: String) async throws -> Bool {
    print("starting capture: \(why)"); fflush(stdout)
    for s in [callStream, localStream].compactMap({ $0 }) { try? await s.stopCapture() }
    callStream = nil; localStream = nil
    callRec.finishAll(); localRec.finishAll()
    let f: (call: SCContentFilter, local: SCContentFilter)
    do { f = try await filters() } catch is NoDisplay {
        FileManager.default.createFile(atPath: noDisplayFlag.path, contents: nil)
        return false
    }
    let c = SCStream(filter: f.call, configuration: config(mic: true), delegate: callRec)
    let l = SCStream(filter: f.local, configuration: config(mic: false), delegate: localRec)
    try c.addStreamOutput(callRec, type: .audio, sampleHandlerQueue: q)
    try c.addStreamOutput(callRec, type: .microphone, sampleHandlerQueue: q)
    try l.addStreamOutput(localRec, type: .audio, sampleHandlerQueue: q)
    try await c.startCapture()
    try await l.startCapture()
    callStream = c; localStream = l
    callRec.clearStopped(); localRec.clearStopped()
    callRec.resetMic()
    try? FileManager.default.removeItem(at: noDisplayFlag)
    print("recording to \(outDir.path)"); fflush(stdout)
    return true
}

signal(SIGINT, SIG_IGN); signal(SIGTERM, SIG_IGN)
for sig in [SIGINT, SIGTERM] {
    let src = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    src.setEventHandler {
        callRec.finishAll(); localRec.finishAll()
        try? FileManager.default.removeItem(at: noDisplayFlag)
        exit(0)
    }
    src.resume()
    _ = Unmanaged.passRetained(src)
}

var live = try await startStreams("start")

// Meeting apps opened after start must join the call filter, so refresh the filters periodically.
// If the mic stops delivering (its device disappeared, e.g. AirPods/iPhone mic) or capture stopped, rebuild.
// A live mic delivers silence too, so a gap = dead. A mic that delivers only zeros gets one restart per
// silent spell; if that doesn't bring it back, the mic-silent flag tells the menu bar to ask the user to
// check the input device.
let silentFlag = URL(fileURLWithPath: "mic-silent")
var retriedSilence = false
var tick = 0
while true {
    try await Task.sleep(for: .seconds(2))
    tick += 1
    let stopped = callRec.isStopped || localRec.isStopped
    if !live || stopped {
        if tick % 5 == 0 || stopped { live = try await startStreams(live ? "capture stopped" : "waiting for a display") }
        continue
    }
    if callRec.micAge > 30 {
        live = try await startStreams("no mic audio for 30s")
        continue
    }
    let silence = callRec.micSilence
    if silence < 30 {
        retriedSilence = false
        try? FileManager.default.removeItem(at: silentFlag)
    } else if !retriedSilence {
        retriedSilence = true
        live = try await startStreams("mic delivered only silence for 30s")
        continue
    } else if silence > 60 {
        let device = AVCaptureDevice.default(for: .audio)?.localizedName ?? "the input device"
        FileManager.default.createFile(atPath: silentFlag.path, contents: Data(device.utf8))
    }
    if tick % 5 == 0, let nf = try? await filters() {
        try? await callStream?.updateContentFilter(nf.call)
        try? await localStream?.updateContentFilter(nf.local)
    }
}
