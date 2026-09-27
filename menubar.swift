// Menu bar ear icon: left-click shows the live transcript with start/pause/stop controls,
// right-click offers the same controls plus Quit. Controls call the ozen CLI (src/main.rs).
// Click a speaker name in the transcript to tag who really said that line; every tag retrains
// the voiceprints (train.py), so labels improve the more you tag. Click a line's text to fix what was
// said; fixes teach the transcriber words and repeated corrections (`ozen fix`, src/fixes.rs).
// Tag a voice "Ignored" (a video playing nearby) and ozen stops transcribing it; ignored lines show
// dimmed and leave the timeline.
// Record mode: Always, or Meetings (auto start/stop while a meeting app is using the microphone).
// Places: labeled locations that override the mode while you're there (auto record, or auto off).
// Meetings view: pick past meetings (⌘/⇧-click for several), optionally let Kev add related ones, and start
// Claude Code or Hermes in a folder holding their transcripts (`ozen gather`).
// Built into ~/Applications/Ozen.app by `ozen app`. Direct use: Ozen [dir] [--open]
import AppKit
import CoreAudio
import CoreLocation
import WebKit  // hosts map.html (Leaflet + OpenStreetMap), the same map any OS can show

let args = CommandLine.arguments.dropFirst().filter { !$0.hasPrefix("--") }
// Launched as Ozen.app (Finder/Spotlight) there are no args: use the standard checkout.
let dir = URL(fileURLWithPath: args.first ?? NSString(string: "~/ozen").expandingTildeInPath)
let maxLines = 400
let ignoreTag = "Ignored"  // reserved tag, same as src/ignore.rs / transcribe.py

func json(_ name: String) -> Any? {
    (try? Data(contentsOf: dir.appendingPathComponent(name))).flatMap { try? JSONSerialization.jsonObject(with: $0) }
}

// Meeting apps by bundle id prefix. Chrome covers Google Meet; FaceTime calls capture via avconferenced.
let meetingApps = [("us.zoom", "Zoom"), ("com.google.Chrome", "Chrome"), ("com.microsoft.teams", "Teams"),
                   ("com.tinyspeck.slackmacgap", "Slack"), ("com.apple.FaceTime", "FaceTime"),
                   ("com.apple.avconferenced", "FaceTime"), ("com.hnc.Discord", "Discord")]
let meetingGrace: TimeInterval = 20  // mic can drop briefly (mute toggles, reconnects) without ending the meeting

// A place with no coordinates yet does nothing until you set it to where you are.
struct Place: Codable {
    var label: String
    var lat: Double?
    var lon: Double?
    var action: String  // "record" | "meetings" | "off"
    var radius: Double?  // meters; nil = defaultRadius
}
let defaultRadius: CLLocationDistance = 150
let placeActions = [("record", "Auto record"), ("meetings", "Record meetings only"), ("off", "Auto off")]

// places.json in the ozen dir: plain JSON any platform or tool can read, not macOS-only preferences.
let placesFile = dir.appendingPathComponent("places.json")

func loadPlaces() -> [Place] {
    // Older builds kept places in UserDefaults: read them once, then they move to places.json on the next save.
    guard let d = (try? Data(contentsOf: placesFile)) ?? UserDefaults.standard.data(forKey: "places"),
          let p = try? JSONDecoder().decode([Place].self, from: d) else {
        return [Place(label: "Home", action: "off"), Place(label: "Work", action: "record")]
    }
    return p
}

func savePlaces(_ p: [Place]) {
    let enc = JSONEncoder()
    enc.outputFormatting = [.prettyPrinted, .sortedKeys]
    if let d = try? enc.encode(p), (try? d.write(to: placesFile, options: .atomic)) != nil {
        UserDefaults.standard.removeObject(forKey: "places")  // migrated
    }
}

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

struct Line { let id: String, time: String, t: Double, d: Double, spk: String, src: String, text: String, run: Int?, doubt: Double? }

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

final class App: NSObject, NSApplicationDelegate, NSTextViewDelegate, CLLocationManagerDelegate, WKNavigationDelegate, NSTableViewDataSource,
                 WKScriptMessageHandler {
    let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    let popover = NSPopover()
    let scroll = NSTextView.scrollableTextView()
    let footer = NSTextField(labelWithString: "")
    var text: NSTextView { scroll.documentView as! NSTextView }
    var signature = ""
    var pending: [String: String] = [:]  // tags shown right away while `ozen tag` retrains
    var pendingFixes: [String: String] = [:]  // same for text fixes
    var state = "stopped"  // from `ozen status`: recording | paused | stopping | stopped
    var problems: [String] = []  // from `ozen health`: why recording isn't turning into transcript
    let warning = NSTextField(wrappingLabelWithString: "")
    let status = NSTextField(labelWithString: "")
    let startButton = NSButton(title: "Start", target: nil, action: nil)
    let pauseButton = NSButton(title: "Pause", target: nil, action: nil)
    let stopButton = NSButton(title: "Stop", target: nil, action: nil)
    let reviewButton = NSButton(title: "Review", target: nil, action: nil)
    var headerRanges: [String: NSRange] = [:]  // line id -> speaker name range in the text view
    var reviewQueue: [String] = []  // untagged unsure line ids, most uncertain first
    var review: [String] = []  // the queue minus lines too old to remember who said them
    let reviewMaxAge: Double = 600  // seconds
    let unsureDoubt = 0.08  // train.py UNSURE: this close to the threshold, or to a second person
    var shown: [String: (spk: String, t: Double, run: Int?)] = [:]  // line id -> speaker as shown in the transcript
    let modeControl = NSSegmentedControl(labels: ["Always", "Meetings"], trackingMode: .selectOne, target: nil, action: nil)
    var mode: String { UserDefaults.standard.string(forKey: "mode") ?? "always" }  // "always" | "meetings"
    var lastMeeting: Date?, meetingName: String?
    var lastWanted: Bool?  // act only when "should be recording" flips, so manual Pause/Stop stick until then
    let timeline = TimelineView()
    let timelineScroll = NSScrollView()
    let viewControl = NSSegmentedControl(labels: ["Transcript", "Timeline", "Meetings"], trackingMode: .selectOne, target: nil, action: nil)
    let zoomOut = NSButton(title: "−", target: nil, action: nil)
    let zoomIn = NSButton(title: "+", target: nil, action: nil)
    let placesButton = NSButton(title: "Places…", target: nil, action: nil)
    let location = CLLocationManager()
    var here: CLLocation?
    var placeLabel: String?  // label of the place we're in; a change re-applies auto control
    var settingPlace: Int?  // row waiting for a location fix after "Use current location"
    var pickingPlace: Int?  // row waiting for a map click after "Pick on map"
    var rebuilding = false  // removing a focused field fires its action; ignore those echoes
    var placesWindow: NSWindow?
    let placesStack = NSStackView()
    let placesMap = WKWebView()
    let placesNote = NSTextField(wrappingLabelWithString: "")
    let meetingsTable = NSTableView()
    let meetingsScroll = NSScrollView()
    var meetings: [[String]] = []  // `ozen meetings` rows: id, start, minutes, lines, first words
    let gatherButton = NSButton(title: "Open", target: nil, action: nil)
    let kevButton = NSButton(title: "Auto add with Kev", target: nil, action: nil)

    func applicationDidFinishLaunching(_ n: Notification) {
        let button = item.button!
        button.image = NSImage(systemSymbolName: "ear", accessibilityDescription: "ozen transcript")
        button.target = self
        button.action = #selector(clicked)
        button.sendAction(on: [.leftMouseUp, .rightMouseUp])

        text.isEditable = false
        text.delegate = self
        text.textContainerInset = NSSize(width: 10, height: 10)
        text.linkTextAttributes = [.cursor: NSCursor.pointingHand]  // links keep their own colors: names, unsure, text
        footer.font = .systemFont(ofSize: 11)
        footer.textColor = .secondaryLabelColor
        for (b, cmd) in [(startButton, #selector(startCapture)), (pauseButton, #selector(pauseCapture)), (stopButton, #selector(stopCapture)), (reviewButton, #selector(reviewNext)), (placesButton, #selector(showPlaces))] {
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
        let controls = NSStackView(views: [status, NSView(), modeControl, placesButton, reviewButton, startButton, pauseButton, stopButton])
        controls.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 0, right: 12)
        viewControl.target = self
        viewControl.action = #selector(switchView)
        viewControl.controlSize = .small
        viewControl.selectedSegment = UserDefaults.standard.integer(forKey: "view")
        for (b, sel) in [(zoomOut, #selector(zoom(_:))), (zoomIn, #selector(zoom(_:))), (gatherButton, #selector(gather(_:))), (kevButton, #selector(gather(_:)))] {
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
        gatherButton.toolTip = "Put the selected meetings' transcripts in a folder and start Claude Code or Hermes there"
        kevButton.toolTip = "Same, plus every other meeting Kev (localhost:8009) judges related"
        for (title, width) in [("When", 120), ("Min", 40), ("Lines", 44), ("Starts with", 380)] {
            let col = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(title))
            col.title = title
            col.width = CGFloat(width)
            meetingsTable.addTableColumn(col)
        }
        meetingsTable.allowsMultipleSelection = true
        meetingsTable.usesAlternatingRowBackgroundColors = true
        meetingsTable.dataSource = self
        meetingsTable.doubleAction = #selector(gather(_:))
        meetingsTable.target = self
        meetingsScroll.documentView = meetingsTable
        meetingsScroll.hasVerticalScroller = true
        let viewRow = NSStackView(views: [viewControl, NSView(), zoomOut, zoomIn, kevButton, gatherButton])
        viewRow.edgeInsets = NSEdgeInsets(top: 0, left: 12, bottom: 0, right: 12)
        let warningRow = NSStackView(views: [warning])
        warningRow.edgeInsets = NSEdgeInsets(top: 0, left: 12, bottom: 0, right: 12)
        let stack = NSStackView(views: [controls, warningRow, viewRow, scroll, timelineScroll, meetingsScroll, footer])
        stack.orientation = .vertical
        stack.edgeInsets = NSEdgeInsets(top: 0, left: 0, bottom: 8, right: 0)
        stack.frame = NSRect(x: 0, y: 0, width: 640, height: 680)
        applyView()
        let vc = NSViewController()
        vc.view = stack
        popover.contentViewController = vc
        popover.behavior = .transient

        location.delegate = self
        location.desiredAccuracy = kCLLocationAccuracyHundredMeters
        location.distanceFilter = 50
        watchLocation()
        // The Mac may have moved while asleep: restarting tracking sends a fresh fix within seconds; the old place
        // holds until it lands, rather than dropping to the global mode.
        NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { [weak self] _ in
            self?.location.stopUpdatingLocation()
            self?.watchLocation()
        }
        Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in self?.reload(); self?.refreshReview(); self?.refreshState() }
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
            let places = NSMenuItem(title: "Places…", action: #selector(showPlaces), keyEquivalent: "")
            places.target = self
            menu.addItem(places)
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

    func ozen(_ args: String..., done: ((String) -> Void)? = nil) {
        run(args) { out, _, _ in done?(out) }
    }

    /// Runs the CLI off the main thread; calls back on it with stdout, stderr and the exit code.
    func run(_ args: [String], done: @escaping (String, String, Int32) -> Void) {
        DispatchQueue.global().async {
            let p = Process()
            let (pipe, errPipe) = (Pipe(), Pipe())
            p.executableURL = dir.appendingPathComponent("target/release/ozen")
            p.arguments = args
            p.standardOutput = pipe
            p.standardError = errPipe
            guard (try? p.run()) != nil else { return DispatchQueue.main.async { done("", "can't run \(p.executableURL!.path)", -1) } }
            // stderr is a few lines at most, so reading stdout first can't block on a full stderr pipe
            let out = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            let err = String(decoding: errPipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            p.waitUntilExit()
            let trim = { (s: String) in s.trimmingCharacters(in: .whitespacesAndNewlines) }
            DispatchQueue.main.async { done(trim(out), trim(err), p.terminationStatus) }
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
        let place = currentPlace()
        if place?.label != placeLabel {
            placeLabel = place?.label
            lastWanted = nil  // arriving at or leaving a place applies right away
        }
        let wanted = place.map { $0.action == "record" || $0.action == "meetings" && inMeeting } ?? (mode == "always" || inMeeting)
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
        let image = NSImage(systemSymbolName: icon, accessibilityDescription: "ozen \(s)")
        // Only recording is red and filled; every other state is the plain monochrome menu bar icon.
        item.button?.image = s == "recording"
            ? image?.withSymbolConfiguration(.init(paletteColors: [.systemRed])).map { $0.isTemplate = false; return $0 }
            : image
        let inMeeting = lastMeeting.map { Date().timeIntervalSince($0) < meetingGrace } ?? false
        let place = currentPlace()
        let meetingsOnly = place.map { $0.action == "meetings" } ?? (mode == "meetings")
        let meeting = (meetingsOnly && inMeeting ? " · \(meetingName ?? "meeting")" : "") + (place.map { " · \($0.label)" } ?? "")
        // Anything but recording says so first, so a paused, finishing or waiting state never reads as recording.
        status.stringValue = s == "recording" ? "● Recording\(meeting)"
            : "Not recording · " + (["paused": "paused", "stopping": "finishing transcription…"][s]
                ?? (meetingsOnly ? "waiting for a meeting" : "stopped") + (place.map { " · \($0.label)" } ?? ""))
        item.button?.toolTip = "Ozen: " + status.stringValue
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

    // MARK: places

    func currentPlace() -> Place? {
        // No age limit: tracking sends no new fix while you stand still, so an old fix at home is still home.
        // A failed or revoked location clears `here` instead, so a stale place never outlives its source.
        guard let here else { return nil }
        return loadPlaces().first { p in
            guard let lat = p.lat, let lon = p.lon else { return false }
            return here.distance(from: CLLocation(latitude: lat, longitude: lon)) <= p.radius ?? defaultRadius
        }
    }

    /// Track location only while some place has coordinates, so an unused feature never asks for location.
    func watchLocation() {
        if loadPlaces().contains(where: { $0.lat != nil }) {
            location.requestAlwaysAuthorization()
            location.startUpdatingLocation()
        } else {
            location.stopUpdatingLocation()
            here = nil
        }
    }

    func locationManager(_ m: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard let fix = locations.last else { return }
        here = fix
        if let i = settingPlace {
            settingPlace = nil
            var places = loadPlaces()
            if places.indices.contains(i) {
                places[i].lat = fix.coordinate.latitude
                places[i].lon = fix.coordinate.longitude
                savePlaces(places)
                watchLocation()
                buildPlaces()
            }
        }
        autoControl()
    }

    func locationManager(_ m: CLLocationManager, didFailWithError error: Error) {
        if (error as? CLError)?.code != .locationUnknown { here = nil }  // locationUnknown is transient; it keeps trying
        if settingPlace != nil {
            settingPlace = nil
            placesNote.stringValue = "Couldn't get your location: \(error.localizedDescription)"
        }
    }

    func locationManagerDidChangeAuthorization(_ m: CLLocationManager) {
        if m.authorizationStatus == .denied || m.authorizationStatus == .restricted {
            here = nil
            placesNote.stringValue = "Location access is off. Turn on Ozen in System Settings → Privacy & Security → Location Services."
        }
    }

    @objc func showPlaces() {
        if placesWindow == nil {
            let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 240), styleMask: [.titled, .closable],
                             backing: .buffered, defer: false)
            w.title = "Ozen Places"
            w.isReleasedWhenClosed = false
            placesStack.orientation = .vertical
            placesStack.alignment = .leading
            placesStack.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
            placesNote.font = .systemFont(ofSize: 11)
            placesNote.textColor = .secondaryLabelColor
            w.contentView = placesStack
            placesMap.navigationDelegate = self
            placesMap.configuration.userContentController.add(self, name: "ozen")  // map.html → pin drags and map clicks
            placesMap.customUserAgent = "Ozen (https://github.com/tupe12334/ozen)"  // OSM tile policy: identify the app
            placesMap.heightAnchor.constraint(equalToConstant: 280).isActive = true
            // Bundled by `ozen app`; a checkout run (Ozen [dir]) falls back to the repo copy.
            let page = Bundle.main.url(forResource: "map", withExtension: "html") ?? dir.appendingPathComponent("map.html")
            placesMap.loadFileURL(page, allowingReadAccessTo: page.deletingLastPathComponent())
            placesWindow = w
        }
        buildPlaces()
        placesWindow?.center()
        placesWindow?.makeKeyAndOrderFront(nil)
        NSApp.activate()
    }

    /// One row per place: label, action, where it is, and buttons; the row index rides in each control's tag.
    func buildPlaces(fit: Bool = true) {
        rebuilding = true
        placesStack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        rebuilding = false
        let intro = NSTextField(wrappingLabelWithString: "While you're within a place's radius, its setting replaces Always/Meetings. "
            + "Type coordinates, use where you are now, pick a spot on the map, or drag a pin.")
        intro.font = .systemFont(ofSize: 12)
        placesStack.addArrangedSubview(intro)
        placesStack.addArrangedSubview(placesMap)
        for (i, p) in loadPlaces().enumerated() {
            let name = NSTextField(string: p.label)
            name.placeholderString = "Label"
            name.tag = i
            name.target = self
            name.action = #selector(renamePlace(_:))
            name.cell?.sendsActionOnEndEditing = true  // clicking away saves too, not only Return
            name.widthAnchor.constraint(equalToConstant: 160).isActive = true
            let action = NSPopUpButton(frame: .zero, pullsDown: false)
            action.addItems(withTitles: placeActions.map(\.1))
            action.selectItem(at: placeActions.firstIndex { $0.0 == p.action } ?? 0)
            action.tag = i
            action.target = self
            action.action = #selector(placeActionChanged(_:))
            let remove = smallButton("Remove", #selector(removePlace(_:)), i)
            placesStack.addArrangedSubview(NSStackView(views: [name, action, NSView(), remove]))
            // Every value is editable by hand; empty latitude or longitude means "not set".
            let fields = [("lat", "Latitude", p.lat), ("lon", "Longitude", p.lon), ("radius", "\(Int(defaultRadius))", p.radius)].map { key, hint, v in
                let f = NSTextField(string: v.map { key == "radius" ? String(Int($0)) : String(format: "%.6f", $0) } ?? "")
                f.placeholderString = hint
                f.identifier = NSUserInterfaceItemIdentifier(key)
                f.tag = i
                f.target = self
                f.action = #selector(placeValueChanged(_:))
                f.cell?.sendsActionOnEndEditing = true
                f.widthAnchor.constraint(equalToConstant: key == "radius" ? 56 : 104).isActive = true
                return f
            }
            let status = settingPlace == i ? "Locating…" : pickingPlace == i ? "Click the map…" : ""
            let row = NSStackView(views: [NSTextField(labelWithString: "Lat"), fields[0], NSTextField(labelWithString: "Lon"), fields[1],
                                          NSTextField(labelWithString: "Radius"), fields[2], NSTextField(labelWithString: "m"),
                                          smallButton("Use current location", #selector(setPlaceHere(_:)), i),
                                          smallButton("Pick on map", #selector(pickOnMap(_:)), i), NSTextField(labelWithString: status)])
            row.edgeInsets = NSEdgeInsets(top: 0, left: 8, bottom: 6, right: 0)
            placesStack.addArrangedSubview(row)
        }
        let add = NSButton(title: "Add place", target: self, action: #selector(addPlace))
        add.bezelStyle = .rounded
        placesStack.addArrangedSubview(add)
        placesStack.addArrangedSubview(placesNote)
        let rowWidth = placesStack.arrangedSubviews.dropFirst(2).map(\.fittingSize.width).max() ?? 536
        placesMap.constraints.filter { $0.firstAttribute == .width }.forEach { placesMap.removeConstraint($0) }
        placesMap.widthAnchor.constraint(equalToConstant: rowWidth).isActive = true
        showPlacesOnMap(fit: fit)
        intro.preferredMaxLayoutWidth = rowWidth  // wrap the intro to the rows, so it never squeezes them
        placesWindow?.setContentSize(placesStack.fittingSize)
    }

    /// Hand the places to map.html, which draws each located one with its radius; red records, gray turns it off.
    func showPlacesOnMap(fit: Bool = true) {
        struct Here: Encodable { let lat: Double, lon: Double }
        let enc = JSONEncoder()
        guard let places = try? enc.encode(loadPlaces()), let places = String(data: places, encoding: .utf8) else { return }
        let here = self.here.flatMap { try? enc.encode(Here(lat: $0.coordinate.latitude, lon: $0.coordinate.longitude)) }
            .flatMap { String(data: $0, encoding: .utf8) } ?? "null"
        placesMap.evaluateJavaScript("show(\(places), \(defaultRadius), \(here), \(fit))")  // before the page loads this is a no-op
    }

    /// From map.html: {type: "move", index, lat, lon} when a pin is dragged, {type: "click", lat, lon} for a map click.
    func userContentController(_ c: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let m = message.body as? [String: Any], let lat = m["lat"] as? Double, let lon = m["lon"] as? Double else { return }
        let i = m["type"] as? String == "move" ? m["index"] as? Int : pickingPlace
        guard let i else { return }
        commitEdits()
        pickingPlace = nil
        placesMap.evaluateJavaScript("picking(false)")
        editPlaces({ if $0.indices.contains(i) { $0[i].lat = lat; $0[i].lon = lon } }, fit: false)  // the map stays where you put it
        buildPlaces(fit: false)
    }

    func smallButton(_ title: String, _ sel: Selector, _ tag: Int) -> NSButton {
        let b = NSButton(title: title, target: self, action: sel)
        b.tag = tag
        b.bezelStyle = .rounded
        b.controlSize = .small
        return b
    }

    @objc func pickOnMap(_ sender: NSButton) {
        commitEdits()
        placesNote.stringValue = ""
        pickingPlace = sender.tag
        placesMap.evaluateJavaScript("picking(true)")
        buildPlaces(fit: false)
    }

    /// A typed latitude, longitude or radius; anything out of range is refused and the row shows the saved value again.
    @objc func placeValueChanged(_ sender: NSTextField) {
        guard !rebuilding else { return }
        let text = sender.stringValue.trimmingCharacters(in: .whitespaces)
        let key = sender.identifier?.rawValue ?? ""
        let v = Double(text)
        let valid = text.isEmpty || v.map { key == "lat" ? abs($0) <= 90 : key == "lon" ? abs($0) <= 180 : $0 > 0 } == true
        guard valid else {
            placesNote.stringValue = "\(text) isn't a valid \(["lat": "latitude (−90…90)", "lon": "longitude (−180…180)"][key] ?? "radius in meters")."
            buildPlaces(fit: false)
            return
        }
        placesNote.stringValue = ""
        let i = sender.tag
        let old = loadPlaces()
        guard old.indices.contains(i) else { return }
        let current = key == "lat" ? old[i].lat : key == "lon" ? old[i].lon : old[i].radius
        guard v != current else { return }  // end-editing fires on every focus change: skip saves that change nothing
        editPlaces({
            switch key {
            case "lat": $0[i].lat = v
            case "lon": $0[i].lon = v
            default: $0[i].radius = v
            }
        }, fit: key != "radius")
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) { showPlacesOnMap() }

    func editPlaces(_ change: (inout [Place]) -> Void, fit: Bool = true) {
        var places = loadPlaces()
        change(&places)
        savePlaces(places)
        watchLocation()
        lastWanted = nil  // a changed place applies right away
        autoControl()
        showPlacesOnMap(fit: fit)
    }

    /// Save a label still being typed while row indexes are valid; a field removed mid-edit would rename the wrong row.
    func commitEdits() { placesWindow?.makeFirstResponder(nil) }

    @objc func renamePlace(_ sender: NSTextField) {
        guard !rebuilding else { return }
        let label = sender.stringValue.trimmingCharacters(in: .whitespaces)
        editPlaces { if $0.indices.contains(sender.tag), !label.isEmpty { $0[sender.tag].label = label } }
    }

    @objc func placeActionChanged(_ sender: NSPopUpButton) {
        editPlaces { if $0.indices.contains(sender.tag) { $0[sender.tag].action = placeActions[sender.indexOfSelectedItem].0 } }
    }

    @objc func setPlaceHere(_ sender: NSButton) {
        commitEdits()
        placesNote.stringValue = ""
        settingPlace = sender.tag
        if let here, Date().timeIntervalSince(here.timestamp) < 120 {
            locationManager(location, didUpdateLocations: [here])  // already tracking and standing still: no new fix will come
            return
        }
        location.requestAlwaysAuthorization()
        location.requestLocation()  // one fresh fix, even when an older one is cached
        buildPlaces()
    }

    @objc func removePlace(_ sender: NSButton) {
        commitEdits()
        editPlaces { if $0.indices.contains(sender.tag) { $0.remove(at: sender.tag) } }
        buildPlaces()
    }

    @objc func addPlace() {
        commitEdits()
        editPlaces { $0.append(Place(label: "Place \($0.count + 1)", action: "record")) }
        buildPlaces()
    }

    // MARK: transcript / timeline switch

    @objc func switchView() {
        UserDefaults.standard.set(viewControl.selectedSegment, forKey: "view")
        applyView()
    }

    func applyView() {
        let showTimeline = viewControl.selectedSegment == 1, showMeetings = viewControl.selectedSegment == 2
        scroll.isHidden = showTimeline || showMeetings
        timelineScroll.isHidden = !showTimeline
        meetingsScroll.isHidden = !showMeetings
        zoomIn.isHidden = !showTimeline
        zoomOut.isHidden = !showTimeline
        gatherButton.isHidden = !showMeetings
        kevButton.isHidden = !showMeetings
        if showTimeline { scrollTimelineToEnd() }
        if showMeetings { loadMeetings() }
    }

    // MARK: meetings -> context folder for Claude Code / Hermes

    func loadMeetings() {
        ozen("meetings") { out in
            let picked = Set(self.meetingsTable.selectedRowIndexes.map { self.meetings[$0][0] })
            self.meetings = out.split(separator: "\n").map { $0.split(separator: "\t", omittingEmptySubsequences: false).map(String.init) }
                .filter { $0.count == 5 }
            self.meetingsTable.reloadData()
            self.meetingsTable.selectRowIndexes(IndexSet(self.meetings.indices.filter { picked.contains(self.meetings[$0][0]) }),
                                                byExtendingSelection: false)
        }
    }

    func numberOfRows(in tableView: NSTableView) -> Int { meetings.count }

    func tableView(_ tableView: NSTableView, objectValueFor col: NSTableColumn?, row: Int) -> Any? {
        let i = ["When": 1, "Min": 2, "Lines": 3][col?.identifier.rawValue ?? ""] ?? 4
        return meetings[row][i]
    }

    @objc func gather(_ sender: Any?) {
        let ids = meetingsTable.selectedRowIndexes.map { meetings[$0][0] }
        guard !ids.isEmpty else { return }
        let kev = (sender as? NSButton) == kevButton
        kevButton.isEnabled = false
        gatherButton.isEnabled = false
        if kev { kevButton.title = "Asking Kev…" }
        run((kev ? ["gather", "--kev"] : ["gather"]) + ids) { out, err, code in
            self.kevButton.isEnabled = true
            self.gatherButton.isEnabled = true
            self.kevButton.title = "Auto add with Kev"
            let alert = NSAlert()
            NSApp.activate()
            guard code == 0, let folder = out.split(separator: "\n").last.map(String.init) else {
                alert.messageText = "Couldn't gather the transcripts"
                alert.informativeText = err
                alert.runModal()
                return
            }
            let files = (try? FileManager.default.contentsOfDirectory(atPath: folder).filter { $0.hasPrefix("2") }.sorted()) ?? []
            alert.messageText = "\(files.count) transcript\(files.count == 1 ? "" : "s") ready"
            alert.informativeText = files.joined(separator: "\n") + (kev ? "\n\nKev's scores:\n" + err : "") + "\n\n" + folder
            for b in ["Claude Code", "Hermes", "Show in Finder"] { alert.addButton(withTitle: b) }
            switch alert.runModal() {
            case .alertFirstButtonReturn: NSWorkspace.shared.open(URL(fileURLWithPath: folder + "/claude.command"))
            case .alertSecondButtonReturn: NSWorkspace.shared.open(URL(fileURLWithPath: folder + "/hermes.command"))
            default: NSWorkspace.shared.open(URL(fileURLWithPath: folder))
            }
        }
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
                            src: r["src"] as? String ?? "", text: r["text"] as? String ?? "", run: r["run"] as? Int,
                            doubt: r["doubt"] as? Double))
        }.sorted { $0.0 < $1.0 }.map(\.1)
    }

    func reload() {
        guard popover.isShown else { return }
        let files = ["lines.jsonl", "tags.json", "labels.json", "stats.json", "fixes.json"]
        let sig = files.map { f -> String in
            let a = try? FileManager.default.attributesOfItem(atPath: dir.appendingPathComponent(f).path)
            return "\(a?[.size] ?? 0)-\((a?[.modificationDate] as? Date)?.timeIntervalSince1970 ?? 0)"
        }.joined(separator: "|") + "\(pending)\(pendingFixes)"
        guard sig != signature else { return }
        signature = sig
        let atBottom = scroll.verticalScroller.map { $0.floatValue > 0.98 } ?? true

        let tags = (json("tags.json") as? [String: String] ?? [:]).merging(pending) { $1 }
        let labels = json("labels.json") as? [String: [String: Any]] ?? [:]
        let fixes = (json("fixes.json") as? [String: String] ?? [:]).merging(pendingFixes) { $1 }
        let threshold = (json("stats.json") as? [String: Any])?["threshold"] as? Double
        // How unsure a line is, nil when sure: train.py's verdict once it has retrained, before that the
        // transcriber's own (same formula), so new lines reach Review without waiting for the next tag.
        func unsureBy(_ l: Line) -> Double? {
            guard let g = labels[l.id] else { return l.doubt.flatMap { $0 < unsureDoubt ? $0 : nil } }
            guard g["unsure"] as? Bool == true else { return nil }
            guard let sim = g["sim"] as? Double, let thr = threshold else { return 0 }
            return min(abs(sim - thr), g["margin"] as? Double ?? 0)
        }
        var queue: [(Double, String)] = []
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
                                                        text: l.text, unsure: !tagged && unsureBy(l) != nil)
        }
        shown = [:]
        if atEnd { scrollTimelineToEnd() }
        if all.isEmpty { out.append(NSAttributedString(string: "No transcript yet. Press Start.", attributes: [.foregroundColor: NSColor.secondaryLabelColor])) }
        for l in all {
            let tagged = !(tags[l.id] ?? "").isEmpty
            let guess = labels[l.id]
            let doubt = tagged ? nil : unsureBy(l)
            let unsure = doubt != nil
            if let doubt { queue.append((doubt, l.id)) }
            let speaker = tagged ? tags[l.id]! : ((guess?["spk"] as? String) ?? l.spk)
            let said = fixes[l.id].flatMap { $0.isEmpty ? nil : $0 } ?? l.text
            shown[l.id] = (speaker, l.t, l.run)
            let ignored = speaker == ignoreTag
            let para = NSMutableParagraphStyle()
            para.paragraphSpacing = 6
            if said.unicodeScalars.contains(where: { (0x0590...0x05FF).contains($0.value) }) {
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
            out.append(NSAttributedString(string: said, attributes: base.merging([
                .font: NSFont.systemFont(ofSize: ignored ? 11 : 13), .link: URL(string: "ozen://fix/\(l.id)")!,
                .foregroundColor: ignored ? NSColor.tertiaryLabelColor : NSColor.labelColor,
                .toolTip: said == l.text ? "Click to fix the text" : "Fixed. Heard: \(l.text)",
            ]) { $1 }))
            out.append(NSAttributedString(string: "\n", attributes: base))
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
        reviewQueue = queue.sorted { $0.0 < $1.0 }.map(\.1)
        refreshReview()
        let ignoredLines = (s["ignored"] as? Int ?? 0) > 0 ? " · \(s["ignored"]!) ignored" : ""
        footer.stringValue = "  \(acc) · \(tagged) tagged\(ignoredLines) · orange ? = unsure, tag it to teach ozen · click text to fix it"
    }

    // Only ask about recent lines: after 10 minutes nobody remembers who said what.
    func refreshReview() {
        let cutoff = Date().timeIntervalSince1970 - reviewMaxAge
        review = reviewQueue.filter { (shown[$0]?.t ?? 0) >= cutoff }
        reviewButton.title = review.isEmpty ? "Review" : "Review \(review.count)"
        reviewButton.isEnabled = !review.isEmpty
    }

    // Jump to the line ozen is least sure about and ask who said it.
    @objc func reviewNext() {
        refreshReview()
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
        guard let url = link as? URL else { return false }
        if url.host == "fix" { fixText(url.lastPathComponent); return true }
        guard url.host == "tag" else { return false }
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
        if let (spk, t, run) = shown[id], spk.range(of: "^S[0-9]+$", options: .regularExpression) != nil {
            // S1, S2... restart with the transcriber, so only that run's lines are the same voice
            // (older lines have no run: those within an hour).
            let same = shown.filter { l in l.value.spk == spk && (run != nil ? l.value.run == run : abs(l.value.t - t) < 3600) }.map(\.key)
            let all = NSMenuItem(title: "Ignore all \(same.count) line\(same.count == 1 ? "" : "s") by \(spk)", action: #selector(ignoreAll(_:)), keyEquivalent: "")
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
        tag(ids, ignoreTag, command: "cd \"$OZEN_DIR\" && target/release/ozen ignore ${=OZEN_ID}")
    }

    func tag(_ id: String, _ name: String) {
        tag([id], name, command: "cd \"$OZEN_DIR\" && target/release/ozen tag \"$OZEN_ID\" \"$OZEN_NAME\"")
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

    // Clicking a line's text: correct what was said. Empty restores what ozen heard.
    func fixText(_ id: String) {
        guard let line = lines(limit: 5000).first(where: { $0.id == id }) else { return }
        let current = pendingFixes[id] ?? (json("fixes.json") as? [String: String])?[id] ?? line.text
        let alert = NSAlert()
        alert.messageText = "What was really said?"
        alert.informativeText = "Heard: \(line.text)\nozen learns the words you add, and applies a correction you make twice."
        alert.addButton(withTitle: "Fix")
        alert.addButton(withTitle: "Cancel")
        let field = NSTextView(frame: NSRect(x: 0, y: 0, width: 420, height: 90))
        field.string = current
        field.font = .systemFont(ofSize: 13)
        field.isRichText = false
        if current.unicodeScalars.contains(where: { (0x0590...0x05FF).contains($0.value) }) {
            field.baseWritingDirection = .rightToLeft
            field.alignment = .right
        }
        let box = NSScrollView(frame: field.frame)
        box.documentView = field
        box.hasVerticalScroller = true
        box.borderType = .bezelBorder
        alert.accessoryView = box
        alert.window.initialFirstResponder = field
        NSApp.activate()
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        let fixed = field.string.trimmingCharacters(in: .whitespacesAndNewlines)
        pendingFixes[id] = fixed
        reload()
        ozen("fix", id, fixed) { _ in
            self.pendingFixes[id] = nil
            self.signature = ""
            self.reload()
        }
    }

}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)  // menu bar only, no Dock icon
let delegate = App()
app.delegate = delegate
app.run()
