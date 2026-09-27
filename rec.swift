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

    func stream(_ s: SCStream, didStopWithError error: Error) {
        FileHandle.standardError.write("stream stopped: \(error)\n".data(using: .utf8)!)
        exit(1)
    }
}

func filters() async throws -> (call: SCContentFilter, local: SCContentFilter) {
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    guard let display = content.displays.first else { fatalError("no display") }
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
var f = try await filters()
var callStream = SCStream(filter: f.call, configuration: config(mic: true), delegate: callRec)
let localStream = SCStream(filter: f.local, configuration: config(mic: false), delegate: localRec)
let q = DispatchQueue(label: "audio")
try callStream.addStreamOutput(callRec, type: .audio, sampleHandlerQueue: q)
try callStream.addStreamOutput(callRec, type: .microphone, sampleHandlerQueue: q)
try localStream.addStreamOutput(localRec, type: .audio, sampleHandlerQueue: q)
try await callStream.startCapture()
try await localStream.startCapture()
print("recording to \(outDir.path)"); fflush(stdout)

signal(SIGINT, SIG_IGN); signal(SIGTERM, SIG_IGN)
for sig in [SIGINT, SIGTERM] {
    let src = DispatchSource.makeSignalSource(signal: sig, queue: .main)
    src.setEventHandler { callRec.finishAll(); localRec.finishAll(); exit(0) }
    src.resume()
    _ = Unmanaged.passRetained(src)
}
// If the mic stream stops delivering (its device disappeared, e.g. AirPods/iPhone mic, or capture was
// interrupted), rebuild it on the current default input. A live mic delivers silence too, so a gap = dead.
@MainActor func restartMic(_ why: String) async {
    print("restarting mic stream: \(why)"); fflush(stdout)
    try? await callStream.stopCapture()
    callRec.finishAll()
    guard let nf = try? await filters() else { return }
    let s = SCStream(filter: nf.call, configuration: config(mic: true), delegate: callRec)
    try? s.addStreamOutput(callRec, type: .audio, sampleHandlerQueue: q)
    try? s.addStreamOutput(callRec, type: .microphone, sampleHandlerQueue: q)
    if (try? await s.startCapture()) != nil { callStream = s }
    callRec.resetMic()
}

// Meeting apps opened after start must join the call filter, so refresh the filters periodically.
// A mic that delivers only zeros gets one restart per silent spell; if that doesn't bring it back,
// the mic-silent flag tells the menu bar (via ./ozen.sh health) to ask the user to check the input device.
let silentFlag = URL(fileURLWithPath: "mic-silent")
var retriedSilence = false
var tick = 0
while true {
    try await Task.sleep(for: .seconds(2))
    if callRec.micAge > 30 {
        await restartMic("no mic audio for 30s")
    }
    let silence = callRec.micSilence
    if silence < 30 {
        retriedSilence = false
        try? FileManager.default.removeItem(at: silentFlag)
    } else if !retriedSilence {
        retriedSilence = true
        await restartMic("mic delivered only silence for 30s")
    } else if silence > 60 {
        let device = AVCaptureDevice.default(for: .audio)?.localizedName ?? "the input device"
        FileManager.default.createFile(atPath: silentFlag.path, contents: Data(device.utf8))
    }
    tick += 1
    if tick % 5 == 0, let nf = try? await filters() {
        try? await callStream.updateContentFilter(nf.call)
        try? await localStream.updateContentFilter(nf.local)
    }
}
