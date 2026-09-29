// Menu bar ear icon: left-click shows the live transcript with start/pause/stop controls,
// right-click offers the same controls plus Quit (stops recording) or quit the bar alone. Controls call the ozen CLI (src/main.rs).
// Click a speaker name in the transcript to tag who really said that line; every tag retrains
// the voiceprints (src/train.rs), so labels improve the more you tag. Click a line's text to fix what was
// said; fixes teach the transcriber words and repeated corrections (`ozen fix`, src/fixes.rs).
// Ignore a voice (a video playing nearby) and ozen stops transcribing it; ignored lines show dimmed and leave
// the timeline. Each ignore is its own voice (Ignored, Ignored 2...), so you can stop ignoring one alone.
// Record mode: Always, or Meetings (auto start/stop while a meeting app is using the microphone).
// Places: labeled locations that override the mode while you're there (auto record, or auto off).
// Meetings view: pick past meetings (⌘/⇧-click for several), optionally let Kev add related ones, and start
// Claude Code or Hermes in a folder holding their transcripts (`ozen gather`).
// Timebar…: every recorded chunk on a local-time bar, done or still waiting, and the transcriber's pace (chunks.html).
// Built into ~/Applications/Ozen.app by `ozen app`. Direct use: Ozen [dir] [--open]
import AppKit
import CoreAudio
import CoreLocation
import WebKit  // hosts map.html (Leaflet + OpenStreetMap), the same map any OS can show

let args = CommandLine.arguments.dropFirst().filter { !$0.hasPrefix("--") }
// Launched as Ozen.app (Finder/Spotlight) there are no args: use the standard checkout.
let dir = URL(fileURLWithPath: args.first ?? NSString(string: "~/ozen").expandingTildeInPath)

// Ozen.app from the DMG carries a prebuilt checkout in Resources/ozen (.github/workflows/release.yml): copy it into
// ~/ozen on first launch and after an update. Only the files it ships are overwritten; recordings and tags stay.
// A git checkout there is a developer's, built with `ozen app`: leave it alone.
func installBundled(from src: URL, to dest: URL) {
    let fm = FileManager.default
    let version = { (d: URL) in try? String(contentsOf: d.appendingPathComponent(".version"), encoding: .utf8) }
    guard let v = version(src), !fm.fileExists(atPath: dest.appendingPathComponent(".git").path),
          version(dest) != v else { return }
    try? fm.createDirectory(at: dest, withIntermediateDirectories: true)
    // A downloaded app's files are quarantined, which would block its CLI; .version goes last, so a copy cut
    // short is retried on the next launch.
    for (tool, a) in [("/usr/bin/rsync", ["-a", "--exclude=.version", src.path + "/", dest.path + "/"]),
                      ("/usr/bin/xattr", ["-dr", "com.apple.quarantine", dest.appendingPathComponent("target").path])] {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: tool)
        p.arguments = a
        guard (try? p.run()) != nil else { return }
        p.waitUntilExit()
        if tool.hasSuffix("rsync") && p.terminationStatus != 0 { return }
    }
    try? v.write(to: dest.appendingPathComponent(".version"), atomically: true, encoding: .utf8)
}
if args.isEmpty, let res = Bundle.main.resourceURL {
    installBundled(from: res.appendingPathComponent("ozen"), to: dir)
}

let maxLines = 400
let ignoreTag = "Ignored"  // reserved tag, same as src/ignore.rs / src/transcribe.rs
/// ignoreTag or one of its numbered voices ("Ignored 2"), same as src/ignore.rs is_ignored.
func isIgnored(_ name: String) -> Bool { name.range(of: "^Ignored( [0-9]+)?$", options: .regularExpression) != nil }

func json(_ name: String) -> Any? {
    (try? Data(contentsOf: dir.appendingPathComponent(name))).flatMap { try? JSONSerialization.jsonObject(with: $0) }
}

/// What the ozen CLI decides for the panel (src/panel.rs), as JSON. Blocks: these commands only read a few files.
func cli(_ args: String...) -> Any? {
    let p = Process(), pipe = Pipe()
    p.executableURL = dir.appendingPathComponent("target/release/ozen")
    p.arguments = args
    p.standardOutput = pipe
    guard (try? p.run()) != nil else { return nil }
    let out = pipe.fileHandleForReading.readDataToEndOfFile()
    p.waitUntilExit()
    return try? JSONSerialization.jsonObject(with: out)
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

/// Scroll content that starts at the top.
final class FlippedView: NSView { override var isFlipped: Bool { true } }

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
    var state = "stopped"  // from `ozen status`: recording | paused | stopping | processing | stopped
    var problems: [String] = []  // from `ozen health`: why recording isn't turning into transcript
    let warning = NSTextField(wrappingLabelWithString: "")
    let status = NSTextField(labelWithString: "")
    let startButton = NSButton(title: "Start", target: nil, action: nil)
    let pauseButton = NSButton(title: "Pause", target: nil, action: nil)
    let stopButton = NSButton(title: "Stop", target: nil, action: nil)
    let processButton = NSButton(title: "Process", target: nil, action: nil)
    let advancedButton = NSButton(title: "", target: nil, action: nil)
    var advancedWindow: NSWindow?
    // Advanced setting, off by default: Record only records and Process transcribes the queue, each on its own.
    var split: Bool { UserDefaults.standard.bool(forKey: "split") }
    let reviewButton = NSButton(title: "Review", target: nil, action: nil)
    var headerRanges: [String: NSRange] = [:]  // line id -> speaker name range in the text view
    var queued = 0  // chunks waiting to be transcribed, from `ozen controls`
    var controlsAsked = 0  // only the newest `ozen controls` answer is drawn
    var reviewQueue: [(id: String, until: Double)] = []  // `ozen unsure`: most uncertain first, and when each ages out
    var review: [String] = []  // the queue minus lines too old to remember who said them
    var shown: Set<String> = []  // line ids in the transcript
    let modeControl = NSSegmentedControl(labels: ["Always", "Meetings"], trackingMode: .selectOne, target: nil, action: nil)
    var mode: String { UserDefaults.standard.string(forKey: "mode") ?? "always" }  // "always" | "meetings"
    var lastMeeting: Date?, meetingName: String?
    var lastWanted: Bool?  // act only when "should be recording" flips, so manual Pause/Stop stick until then
    var adopted = false  // made the first decision since launch
    let launchedAt = Date()
    let timeline = TimelineView()
    let timelineScroll = NSScrollView()
    let viewControl = NSSegmentedControl(labels: ["Transcript", "Timeline", "Meetings"], trackingMode: .selectOne, target: nil, action: nil)
    let zoomOut = NSButton(title: "−", target: nil, action: nil)
    let zoomIn = NSButton(title: "+", target: nil, action: nil)
    let placesButton = NSButton(title: "Places…", target: nil, action: nil)
    let voicesButton = NSButton(title: "Voices…", target: nil, action: nil)
    let timebarButton = NSButton(title: "Timebar…", target: nil, action: nil)
    var timebarWindow: NSWindow?
    let timebarView = WKWebView()
    var timebarTimer: Timer?
    let location = CLLocationManager()
    var here: (lat: Double, lon: Double)?  // from `ozen place`; nil until the first fix
    var placeNow: Place?  // the place you're in, from `ozen place`
    var supplyingLocation = false  // the app is writing here.json because the locate binary can't (see supplyLocationIfNeeded)
    var placeLabel: String?  // label of the place we're in; a change re-applies auto control
    var settingPlace: Int?  // row waiting for a location fix after "Use current location"
    var pickingPlace: Int?  // row waiting for a map click after "Pick on map"
    var rebuilding = false  // removing a focused field fires its action; ignore those echoes
    var placesWindow: NSWindow?
    var voicesWindow: NSWindow?
    let voicesStack = NSStackView()
    var voices: [[String: Any]] = []  // `ozen voices`: people, this run's unnamed speakers, ignored
    let placesStack = NSStackView()
    let placesMap = WKWebView()
    let placesNote = NSTextField(wrappingLabelWithString: "")
    let meetingsTable = NSTableView()
    let meetingsScroll = NSScrollView()
    var meetings: [[String]] = []  // `ozen meetings` rows: id, start, minutes, lines, first words
    let gatherButton = NSButton(title: "Open", target: nil, action: nil)
    let kevButton = NSButton(title: "Auto add with Kev", target: nil, action: nil)
    let askButton = NSButton(title: "Ask AI", target: nil, action: nil)
    let quitButton = NSButton(title: "Quit", target: nil, action: nil)

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
        for (b, cmd) in [(startButton, #selector(startCapture)), (pauseButton, #selector(pauseCapture)), (stopButton, #selector(stopCapture)), (processButton, #selector(processQueue)), (advancedButton, #selector(showAdvanced)), (reviewButton, #selector(reviewNext)), (placesButton, #selector(showPlaces)), (voicesButton, #selector(showVoices)), (timebarButton, #selector(showTimebar)), (quitButton, #selector(quitOzen))] {
            b.target = self
            b.action = cmd
            b.bezelStyle = .rounded
            b.controlSize = .small
        }
        advancedButton.image = NSImage(systemSymbolName: "gearshape", accessibilityDescription: "Advanced settings")
        advancedButton.imagePosition = .imageOnly
        advancedButton.toolTip = "Advanced settings"
        processButton.toolTip = "Transcribe the recorded audio that's waiting, then stop"
        status.font = .boldSystemFont(ofSize: 12)
        warning.font = .systemFont(ofSize: 12)
        warning.textColor = .systemOrange
        modeControl.target = self
        modeControl.action = #selector(modeChanged)
        modeControl.controlSize = .small
        modeControl.selectedSegment = mode == "meetings" ? 1 : 0
        modeControl.toolTip = "Always: record until you stop. Meetings: start and stop automatically with Zoom/Meet/Teams/Slack/FaceTime calls."
        askButton.target = self
        askButton.action = #selector(askMenu(_:))
        askButton.bezelStyle = .rounded
        askButton.controlSize = .small
        askButton.image = NSImage(systemSymbolName: "sparkles", accessibilityDescription: "AI")  // the usual mark for AI features
        askButton.imagePosition = .imageLeading
        askButton.toolTip = "Start Claude Code or Hermes on the meeting happening now (ozen live)"
        let controls = NSStackView(views: [status, NSView(), askButton, modeControl, placesButton, voicesButton, timebarButton, reviewButton, startButton, pauseButton, stopButton, processButton, advancedButton, quitButton])
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
        askLocation()
        // The Mac may have moved while asleep: restarting the watcher sends a fresh fix within seconds; the old place
        // holds until it lands, rather than dropping to the global mode.
        NSWorkspace.shared.notificationCenter.addObserver(forName: NSWorkspace.didWakeNotification, object: nil, queue: .main) { [weak self] _ in
            self?.ozen("place", "--restart") { self?.applyPlace($0) }
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
            for (title, sel, on) in [(startButton.title, #selector(startCapture), startButton.isEnabled),
                                     ("Pause", #selector(pauseCapture), !pauseButton.isHidden && pauseButton.isEnabled),
                                     ("Stop", #selector(stopCapture), stopButton.isEnabled),
                                     (processButton.title, #selector(processQueue), !processButton.isHidden && processButton.isEnabled)] where on {
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
            let voicesItem = NSMenuItem(title: "Voices…", action: #selector(showVoices), keyEquivalent: "")
            voicesItem.target = self
            menu.addItem(voicesItem)
            let timebarItem = NSMenuItem(title: "Timebar…", action: #selector(showTimebar), keyEquivalent: "")
            timebarItem.target = self
            menu.addItem(timebarItem)
            let advancedItem = NSMenuItem(title: "Advanced…", action: #selector(showAdvanced), keyEquivalent: "")
            advancedItem.target = self
            menu.addItem(advancedItem)
            menu.addItem(.separator())
            let quit = NSMenuItem(title: "Quit ozen", action: #selector(quitOzen), keyEquivalent: "q")
            quit.target = self
            menu.addItem(quit)
            menu.addItem(withTitle: "Quit bar, keep recording", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "")
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
        ozen("place") { self.applyPlace($0) }
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
        // Just launched, places set, no location yet: wait (up to 30s) rather than let the global mode decide
        // for a place we can't see yet, e.g. start recording at a place set to meetings only.
        if place == nil, here == nil, Date().timeIntervalSince(launchedAt) < 30, loadPlaces().contains(where: { $0.lat != nil }) {
            return show(state: state)
        }
        let wanted = place.map { $0.action == "record" || $0.action == "meetings" && inMeeting } ?? (mode == "always" || inMeeting)
        defer { lastWanted = wanted; show(state: state) }
        guard wanted != lastWanted else { return }
        // First decision after launch may start recording, never stop one: a recording already running was
        // started by hand (or by the previous app), and a relaunch or rebuild shouldn't end it.
        if !adopted {
            adopted = true
            if !wanted { return }
        }
        if wanted, state == "stopped" || state == "paused" || state == "processing" {
            startCapture()
        } else if !wanted, state == "recording" || state == "paused" {
            stopCapture()
        }
    }

    func show(state s: String) {
        state = s
        let icon = s == "recording" && !problems.isEmpty ? "ear.trianglebadge.exclamationmark"
            : ["recording": "ear.fill", "paused": "pause.circle", "stopping": "hourglass", "processing": "hourglass"][s] ?? "ear"
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
        let processing = FileManager.default.fileExists(atPath: dir.appendingPathComponent(".processing").path)
        let recordOnly = FileManager.default.fileExists(atPath: dir.appendingPathComponent(".record-only").path)
        status.stringValue = s == "recording" ? "● Recording\(meeting)" + (recordOnly && !processing ? " · not transcribing" : "")
            : "Not recording · " + (["paused": "paused", "stopping": "finishing transcription…",
                                     "processing": "processing \(queued) chunks…"][s]
                ?? (meetingsOnly ? "waiting for a meeting" : "stopped") + (place.map { " · \($0.label)" } ?? ""))
        item.button?.toolTip = "Ozen: " + status.stringValue
        status.textColor = s == "recording" ? .systemRed : .secondaryLabelColor
        // Off the main thread: this runs on every status poll. Until it answers, the buttons keep their last state.
        controlsAsked += 1
        let asked = controlsAsked
        run(["controls", s, split ? "split" : ""]) { out, _, _ in
            guard asked == self.controlsAsked, let c = try? JSONSerialization.jsonObject(with: Data(out.utf8)) as? [String: Any] else { return }
            for (b, name) in [(self.startButton, "start"), (self.pauseButton, "pause"), (self.stopButton, "stop"), (self.processButton, "process")] {
                let c = c[name] as? [String: Any] ?? [:]
                if let title = c["title"] as? String { b.title = title }
                b.isEnabled = c["enabled"] as? Bool ?? false
                b.isHidden = c["hidden"] as? Bool ?? false
            }
            let q = c["queued"] as? Int ?? 0
            if q != self.queued { self.queued = q; self.show(state: s) }  // the status line counts them
        }
    }

    func control(_ cmd: String, optimistic: String) {
        show(state: optimistic)
        ozen(cmd) { _ in DispatchQueue.main.asyncAfter(deadline: .now() + 1) { self.refreshState() } }
    }

    @objc func startCapture() { control(state == "paused" ? "resume" : split ? "record" : "start", optimistic: "recording") }
    @objc func processQueue() {
        let stop = FileManager.default.fileExists(atPath: dir.appendingPathComponent(".processing").path)
        run(stop ? ["process", "stop"] : ["process"]) { _, _, _ in DispatchQueue.main.asyncAfter(deadline: .now() + 1) { self.refreshState() } }
    }
    @objc func pauseCapture() { control("pause", optimistic: "paused") }
    @objc func stopCapture() { control("stop", optimistic: "stopping") }
    // Stop returns at once (the drain runs detached), so the last words still reach the transcript after we exit.
    // Processing without a recording finishes by itself, so quitting leaves it running.
    @objc func quitOzen() { state == "stopped" || state == "processing" ? NSApp.terminate(nil) : ozen("stop") { _ in NSApp.terminate(nil) } }

    // MARK: advanced settings

    @objc func showAdvanced() {
        if advancedWindow == nil {
            let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 460, height: 170), styleMask: [.titled, .closable],
                             backing: .buffered, defer: false)
            w.title = "Ozen Advanced Settings"
            w.isReleasedWhenClosed = false
            let header = NSTextField(labelWithString: "Recording and processing")
            header.font = .boldSystemFont(ofSize: 12)
            let box = NSButton(checkboxWithTitle: "Split recording and processing", target: self, action: #selector(splitChanged(_:)))
            box.state = split ? .on : .off
            let note = NSTextField(wrappingLabelWithString: "Record then only records: nothing is transcribed while it runs, and the audio "
                + "waits in the chunks folder (it takes disk space until processed). Process transcribes the waiting audio, "
                + "with or without a recording going on, and stops once it's done. Off: Start records and transcribes together.")
            note.font = .systemFont(ofSize: 11)
            note.textColor = .secondaryLabelColor
            note.preferredMaxLayoutWidth = 420
            let stack = NSStackView(views: [header, box, note])
            stack.orientation = .vertical
            stack.alignment = .leading
            stack.spacing = 8
            stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
            w.contentView = stack
            advancedWindow = w
            w.center()
        }
        NSApp.activate()
        advancedWindow?.makeKeyAndOrderFront(nil)
    }

    @objc func splitChanged(_ sender: NSButton) {
        UserDefaults.standard.set(sender.state == .on, forKey: "split")
        // A recording in progress switches now. Turning split on keeps the live transcriber going as processing
        // (Stop processing ends it), so nothing already heard waits.
        if state == "recording" { ozen(split ? "record" : "start") { _ in self.refreshState() } }
        show(state: state)
    }

    // MARK: places

    func currentPlace() -> Place? { placeNow }

    /// `ozen place` output: {"here": {lat, lon, age} | null, "place": {label, action} | null}. Where you are and
    /// which place that is are decided in Rust (src/places.rs, src/bin/locate.rs); this only keeps the answer.
    func applyPlace(_ json: String) {
        guard let r = (try? JSONSerialization.jsonObject(with: Data(json.utf8))) as? [String: Any] else { return }
        let h = r["here"] as? [String: Any], p = r["place"] as? [String: Any]
        here = (h?["lat"] as? Double).flatMap { lat in (h?["lon"] as? Double).map { (lat, $0) } }
        let place = (p?["label"] as? String).map { Place(label: $0, action: p?["action"] as? String ?? "off") }
        if place?.label != placeNow?.label || place?.action != placeNow?.action {
            placeNow = place
            autoControl()
            if placesWindow?.isVisible == true { showPlacesOnMap(fit: false) }
        }
        supplyLocationIfNeeded()
    }

    /// Fallback: the locate binary shares the app's location permission through the app's identity. If it can't get
    /// a location (it writes here.json.error) while the app itself is allowed, the app writes here.json instead, so
    /// place switching keeps working; `ozen place` still decides which place that is. The locate binary retries each
    /// minute and clears the error on its first fix, which hands the job back.
    func supplyLocationIfNeeded() {
        let blocked = FileManager.default.fileExists(atPath: dir.appendingPathComponent("here.json.error").path)
        let allowed = location.authorizationStatus == .authorizedAlways
        let want = blocked && allowed && loadPlaces().contains(where: { $0.lat != nil })
        if want {  // heartbeat for `ozen health`: the app has the location covered, so don't warn
            FileManager.default.createFile(atPath: dir.appendingPathComponent("here.json.app").path, contents: nil)
        }
        guard want != supplyingLocation else { return }
        supplyingLocation = want
        want ? location.startUpdatingLocation() : location.stopUpdatingLocation()
    }

    func locationManager(_ m: CLLocationManager, didUpdateLocations locations: [CLLocation]) {
        guard supplyingLocation, let fix = locations.last else { return }
        let json = "{\"lat\":\(fix.coordinate.latitude),\"lon\":\(fix.coordinate.longitude),\"t\":\(fix.timestamp.timeIntervalSince1970)}"
        try? Data(json.utf8).write(to: dir.appendingPathComponent("here.json"), options: .atomic)
    }

    /// Only the app can show the location prompt (the locate binary uses the answer), so ask here once some
    /// place has coordinates; an unused feature never asks. macOS shows the prompt when updates start, not on the
    /// request alone, so start them until the user answers.
    func askLocation() {
        if loadPlaces().contains(where: { $0.lat != nil }), location.authorizationStatus == .notDetermined {
            location.requestAlwaysAuthorization()
            location.startUpdatingLocation()
        }
    }

    func locationManagerDidChangeAuthorization(_ m: CLLocationManager) {
        if m.authorizationStatus != .notDetermined, !supplyingLocation { m.stopUpdatingLocation() }  // answered: locate takes over
        if m.authorizationStatus == .denied || m.authorizationStatus == .restricted {
            placesNote.stringValue = "Location access is off. Turn on Ozen in System Settings → Privacy & Security → Location Services."
        }
    }

    // MARK: voices

    /// Everyone ozen has heard, to rename, merge, name, ignore or forget in bulk. A window, not the popover:
    /// the popover closes on every alert, and this is cleanup work between meetings.
    @objc func showVoices() {
        if voicesWindow == nil {
            let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 560, height: 520), styleMask: [.titled, .closable, .resizable],
                             backing: .buffered, defer: false)
            w.title = "Ozen Voices"
            w.isReleasedWhenClosed = false
            voicesStack.orientation = .vertical
            voicesStack.alignment = .leading
            voicesStack.spacing = 10
            voicesStack.edgeInsets = NSEdgeInsets(top: 12, left: 12, bottom: 12, right: 12)
            let flipped = FlippedView()
            flipped.addSubview(voicesStack)
            voicesStack.translatesAutoresizingMaskIntoConstraints = false
            NSLayoutConstraint.activate([
                voicesStack.topAnchor.constraint(equalTo: flipped.topAnchor),
                voicesStack.leadingAnchor.constraint(equalTo: flipped.leadingAnchor),
                voicesStack.trailingAnchor.constraint(equalTo: flipped.trailingAnchor),
                voicesStack.bottomAnchor.constraint(equalTo: flipped.bottomAnchor),
            ])
            let scroll = NSScrollView()
            scroll.hasVerticalScroller = true
            scroll.documentView = flipped
            flipped.translatesAutoresizingMaskIntoConstraints = false
            flipped.widthAnchor.constraint(equalTo: scroll.contentView.widthAnchor).isActive = true
            w.contentView = scroll
            voicesWindow = w
            w.center()
        }
        loadVoices()
        NSApp.activate()
        voicesWindow?.makeKeyAndOrderFront(nil)
    }

    func loadVoices(note: String? = nil) {
        run(["voices"]) { out, err, code in
            self.voices = (try? JSONSerialization.jsonObject(with: Data(out.utf8)) as? [[String: Any]]) ?? []
            self.buildVoices(note: code == 0 ? note : "Couldn't list voices: \(err)")
        }
    }

    func buildVoices(note: String? = nil) {
        voicesStack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        let intro = NSTextField(wrappingLabelWithString: note ?? "Rename or merge people, name this run's unnamed speakers, "
            + "and ignore voices that aren't in the meeting. Changes retrain the voiceprints.")
        intro.font = .systemFont(ofSize: 12)
        intro.textColor = .secondaryLabelColor
        voicesStack.addArrangedSubview(intro)
        if voices.isEmpty { voicesStack.addArrangedSubview(NSTextField(labelWithString: "No voices yet.")) }
        let hints = ["person": "", "unnamed": " · unnamed, this run", "ignored": " · not transcribed"]
        for v in voices {
            let name = v["name"] as? String ?? "?", kind = v["kind"] as? String ?? ""
            let n = v["lines"] as? Int ?? 0
            // Isolate names and lines: a Hebrew name would otherwise reorder the whole row ("lines 1 · נתן").
            let shown = "\u{2068}\(name)\u{2069}"
            let title = NSTextField(labelWithString: "\(shown) · \(n) line\(n == 1 ? "" : "s")\(hints[kind] ?? "")")
            title.font = .boldSystemFont(ofSize: 13)
            let actions: [(String, Selector)] = kind == "person" ? [("Rename…", #selector(renameVoice(_:))), ("Ignore…", #selector(ignoreVoice(_:))), ("Forget…", #selector(forgetVoice(_:)))]
                : kind == "unnamed" ? [("Name…", #selector(renameVoice(_:))), ("Ignore", #selector(ignoreVoice(_:)))]
                : [("Stop ignoring…", #selector(forgetVoice(_:)))]
            let buttons = actions.map { t, sel -> NSButton in
                let b = NSButton(title: t, target: self, action: sel)
                b.bezelStyle = .rounded
                b.controlSize = .small
                b.identifier = NSUserInterfaceItemIdentifier(name)
                return b
            }
            voicesStack.addArrangedSubview(NSStackView(views: [title, NSView()] + buttons))
            for line in v["recent"] as? [[String: Any]] ?? [] {
                // Click a line to see it in the transcript.
                let b = NSButton(title: "  “\u{2068}\(line["text"] as? String ?? "")\u{2069}”", target: self, action: #selector(showVoiceLine(_:)))
                b.isBordered = false
                b.font = .systemFont(ofSize: 11)
                b.contentTintColor = .secondaryLabelColor
                b.lineBreakMode = .byTruncatingTail
                b.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)  // truncate, don't widen the window
                b.identifier = NSUserInterfaceItemIdentifier(line["id"] as? String ?? "")
                voicesStack.addArrangedSubview(b)
                b.widthAnchor.constraint(lessThanOrEqualTo: voicesStack.widthAnchor, constant: -24).isActive = true
            }
        }
        voicesWindow?.contentView?.needsLayout = true
    }

    func voice(_ sender: NSButton) -> [String: Any]? { voices.first { $0["name"] as? String == sender.identifier?.rawValue } }

    func confirm(_ message: String, _ info: String, _ action: String) -> Bool {
        let alert = NSAlert()
        alert.messageText = message
        alert.informativeText = info
        alert.addButton(withTitle: action)
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }

    /// Runs a voices command, then refreshes this window and the transcript.
    func changeVoices(_ args: [String]) {
        buildVoices(note: "Retraining…")
        voicesStack.arrangedSubviews.forEach { ($0 as? NSStackView)?.views.forEach { ($0 as? NSButton)?.isEnabled = false } }
        run(args) { _, err, code in
            self.signature = ""
            self.reload()
            self.loadVoices(note: code == 0 ? nil : "That didn't work: \(err.trimmingCharacters(in: .whitespacesAndNewlines))")
        }
    }

    @objc func renameVoice(_ sender: NSButton) {
        guard let v = voice(sender), let name = v["name"] as? String else { return }
        let unnamed = v["kind"] as? String == "unnamed"
        let alert = NSAlert()
        alert.messageText = unnamed ? "Who is \(name)?" : "Rename \(name)"
        alert.informativeText = "Use an existing name to merge the two voices."
        alert.addButton(withTitle: unnamed ? "Name" : "Rename")
        alert.addButton(withTitle: "Cancel")
        let field = NSComboBox(frame: NSRect(x: 0, y: 0, width: 260, height: 26))
        let people = voices.filter { $0["kind"] as? String == "person" }.compactMap { $0["name"] as? String }.filter { $0 != name }
        field.addItems(withObjectValues: people)
        field.placeholderString = "Full name"
        alert.accessoryView = field
        alert.window.initialFirstResponder = field
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        let to = field.stringValue.trimmingCharacters(in: .whitespaces)
        guard !to.isEmpty, to != name, !isIgnored(to) else { return }
        if people.contains(to), !confirm("Merge \(name) into \(to)?", "All of \(name)'s lines become \(to)'s, and one voiceprint is built from both.", "Merge") { return }
        changeVoices(unnamed ? ["name", to] + (v["ids"] as? [String] ?? []) : ["rename", name, to])
    }

    @objc func ignoreVoice(_ sender: NSButton) {
        guard let v = voice(sender), let name = v["name"] as? String else { return }
        if v["kind"] as? String == "unnamed" { return changeVoices(["ignore"] + (v["ids"] as? [String] ?? [])) }
        guard confirm("Ignore \(name)?", "Their lines stop being a person and their voice stops being transcribed.", "Ignore") else { return }
        changeVoices(["rename", name, ignoreTag])
    }

    @objc func forgetVoice(_ sender: NSButton) {
        guard let v = voice(sender), let name = v["name"] as? String else { return }
        let ignored = isIgnored(name)
        guard confirm(ignored ? "Stop ignoring \(name)?" : "Forget \(name) on this Mac?",
                      ignored ? "Their speech is transcribed again from now on." : "Clears every tag of \(name) here. Other Macs keep theirs.",
                      ignored ? "Stop ignoring" : "Forget") else { return }
        changeVoices(["forget", name])
    }

    @objc func showVoiceLine(_ sender: NSButton) {
        guard let id = sender.identifier?.rawValue else { return }
        if !popover.isShown { clicked() }
        jump(to: id)
    }

    @objc func showTimebar() {
        if timebarWindow == nil {
            let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 900, height: 420), styleMask: [.titled, .closable, .resizable],
                             backing: .buffered, defer: false)
            w.title = "Ozen Timebar"
            w.isReleasedWhenClosed = false
            w.contentView = timebarView
            timebarView.navigationDelegate = self
            // Bundled by `ozen app`; a checkout run (Ozen [dir]) falls back to the repo copy.
            let page = Bundle.main.url(forResource: "chunks", withExtension: "html") ?? dir.appendingPathComponent("chunks.html")
            timebarView.loadFileURL(page, allowingReadAccessTo: page.deletingLastPathComponent())
            timebarWindow = w
            w.center()
        }
        timebarWindow?.makeKeyAndOrderFront(nil)
        NSApp.activate()
        refreshTimebar()
        timebarTimer?.invalidate()
        timebarTimer = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { [weak self] t in
            guard let self, self.timebarWindow?.isVisible == true else { return t.invalidate() }  // closed: stop polling
            self.refreshTimebar()
        }
    }

    func refreshTimebar() {
        ozen("timebar") { self.timebarView.evaluateJavaScript("show(\($0))") }  // before the page loads this is a no-op
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
            placesMap.customUserAgent = "Ozen (https://github.com/ozenhq/ozen)"  // OSM tile policy: identify the app
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
        let here = self.here.flatMap { try? enc.encode(Here(lat: $0.lat, lon: $0.lon)) }
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

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        webView === timebarView ? refreshTimebar() : showPlacesOnMap()
    }

    func editPlaces(_ change: (inout [Place]) -> Void, fit: Bool = true) {
        var places = loadPlaces()
        change(&places)
        savePlaces(places)
        askLocation()
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
        askLocation()
        let row = sender.tag
        settingPlace = row
        buildPlaces()
        run(["places", "here", String(row)]) { _, err, code in  // Rust gets the fix and saves it (src/places.rs)
            self.settingPlace = nil
            if code != 0 { self.placesNote.stringValue = err }
            self.lastWanted = nil  // a changed place applies right away
            self.buildPlaces()
            self.refreshState()
        }
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
            let lines = out.split(separator: "\n").map(String.init)  // files written, then the folder
            guard code == 0, let folder = lines.last else { return self.fail("Couldn't gather the transcripts", err) }
            let files = lines.dropLast()
            alert.messageText = "\(files.count) transcript\(files.count == 1 ? "" : "s") ready"
            alert.informativeText = files.joined(separator: "\n") + (kev ? "\n\nKev's scores:\n" + err : "") + "\n\n" + folder
            for b in ["Claude Code", "Hermes", "Show in Finder"] { alert.addButton(withTitle: b) }
            let what = [NSApplication.ModalResponse.alertFirstButtonReturn: "claude", .alertSecondButtonReturn: "hermes"][alert.runModal()] ?? "finder"
            self.run(["open", folder, what]) { _, err, code in if code != 0 { self.fail("Couldn't open \(what)", err) } }
        }
    }

    func fail(_ title: String, _ detail: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = detail
        NSApp.activate()
        alert.runModal()
    }

    // MARK: ask about the meeting happening now

    @objc func askMenu(_ sender: NSButton) {
        let menu = NSMenu()
        for (title, tool) in [("Claude Code", "claude"), ("Hermes", "hermes")] {
            let mi = NSMenuItem(title: title, action: #selector(askNow(_:)), keyEquivalent: "")
            mi.target = self
            mi.representedObject = tool
            menu.addItem(mi)
        }
        menu.popUp(positioning: nil, at: NSPoint(x: 0, y: sender.bounds.height + 4), in: sender)
    }

    @objc func askNow(_ sender: NSMenuItem) {
        guard let tool = sender.representedObject as? String else { return }
        askButton.isEnabled = false
        run(["live", "--open", tool]) { _, err, code in
            self.askButton.isEnabled = true
            if code != 0 { self.fail("Couldn't start \(sender.title) on this meeting", err) }
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
        let junk = Set(json("junk.json") as? [String] ?? [])  // old lines the transcriber's filters now drop
        let fmt = DateFormatter()
        fmt.dateFormat = "HH:mm:ss"
        // Call and mic chunks finish transcribing at different times, so file order isn't time order.
        return raw.split(separator: "\n").suffix(limit).compactMap { row -> (Double, Line)? in
            guard let r = try? JSONSerialization.jsonObject(with: Data(row.utf8)) as? [String: Any],
                  let id = r["id"] as? String, let t = r["t"] as? Double, !junk.contains(id) else { return nil }
            let text = r["text"] as? String ?? ""
            let d = r["d"] as? Double ?? min(15, max(1, Double(text.count) / 14))  // older lines: estimate from length
            return (t, Line(id: id, time: fmt.string(from: Date(timeIntervalSince1970: t)), t: t, d: d, spk: r["spk"] as? String ?? "?",
                            src: r["src"] as? String ?? "", text: r["text"] as? String ?? "", run: r["run"] as? Int,
                            doubt: r["doubt"] as? Double))
        }.sorted { $0.0 < $1.0 }.map(\.1)
    }

    func reload() {
        guard popover.isShown else { return }
        let files = ["lines.jsonl", "tags.json", "labels.json", "stats.json", "fixes.json", "junk.json"]
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
        // Untagged lines ozen isn't sure who said, most uncertain first (src/panel.rs).
        let unsure = (cli("unsure") as? [[String: Any]] ?? []).compactMap { u -> (id: String, until: Double)? in
            guard let id = u["id"] as? String, pending[id]?.isEmpty ?? true else { return nil }
            return (id, u["until"] as? Double ?? 0)
        }
        let isUnsure = Set(unsure.map(\.id))
        let out = NSMutableAttributedString()
        headerRanges = [:]
        let history = lines(limit: 5000)  // timeline spans more than the transcript shows
        let all = Array(history.suffix(maxLines))
        let atEnd = timelineScroll.contentView.bounds.maxX >= timeline.bounds.width - 20
        timeline.segments = history.compactMap { l in
            let tagged = !(tags[l.id] ?? "").isEmpty
            let guess = labels[l.id]
            let speaker = tagged ? tags[l.id]! : ((guess?["spk"] as? String) ?? l.spk)
            return isIgnored(speaker) ? nil : Segment(id: l.id, t: l.t, d: l.d, speaker: speaker,
                                                        text: l.text, unsure: !tagged && isUnsure.contains(l.id))
        }
        shown = []
        if atEnd { scrollTimelineToEnd() }
        if all.isEmpty { out.append(NSAttributedString(string: "No transcript yet. Press Start.", attributes: [.foregroundColor: NSColor.secondaryLabelColor])) }
        for l in all {
            let tagged = !(tags[l.id] ?? "").isEmpty
            let guess = labels[l.id]
            let unsure = !tagged && isUnsure.contains(l.id)
            let speaker = tagged ? tags[l.id]! : ((guess?["spk"] as? String) ?? l.spk)
            let said = fixes[l.id].flatMap { $0.isEmpty ? nil : $0 } ?? l.text
            shown.insert(l.id)
            let ignored = isIgnored(speaker)
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
        reviewQueue = unsure.filter { shown.contains($0.id) }
        refreshReview()
        let ignoredLines = (s["ignored"] as? Int ?? 0) > 0 ? " · \(s["ignored"]!) ignored" : ""
        footer.stringValue = "  \(acc) · \(tagged) tagged\(ignoredLines) · orange ? = unsure, tag it to teach ozen · click text to fix it"
    }

    // Only ask about recent lines: after 10 minutes nobody remembers who said what.
    func refreshReview() {
        let now = Date().timeIntervalSince1970
        review = reviewQueue.filter { $0.until >= now }.map(\.id)
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
        // `ozen tag-menu` (src/panel.rs) decides the entries: people, a new person, ignoring (a new voice, one
        // already ignored, or every line of this run's unnamed speaker) and clearing the tag.
        for e in cli("tag-menu", id) as? [[String: Any]] ?? [] {
            let title = e["title"] as? String ?? "", arg = e["arg"]
            switch e["action"] as? String {
            case "separator": menu.addItem(.separator()); continue
            case "new": menu.addItem(menuItem(title, #selector(newPerson(_:)), id))
            case "ignore": menu.addItem(menuItem(title, #selector(ignoreAll(_:)), arg as? [String] ?? [id]))
            default: menu.addItem(menuItem(title, #selector(pick(_:)), [id, arg as? String ?? ""]))
            }
        }
        return menu
    }

    func menuItem(_ title: String, _ action: Selector, _ object: Any) -> NSMenuItem {
        let mi = NSMenuItem(title: title, action: action, keyEquivalent: "")
        mi.target = self
        mi.representedObject = object
        return mi
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
