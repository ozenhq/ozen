// Menu bar ear icon: left-click shows the live transcript with start/pause/stop controls,
// right-click offers the same controls plus Quit. Controls call the ozen CLI (src/main.rs).
// Click a speaker name in the transcript to tag who really said that line; every tag retrains
// the voiceprints (train.py), so labels improve the more you tag. Tag a voice "Ignored" (a video
// playing nearby) and ozen stops transcribing it; ignored lines show dimmed and leave the timeline.
// Record mode: Always, or Meetings (auto start/stop while a meeting app is using the microphone).
// Built into ~/Applications/Ozen.app by `ozen app`. Direct use: Ozen [dir] [--open]
import AppKit
import CoreAudio

let args = CommandLine.arguments.dropFirst().filter { !$0.hasPrefix("--") }
// Launched as Ozen.app (Finder/Spotlight) there are no args: use the standard checkout.
let dir = URL(fileURLWithPath: args.first ?? NSString(string: "~/ozen").expandingTildeInPath)
let maxLines = 400
let ignoreTag = "Ignored"  // reserved tag, same as train.py / transcribe.py

func json(_ name: String) -> Any? {
    (try? Data(contentsOf: dir.appendingPathComponent(name))).flatMap { try? JSONSerialization.jsonObject(with: $0) }
}

// Meeting apps by bundle id prefix. Chrome covers Google Meet; FaceTime calls capture via avconferenced.
let meetingApps = [("us.zoom", "Zoom"), ("com.google.Chrome", "Chrome"), ("com.microsoft.teams", "Teams"),
                   ("com.tinyspeck.slackmacgap", "Slack"), ("com.apple.FaceTime", "FaceTime"),
                   ("com.apple.avconferenced", "FaceTime"), ("com.hnc.Discord", "Discord")]
let meetingGrace: TimeInterval = 20  // mic can drop briefly (mute toggles, reconnects) without ending the meeting

/// Name of a meeting app currently capturing the microphone, via Core Audio's per-process objects (macOS 14.2+).
/// ozen's own capture shows up as com.apple.replayd, so it never counts as a meeting.
func meetingUsingMic() -> String? {
    func address(_ sel: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
        AudioObjectPropertyAddress(mSelector: sel, mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    }
    func capturing(_ obj: AudioObjectID) -> Bool {
        var addr = address(kAudioProcessPropertyIsRunningInput)
        var running: UInt32 = 0
        var size = UInt32(MemoryLayout<UInt32>.size)
        return AudioObjectGetPropertyData(obj, &addr, 0, nil, &size, &running) == noErr && running != 0
    }
    func bundleID(_ obj: AudioObjectID) -> String {
        var addr = address(kAudioProcessPropertyBundleID)
        var ref: Unmanaged<CFString>?
        var size = UInt32(MemoryLayout<Unmanaged<CFString>?>.size)
        guard AudioObjectGetPropertyData(obj, &addr, 0, nil, &size, &ref) == noErr else { return "" }
        return ref?.takeRetainedValue() as String? ?? ""  // Core Audio returns a +1 CFString: release it
    }
    var addr = AudioObjectPropertyAddress(mSelector: kAudioHardwarePropertyProcessObjectList,
                                          mScope: kAudioObjectPropertyScopeGlobal, mElement: kAudioObjectPropertyElementMain)
    var size: UInt32 = 0
    let system = AudioObjectID(kAudioObjectSystemObject)
    guard AudioObjectGetPropertyDataSize(system, &addr, 0, nil, &size) == noErr else { return nil }
    var ids = [AudioObjectID](repeating: 0, count: Int(size) / MemoryLayout<AudioObjectID>.size)
    guard AudioObjectGetPropertyData(system, &addr, 0, nil, &size, &ids) == noErr else { return nil }
    for id in ids where capturing(id) {
        let bundle = bundleID(id)
        if let app = meetingApps.first(where: { bundle.hasPrefix($0.0) }) { return app.1 }
    }
    return nil
}

struct Line { let id: String, time: String, t: Double, d: Double, spk: String, src: String, text: String }

// MARK: timeline

struct Segment { let id: String, t: Double, d: Double, speaker: String, text: String, unsure: Bool }

/// One lane per speaker, a bar for each line they spoke, on a horizontally scrollable time axis.
/// Silences longer than `gapCap` are squeezed to a short break marker so a day of meetings stays scrollable.
final class TimelineView: NSView, NSViewToolTipOwner {
    var segments: [Segment] = [] { didSet { layoutTimeline() } }
    var pxPerSec: CGFloat = 4 { didSet { layoutTimeline() } }
    var onSelect: ((String) -> Void)?
    private var lanes: [String] = []
    private var bars: [(rect: CGRect, seg: Segment)] = []
    private var spans: [(t0: Double, t1: Double, x0: CGFloat)] = []  // continuous stretches between breaks
    private var breaks: [(x: CGFloat, gap: Double)] = []
    let gutter: CGFloat = 112, laneH: CGFloat = 28, axisH: CGFloat = 22, gapCap: Double = 120, breakW: CGFloat = 36
    override var isFlipped: Bool { true }

    static func color(_ name: String) -> NSColor {
        if name == "?" { return .tertiaryLabelColor }
        let palette: [NSColor] = [.systemBlue, .systemGreen, .systemPurple, .systemPink, .systemTeal,
                                  .systemIndigo, .systemBrown, .systemMint, .systemCyan, .systemYellow]
        let h = name.unicodeScalars.reduce(UInt32(5381)) { ($0 &* 33) &+ $1.value }  // stable per name
        return palette[Int(h % UInt32(palette.count))]
    }

    func layoutTimeline() {
        var talk: [String: Double] = [:]
        for s in segments { talk[s.speaker, default: 0] += s.d }
        lanes = talk.sorted { $0.value > $1.value }.map(\.key)
        bars = []; spans = []; breaks = []
        var x = gutter + 12, cursor: Double? = nil
        for s in segments.sorted(by: { $0.t < $1.t }) {
            if let c = cursor, s.t - c > gapCap {  // long silence: fixed-width break instead of real time
                spans[spans.count - 1].t1 = c
                breaks.append((x + 4, s.t - c))
                x += breakW
                cursor = nil
            }
            if cursor == nil { spans.append((s.t, s.t, x)); cursor = s.t }
            if s.t > cursor! { x += CGFloat(s.t - cursor!) * pxPerSec; cursor = s.t }
            let start = x - CGFloat(cursor! - s.t) * pxPerSec  // overlapping speech starts before the cursor
            let lane = CGFloat(lanes.firstIndex(of: s.speaker) ?? 0)
            let rect = CGRect(x: start, y: axisH + lane * laneH + 5, width: max(3, CGFloat(s.d) * pxPerSec), height: laneH - 10)
            bars.append((rect, s))
            if s.t + s.d > cursor! { x += CGFloat(s.t + s.d - cursor!) * pxPerSec; cursor = s.t + s.d }
            spans[spans.count - 1].t1 = cursor!
        }
        setFrameSize(NSSize(width: max(x + 60, superview?.bounds.width ?? 0),
                            height: max(axisH + CGFloat(lanes.count) * laneH + 8, superview?.bounds.height ?? 0)))
        removeAllToolTips()
        for b in bars { addToolTip(b.rect, owner: self, userData: nil) }
        needsDisplay = true
    }

    override func draw(_ dirty: NSRect) {
        NSColor.textBackgroundColor.setFill()
        dirty.fill()
        let small: [NSAttributedString.Key: Any] = [.font: NSFont.monospacedDigitSystemFont(ofSize: 10, weight: .regular),
                                                    .foregroundColor: NSColor.secondaryLabelColor]
        for (i, _) in lanes.enumerated() where i % 2 == 1 {  // zebra lanes
            NSColor.quaternaryLabelColor.withAlphaComponent(0.08).setFill()
            NSRect(x: dirty.minX, y: axisH + CGFloat(i) * laneH, width: dirty.width, height: laneH).fill()
        }
        let fmt = DateFormatter()
        fmt.dateFormat = "HH:mm"
        let step: Double = pxPerSec >= 8 ? 30 : pxPerSec >= 2 ? 60 : pxPerSec >= 0.5 ? 300 : 900  // tick spacing, s
        for sp in spans {
            var m = (sp.t0 / step).rounded(.up) * step
            while m <= sp.t1 {
                let tx = sp.x0 + CGFloat(m - sp.t0) * pxPerSec
                NSColor.separatorColor.setFill()
                NSRect(x: tx, y: axisH - 6, width: 1, height: bounds.height).fill()
                (fmt.string(from: Date(timeIntervalSince1970: m)) as NSString).draw(at: NSPoint(x: tx + 3, y: 4), withAttributes: small)
                m += step
            }
        }
        for b in breaks {  // squeezed silence
            let label = b.gap >= 3600 ? "\(Int(b.gap / 3600))h" : "\(Int(b.gap / 60))m"
            NSColor.separatorColor.setFill()
            NSRect(x: b.x + breakW / 2 - 3, y: axisH, width: 1, height: bounds.height).fill()
            NSRect(x: b.x + breakW / 2 + 2, y: axisH, width: 1, height: bounds.height).fill()
            ("⋯" + label as NSString).draw(at: NSPoint(x: b.x, y: 4), withAttributes: small)
        }
        for b in bars where b.rect.intersects(dirty) {
            let path = NSBezierPath(roundedRect: b.rect, xRadius: 3, yRadius: 3)
            TimelineView.color(b.seg.speaker).withAlphaComponent(b.seg.unsure ? 0.45 : 0.85).setFill()
            path.fill()
            if b.seg.unsure {
                NSColor.systemOrange.setStroke()
                path.lineWidth = 1.5
                path.stroke()
            }
        }
        // Speaker names stay pinned to the left edge while scrolling horizontally.
        let g = NSRect(x: visibleRect.minX, y: 0, width: gutter, height: bounds.height)
        NSColor.windowBackgroundColor.setFill()
        g.fill()
        NSColor.separatorColor.setFill()
        NSRect(x: g.maxX - 1, y: 0, width: 1, height: bounds.height).fill()
        for (i, name) in lanes.enumerated() {
            let y = axisH + CGFloat(i) * laneH
            TimelineView.color(name).setFill()
            NSBezierPath(ovalIn: NSRect(x: g.minX + 8, y: y + laneH / 2 - 4, width: 8, height: 8)).fill()
            let total = segments.filter { $0.speaker == name }.reduce(0) { $0 + $1.d }
            let label = NSMutableAttributedString(string: name, attributes: [.font: NSFont.boldSystemFont(ofSize: 11),
                                                                             .foregroundColor: NSColor.labelColor])
            label.append(NSAttributedString(string: " \(Int(total / 60))m\(Int(total) % 60)s", attributes: small))
            label.draw(with: NSRect(x: g.minX + 20, y: y + 6, width: gutter - 24, height: laneH - 8),
                       options: [.usesLineFragmentOrigin, .truncatesLastVisibleLine])
        }
    }

    func segment(at p: NSPoint) -> Segment? { bars.first { $0.rect.insetBy(dx: -2, dy: -2).contains(p) }?.seg }

    override func mouseDown(with event: NSEvent) {
        if let s = segment(at: convert(event.locationInWindow, from: nil)) { onSelect?(s.id) }
    }

    func view(_ view: NSView, stringForToolTip tag: NSView.ToolTipTag, point: NSPoint, userData: UnsafeMutableRawPointer?) -> String {
        guard let s = segment(at: point) else { return "" }
        let f = DateFormatter()
        f.dateFormat = "HH:mm:ss"
        return "\(f.string(from: Date(timeIntervalSince1970: s.t)))  \(s.speaker)\(s.unsure ? " ?" : "")  (\(Int(s.d.rounded()))s)\n\(s.text)"
    }
}

final class App: NSObject, NSApplicationDelegate, NSTextViewDelegate {
    let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    let popover = NSPopover()
    let scroll = NSTextView.scrollableTextView()
    let footer = NSTextField(labelWithString: "")
    var text: NSTextView { scroll.documentView as! NSTextView }
    var signature = ""
    var pending: [String: String] = [:]  // tags shown right away while train.py runs
    var state = "stopped"  // from `ozen status`: recording | paused | stopping | stopped
    var problems: [String] = []  // from `ozen health`: why recording isn't turning into transcript
    let warning = NSTextField(wrappingLabelWithString: "")
    let status = NSTextField(labelWithString: "")
    let startButton = NSButton(title: "Start", target: nil, action: nil)
    let pauseButton = NSButton(title: "Pause", target: nil, action: nil)
    let stopButton = NSButton(title: "Stop", target: nil, action: nil)
    let reviewButton = NSButton(title: "Review", target: nil, action: nil)
    var headerRanges: [String: NSRange] = [:]  // line id -> speaker name range in the text view
    var review: [String] = []  // line ids train.py is least sure about, most uncertain first
    var shown: [String: (spk: String, t: Double)] = [:]  // line id -> speaker as shown in the transcript
    let modeControl = NSSegmentedControl(labels: ["Always", "Meetings"], trackingMode: .selectOne, target: nil, action: nil)
    var mode: String { UserDefaults.standard.string(forKey: "mode") ?? "always" }  // "always" | "meetings"
    var lastMeeting: Date?, meetingName: String?
    var lastWanted: Bool?  // act only when "should be recording" flips, so manual Pause/Stop stick until then
    let timeline = TimelineView()
    let timelineScroll = NSScrollView()
    let viewControl = NSSegmentedControl(labels: ["Transcript", "Timeline"], trackingMode: .selectOne, target: nil, action: nil)
    let zoomOut = NSButton(title: "−", target: nil, action: nil)
    let zoomIn = NSButton(title: "+", target: nil, action: nil)

    func applicationDidFinishLaunching(_ n: Notification) {
        let button = item.button!
        button.image = NSImage(systemSymbolName: "ear", accessibilityDescription: "ozen transcript")
        button.target = self
        button.action = #selector(clicked)
        button.sendAction(on: [.leftMouseUp, .rightMouseUp])

        text.isEditable = false
        text.delegate = self
        text.textContainerInset = NSSize(width: 10, height: 10)
        text.linkTextAttributes = [.foregroundColor: NSColor.secondaryLabelColor, .cursor: NSCursor.pointingHand]
        footer.font = .systemFont(ofSize: 11)
        footer.textColor = .secondaryLabelColor
        for (b, cmd) in [(startButton, #selector(startCapture)), (pauseButton, #selector(pauseCapture)), (stopButton, #selector(stopCapture)), (reviewButton, #selector(reviewNext))] {
            b.target = self
            b.action = cmd
            b.bezelStyle = .rounded
            b.controlSize = .small
        }
        status.font = .boldSystemFont(ofSize: 12)
        warning.font = .systemFont(ofSize: 12)
        warning.textColor = .systemOrange
        modeControl.target = self
        modeControl.action = #selector(modeChanged)
        modeControl.controlSize = .small
        modeControl.selectedSegment = mode == "meetings" ? 1 : 0
        modeControl.toolTip = "Always: record until you stop. Meetings: start and stop automatically with Zoom/Meet/Teams/Slack/FaceTime calls."
        let controls = NSStackView(views: [status, NSView(), modeControl, reviewButton, startButton, pauseButton, stopButton])
        controls.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 0, right: 12)
        viewControl.target = self
        viewControl.action = #selector(switchView)
        viewControl.controlSize = .small
        viewControl.selectedSegment = UserDefaults.standard.integer(forKey: "view")
        for (b, sel) in [(zoomOut, #selector(zoom(_:))), (zoomIn, #selector(zoom(_:)))] {
            b.target = self
            b.action = sel
            b.bezelStyle = .rounded
            b.controlSize = .small
        }
        timeline.pxPerSec = CGFloat(UserDefaults.standard.object(forKey: "pxPerSec") as? Double ?? 4)
        timeline.onSelect = { [weak self] id in self?.jump(to: id) }
        timelineScroll.documentView = timeline
        timelineScroll.hasHorizontalScroller = true
        timelineScroll.hasVerticalScroller = true
        timelineScroll.autohidesScrollers = true
        timelineScroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(forName: NSView.boundsDidChangeNotification, object: timelineScroll.contentView,
                                               queue: .main) { [weak self] _ in self?.timeline.needsDisplay = true }  // repin names
        let viewRow = NSStackView(views: [viewControl, NSView(), zoomOut, zoomIn])
        viewRow.edgeInsets = NSEdgeInsets(top: 0, left: 12, bottom: 0, right: 12)
        let warningRow = NSStackView(views: [warning])
        warningRow.edgeInsets = NSEdgeInsets(top: 0, left: 12, bottom: 0, right: 12)
        let stack = NSStackView(views: [controls, warningRow, viewRow, scroll, timelineScroll, footer])
        stack.orientation = .vertical
        stack.edgeInsets = NSEdgeInsets(top: 0, left: 0, bottom: 8, right: 0)
        stack.frame = NSRect(x: 0, y: 0, width: 640, height: 680)
        applyView()
        let vc = NSViewController()
        vc.view = stack
        popover.contentViewController = vc
        popover.behavior = .transient

        Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in self?.reload(); self?.refreshState() }
        refreshState()
        if CommandLine.arguments.contains("--open") { DispatchQueue.main.asyncAfter(deadline: .now() + 1) { self.clicked() } }
    }

    @objc func clicked() {
        if NSApp.currentEvent?.type == .rightMouseUp {
            let menu = NSMenu()
            menu.addItem(withTitle: "ozen: \(state)", action: nil, keyEquivalent: "")
            for p in problems { menu.addItem(withTitle: "⚠︎ " + p, action: nil, keyEquivalent: "") }
            menu.addItem(.separator())
            for (title, sel, on) in [(state == "paused" ? "Resume" : "Start", #selector(startCapture), state == "stopped" || state == "paused"),
                                     ("Pause", #selector(pauseCapture), state == "recording"),
                                     ("Stop", #selector(stopCapture), state == "recording" || state == "paused")] where on {
                let mi = NSMenuItem(title: title, action: sel, keyEquivalent: "")
                mi.target = self
                menu.addItem(mi)
            }
            menu.addItem(.separator())
            for (title, m) in [("Record always", "always"), ("Record only meetings", "meetings")] {
                let mi = NSMenuItem(title: title, action: #selector(modeChanged(_:)), keyEquivalent: "")
                mi.target = self
                mi.representedObject = m
                mi.state = mode == m ? .on : .off
                menu.addItem(mi)
            }
            menu.addItem(.separator())
            menu.addItem(withTitle: "Quit ozen bar", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
            item.menu = menu
            item.button?.performClick(nil)  // shows the menu
            item.menu = nil  // keep left-click for the popover
            return
        }
        if popover.isShown {
            popover.performClose(nil)
        } else {
            signature = ""
            popover.show(relativeTo: item.button!.bounds, of: item.button!, preferredEdge: .minY)
            NSApp.activate()
            reload()
            text.scrollToEndOfDocument(nil)
        }
    }

    // MARK: capture control (the ozen CLI owns the processes; this only asks it)

    func ozen(_ cmd: String, done: ((String) -> Void)? = nil) {
        DispatchQueue.global().async {
            let p = Process()
            let pipe = Pipe()
            p.executableURL = dir.appendingPathComponent("target/release/ozen")
            p.arguments = [cmd]
            p.standardOutput = pipe
            try? p.run()
            p.waitUntilExit()
            let out = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            DispatchQueue.main.async { done?(out.trimmingCharacters(in: .whitespacesAndNewlines)) }
        }
    }

    func refreshState() {
        ozen("status") { self.show(state: $0); self.autoControl() }
        ozen("health") {
            self.problems = $0.split(separator: "\n").map(String.init)
            self.show(state: self.state)
        }
    }

    // MARK: record mode

    @objc func modeChanged(_ sender: Any?) {
        let m = (sender as? NSMenuItem)?.representedObject as? String ?? (modeControl.selectedSegment == 1 ? "meetings" : "always")
        UserDefaults.standard.set(m, forKey: "mode")
        modeControl.selectedSegment = m == "meetings" ? 1 : 0
        lastWanted = nil  // apply the new mode right away
        autoControl()
    }

    func autoControl() {
        if let app = meetingUsingMic() {
            lastMeeting = Date()
            meetingName = app
        }
        let inMeeting = lastMeeting.map { Date().timeIntervalSince($0) < meetingGrace } ?? false
        let wanted = mode == "always" || inMeeting
        defer { lastWanted = wanted; show(state: state) }
        guard wanted != lastWanted else { return }
        if wanted, state == "stopped" || state == "paused" {
            startCapture()
        } else if !wanted, state == "recording" || state == "paused" {
            stopCapture()
        }
    }

    func show(state s: String) {
        state = s
        let icon = s == "recording" && !problems.isEmpty ? "ear.trianglebadge.exclamationmark"
            : ["recording": "ear.fill", "paused": "pause.circle", "stopping": "hourglass"][s] ?? "ear"
        warning.stringValue = problems.map { "⚠︎ " + $0 }.joined(separator: "\n")
        warning.superview?.isHidden = problems.isEmpty
        item.button?.image = NSImage(systemSymbolName: icon, accessibilityDescription: "ozen \(s)")
        let inMeeting = lastMeeting.map { Date().timeIntervalSince($0) < meetingGrace } ?? false
        let meeting = mode == "meetings" && inMeeting ? " · \(meetingName ?? "meeting")" : ""
        status.stringValue = ["recording": "● Recording\(meeting)", "paused": "Paused", "stopping": "Finishing transcription…"][s]
            ?? (mode == "meetings" ? "Waiting for a meeting" : "Stopped")
        status.textColor = s == "recording" ? .systemRed : .secondaryLabelColor
        startButton.title = s == "paused" ? "Resume" : "Start"
        startButton.isEnabled = s == "stopped" || s == "paused"
        pauseButton.isEnabled = s == "recording"
        stopButton.isEnabled = s == "recording" || s == "paused"
    }

    func control(_ cmd: String, optimistic: String) {
        show(state: optimistic)
        ozen(cmd) { _ in DispatchQueue.main.asyncAfter(deadline: .now() + 1) { self.refreshState() } }
    }

    @objc func startCapture() { control(state == "paused" ? "resume" : "start", optimistic: "recording") }
    @objc func pauseCapture() { control("pause", optimistic: "paused") }
    @objc func stopCapture() { control("stop", optimistic: "stopping") }

    // MARK: transcript / timeline switch

    @objc func switchView() {
        UserDefaults.standard.set(viewControl.selectedSegment, forKey: "view")
        applyView()
    }

    func applyView() {
        let showTimeline = viewControl.selectedSegment == 1
        scroll.isHidden = showTimeline
        timelineScroll.isHidden = !showTimeline
        zoomIn.isHidden = !showTimeline
        zoomOut.isHidden = !showTimeline
        if showTimeline { scrollTimelineToEnd() }
    }

    @objc func zoom(_ sender: NSButton) {
        let anchor = timelineScroll.contentView.bounds.midX / max(timeline.bounds.width, 1)  // keep the view centered
        timeline.pxPerSec = min(32, max(0.25, timeline.pxPerSec * (sender == zoomIn ? 2 : 0.5)))
        UserDefaults.standard.set(Double(timeline.pxPerSec), forKey: "pxPerSec")
        let w = timelineScroll.contentView.bounds.width
        timelineScroll.contentView.scroll(to: NSPoint(x: max(0, anchor * timeline.bounds.width - w / 2), y: 0))
        timelineScroll.reflectScrolledClipView(timelineScroll.contentView)
    }

    func scrollTimelineToEnd() {
        let x = max(0, timeline.bounds.width - timelineScroll.contentView.bounds.width)
        timelineScroll.contentView.scroll(to: NSPoint(x: x, y: 0))
        timelineScroll.reflectScrolledClipView(timelineScroll.contentView)
    }

    /// Timeline bar clicked: show that line in the transcript.
    func jump(to id: String) {
        viewControl.selectedSegment = 0
        switchView()
        guard let range = headerRanges[id] else { return }
        text.scrollRangeToVisible(range)
        text.showFindIndicator(for: range)
    }

    func lines(limit: Int) -> [Line] {
        guard let raw = try? String(contentsOf: dir.appendingPathComponent("lines.jsonl"), encoding: .utf8) else { return [] }
        let fmt = DateFormatter()
        fmt.dateFormat = "HH:mm:ss"
        // Call and mic chunks finish transcribing at different times, so file order isn't time order.
        return raw.split(separator: "\n").suffix(limit).compactMap { row -> (Double, Line)? in
            guard let r = try? JSONSerialization.jsonObject(with: Data(row.utf8)) as? [String: Any],
                  let id = r["id"] as? String, let t = r["t"] as? Double else { return nil }
            let text = r["text"] as? String ?? ""
            let d = r["d"] as? Double ?? min(15, max(1, Double(text.count) / 14))  // older lines: estimate from length
            return (t, Line(id: id, time: fmt.string(from: Date(timeIntervalSince1970: t)), t: t, d: d, spk: r["spk"] as? String ?? "?",
                            src: r["src"] as? String ?? "", text: r["text"] as? String ?? ""))
        }.sorted { $0.0 < $1.0 }.map(\.1)
    }

    func reload() {
        guard popover.isShown else { return }
        let files = ["lines.jsonl", "tags.json", "labels.json", "stats.json"]
        let sig = files.map { f -> String in
            let a = try? FileManager.default.attributesOfItem(atPath: dir.appendingPathComponent(f).path)
            return "\(a?[.size] ?? 0)-\((a?[.modificationDate] as? Date)?.timeIntervalSince1970 ?? 0)"
        }.joined(separator: "|") + "\(pending)"
        guard sig != signature else { return }
        signature = sig
        let atBottom = scroll.verticalScroller.map { $0.floatValue > 0.98 } ?? true

        let tags = (json("tags.json") as? [String: String] ?? [:]).merging(pending) { $1 }
        let labels = json("labels.json") as? [String: [String: Any]] ?? [:]
        let out = NSMutableAttributedString()
        headerRanges = [:]
        let history = lines(limit: 5000)  // timeline spans more than the transcript shows
        let all = Array(history.suffix(maxLines))
        let atEnd = timelineScroll.contentView.bounds.maxX >= timeline.bounds.width - 20
        timeline.segments = history.compactMap { l in
            let tagged = !(tags[l.id] ?? "").isEmpty
            let guess = labels[l.id]
            let speaker = tagged ? tags[l.id]! : ((guess?["spk"] as? String) ?? l.spk)
            return speaker == ignoreTag ? nil : Segment(id: l.id, t: l.t, d: l.d, speaker: speaker,
                                                        text: l.text, unsure: !tagged && (guess?["unsure"] as? Bool ?? false))
        }
        shown = [:]
        if atEnd { scrollTimelineToEnd() }
        if all.isEmpty { out.append(NSAttributedString(string: "No transcript yet. Press Start.", attributes: [.foregroundColor: NSColor.secondaryLabelColor])) }
        for l in all {
            let tagged = !(tags[l.id] ?? "").isEmpty
            let guess = labels[l.id]
            let unsure = !tagged && (guess?["unsure"] as? Bool ?? false)
            let speaker = tagged ? tags[l.id]! : ((guess?["spk"] as? String) ?? l.spk)
            shown[l.id] = (speaker, l.t)
            let ignored = speaker == ignoreTag
            let para = NSMutableParagraphStyle()
            para.paragraphSpacing = 6
            if l.text.unicodeScalars.contains(where: { (0x0590...0x05FF).contains($0.value) }) {
                para.baseWritingDirection = .rightToLeft
                para.alignment = .right
            }
            let base: [NSAttributedString.Key: Any] = [.paragraphStyle: para, .foregroundColor: NSColor.labelColor]
            out.append(NSAttributedString(string: "[\(l.time)] ", attributes: base.merging([
                .font: NSFont.monospacedDigitSystemFont(ofSize: 11, weight: .regular), .foregroundColor: NSColor.tertiaryLabelColor,
            ]) { $1 }))
            let header = speaker + (tagged ? " ✓" : unsure ? " ?" : "")
            headerRanges[l.id] = NSRange(location: out.length, length: (header as NSString).length)
            out.append(NSAttributedString(string: header, attributes: base.merging([
                .font: NSFont.boldSystemFont(ofSize: 12), .link: URL(string: "ozen://tag/\(l.id)")!,
                // unsure lines are what the loop wants tagged next
                .foregroundColor: unsure ? NSColor.systemOrange : NSColor.secondaryLabelColor,
            ]) { $1 }))
            out.append(NSAttributedString(string: " (\(l.src)): ", attributes: base.merging([
                .font: NSFont.systemFont(ofSize: 11), .foregroundColor: NSColor.tertiaryLabelColor,
            ]) { $1 }))
            out.append(NSAttributedString(string: l.text + "\n", attributes: base.merging([
                .font: NSFont.systemFont(ofSize: ignored ? 11 : 13), .foregroundColor: ignored ? NSColor.tertiaryLabelColor : NSColor.labelColor,
            ]) { $1 }))
        }
        text.textStorage?.setAttributedString(out)
        if atBottom { text.scrollToEndOfDocument(nil) }

        let s = json("stats.json") as? [String: Any] ?? [:]
        let tagged = s["tagged"] as? Int ?? 0
        let pct = { (x: Double) in "\(Int((x * 100).rounded()))%" }
        var acc = (s["accuracy"] as? Double).map { "accuracy \(pct($0)) on \(s["evaluated"] ?? 0) checks" } ?? "accuracy after 2 tags of one person"
        if let first = s["accuracy_first"] as? Double, let now = s["accuracy"] as? Double, first != now {
            acc += " (was \(pct(first)))"
        }
        review = (s["review"] as? [String] ?? []).filter { (tags[$0] ?? "").isEmpty && headerRanges[$0] != nil }
        reviewButton.title = review.isEmpty ? "Review" : "Review \(review.count)"
        reviewButton.isEnabled = !review.isEmpty
        let ignoredLines = (s["ignored"] as? Int ?? 0) > 0 ? " · \(s["ignored"]!) ignored" : ""
        footer.stringValue = "  \(acc) · \(tagged) tagged\(ignoredLines) · orange ? = unsure, tag it to teach ozen"
    }

    // Jump to the line ozen is least sure about and ask who said it.
    @objc func reviewNext() {
        guard let id = review.first, let range = headerRanges[id],
              let lm = text.layoutManager, let tc = text.textContainer else { return }
        text.scrollRangeToVisible(range)
        text.showFindIndicator(for: range)
        let glyphs = lm.glyphRange(forCharacterRange: range, actualCharacterRange: nil)
        var rect = lm.boundingRect(forGlyphRange: glyphs, in: tc)
        rect.origin.x += text.textContainerOrigin.x
        rect.origin.y += text.textContainerOrigin.y + rect.height
        tagMenu(for: id).popUp(positioning: nil, at: rect.origin, in: text)
    }

    // Clicking a speaker name: pick who really said the line.
    func textView(_ view: NSTextView, clickedOnLink link: Any, at index: Int) -> Bool {
        guard let url = link as? URL, url.host == "tag" else { return false }
        if let event = NSApp.currentEvent { NSMenu.popUpContextMenu(tagMenu(for: url.lastPathComponent), with: event, for: view) }
        return true
    }

    func tagMenu(for id: String) -> NSMenu {
        let menu = NSMenu(title: "Who said this?")
        for name in knownNames() {
            let mi = NSMenuItem(title: name, action: #selector(pick(_:)), keyEquivalent: "")
            mi.target = self
            mi.representedObject = [id, name]
            menu.addItem(mi)
        }
        if !menu.items.isEmpty { menu.addItem(.separator()) }
        let new = NSMenuItem(title: "New person…", action: #selector(newPerson(_:)), keyEquivalent: "")
        new.target = self
        new.representedObject = id
        menu.addItem(new)
        // Not a person (a video playing nearby): ozen stops transcribing voices like this one.
        let ignore = NSMenuItem(title: "Ignore this voice", action: #selector(pick(_:)), keyEquivalent: "")
        ignore.target = self
        ignore.representedObject = [id, ignoreTag]
        menu.addItem(ignore)
        // Only session labels: a named person (you) is never one click from being ignored.
        if let (spk, t) = shown[id], spk.range(of: "^S[0-9]+$", options: .regularExpression) != nil {
            // S1, S2... restart with the transcriber, so only lines near this one are the same voice.
            let same = shown.filter { $0.value.spk == spk && abs($0.value.t - t) < 3600 }.map(\.key)
            let all = NSMenuItem(title: "Ignore all \(same.count) lines by \(spk)", action: #selector(ignoreAll(_:)), keyEquivalent: "")
            all.target = self
            all.representedObject = same
            menu.addItem(all)
        }
        menu.addItem(.separator())
        let clear = NSMenuItem(title: "Clear tag", action: #selector(pick(_:)), keyEquivalent: "")
        clear.target = self
        clear.representedObject = [id, ""]
        menu.addItem(clear)
        return menu
    }

    func knownNames() -> [String] {
        let registry = (try? FileManager.default.contentsOfDirectory(at: dir.appendingPathComponent("voices/voices"),
                                                                     includingPropertiesForKeys: nil)) ?? []
        let fromRegistry = registry.compactMap { f in
            (try? Data(contentsOf: f)).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }?["name"] as? String
        }
        let fromTags = (json("tags.json") as? [String: String] ?? [:]).values.filter { !$0.isEmpty && $0 != ignoreTag }
        return Array(Set(fromRegistry + fromTags)).sorted()
    }

    @objc func newPerson(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        let alert = NSAlert()
        alert.messageText = "Who said this line?"
        alert.addButton(withTitle: "Tag")
        alert.addButton(withTitle: "Cancel")
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 24))
        field.placeholderString = "Full name"
        alert.accessoryView = field
        alert.window.initialFirstResponder = field
        NSApp.activate()
        if alert.runModal() == .alertFirstButtonReturn, !field.stringValue.trimmingCharacters(in: .whitespaces).isEmpty {
            tag(id, field.stringValue.trimmingCharacters(in: .whitespaces))
        }
    }

    @objc func pick(_ sender: NSMenuItem) {
        guard let pair = sender.representedObject as? [String] else { return }
        tag(pair[0], pair[1])
    }

    @objc func ignoreAll(_ sender: NSMenuItem) {
        guard let ids = sender.representedObject as? [String] else { return }
        tag(ids, ignoreTag, command: "cd \"$OZEN_DIR\" && uv run -q train.py ignore ${=OZEN_ID}")
    }

    func tag(_ id: String, _ name: String) {
        tag([id], name, command: "cd \"$OZEN_DIR\" && uv run -q train.py tag \"$OZEN_ID\" \"$OZEN_NAME\"")
    }

    func tag(_ ids: [String], _ name: String, command: String) {
        for id in ids { pending[id] = name }
        reload()
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/zsh")
        // Values go through the environment, never into the command string. Line ids have no spaces.
        p.arguments = ["-lc", command]
        p.environment = ProcessInfo.processInfo.environment.merging(["OZEN_DIR": dir.path, "OZEN_ID": ids.joined(separator: " "), "OZEN_NAME": name]) { $1 }
        p.terminationHandler = { _ in
            DispatchQueue.main.async {
                for id in ids { self.pending[id] = nil }
                self.signature = ""
                self.reload()
            }
        }
        try? p.run()
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)  // menu bar only, no Dock icon
let delegate = App()
app.delegate = delegate
app.run()
