// Menu bar ear icon: left-click shows the live transcript with start/pause/stop controls,
// right-click offers the same controls plus Quit. Controls call ozen.sh.
// Click a speaker name in the transcript to tag who really said that line; every tag retrains
// the voiceprints (train.py), so labels improve the more you tag.
// Usage: ozen-bar [dir] [--open]
import AppKit

let args = CommandLine.arguments.dropFirst().filter { !$0.hasPrefix("--") }
let dir = URL(fileURLWithPath: args.first ?? FileManager.default.currentDirectoryPath)
let maxLines = 400

func json(_ name: String) -> Any? {
    (try? Data(contentsOf: dir.appendingPathComponent(name))).flatMap { try? JSONSerialization.jsonObject(with: $0) }
}

struct Line { let id: String, time: String, spk: String, src: String, text: String }

final class App: NSObject, NSApplicationDelegate, NSTextViewDelegate {
    let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    let popover = NSPopover()
    let scroll = NSTextView.scrollableTextView()
    let footer = NSTextField(labelWithString: "")
    var text: NSTextView { scroll.documentView as! NSTextView }
    var signature = ""
    var pending: [String: String] = [:]  // tags shown right away while train.py runs
    var state = "stopped"  // from `ozen.sh status`: recording | paused | stopping | stopped
    let status = NSTextField(labelWithString: "")
    let startButton = NSButton(title: "Start", target: nil, action: nil)
    let pauseButton = NSButton(title: "Pause", target: nil, action: nil)
    let stopButton = NSButton(title: "Stop", target: nil, action: nil)

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
        for (b, cmd) in [(startButton, #selector(startCapture)), (pauseButton, #selector(pauseCapture)), (stopButton, #selector(stopCapture))] {
            b.target = self
            b.action = cmd
            b.bezelStyle = .rounded
            b.controlSize = .small
        }
        status.font = .boldSystemFont(ofSize: 12)
        let controls = NSStackView(views: [status, NSView(), startButton, pauseButton, stopButton])
        controls.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 0, right: 12)
        let stack = NSStackView(views: [controls, scroll, footer])
        stack.orientation = .vertical
        stack.edgeInsets = NSEdgeInsets(top: 0, left: 0, bottom: 8, right: 0)
        stack.frame = NSRect(x: 0, y: 0, width: 560, height: 660)
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
            menu.addItem(.separator())
            for (title, sel, on) in [(state == "paused" ? "Resume" : "Start", #selector(startCapture), state == "stopped" || state == "paused"),
                                     ("Pause", #selector(pauseCapture), state == "recording"),
                                     ("Stop", #selector(stopCapture), state == "recording" || state == "paused")] where on {
                let mi = NSMenuItem(title: title, action: sel, keyEquivalent: "")
                mi.target = self
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

    // MARK: capture control (ozen.sh owns the processes; this only asks it)

    func ozen(_ cmd: String, done: ((String) -> Void)? = nil) {
        DispatchQueue.global().async {
            let p = Process()
            let pipe = Pipe()
            p.executableURL = URL(fileURLWithPath: "/bin/zsh")
            p.arguments = ["-lc", "cd \"$OZEN_DIR\" && ./ozen.sh \"$OZEN_CMD\""]  // login shell: uv/swiftc on PATH
            p.environment = ProcessInfo.processInfo.environment.merging(["OZEN_DIR": dir.path, "OZEN_CMD": cmd]) { $1 }
            p.standardOutput = pipe
            try? p.run()
            p.waitUntilExit()
            let out = String(decoding: pipe.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
            DispatchQueue.main.async { done?(out.trimmingCharacters(in: .whitespacesAndNewlines)) }
        }
    }

    func refreshState() {
        ozen("status") { self.show(state: $0) }
    }

    func show(state s: String) {
        state = s
        let icon = ["recording": "ear.fill", "paused": "pause.circle", "stopping": "hourglass"][s] ?? "ear"
        item.button?.image = NSImage(systemSymbolName: icon, accessibilityDescription: "ozen \(s)")
        status.stringValue = ["recording": "● Recording", "paused": "Paused", "stopping": "Finishing transcription…"][s] ?? "Stopped"
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

    func lines() -> [Line] {
        guard let raw = try? String(contentsOf: dir.appendingPathComponent("lines.jsonl"), encoding: .utf8) else { return [] }
        let fmt = DateFormatter()
        fmt.dateFormat = "HH:mm:ss"
        return raw.split(separator: "\n").suffix(maxLines).compactMap { row in
            guard let r = try? JSONSerialization.jsonObject(with: Data(row.utf8)) as? [String: Any],
                  let id = r["id"] as? String, let t = r["t"] as? Double else { return nil }
            return Line(id: id, time: fmt.string(from: Date(timeIntervalSince1970: t)), spk: r["spk"] as? String ?? "?",
                        src: r["src"] as? String ?? "", text: r["text"] as? String ?? "")
        }
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
        let labels = json("labels.json") as? [String: String] ?? [:]
        let out = NSMutableAttributedString()
        let all = lines()
        if all.isEmpty { out.append(NSAttributedString(string: "No transcript yet. Press Start.", attributes: [.foregroundColor: NSColor.secondaryLabelColor])) }
        for l in all {
            let tagged = !(tags[l.id] ?? "").isEmpty
            let speaker = tagged ? tags[l.id]! : (labels[l.id] ?? l.spk)
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
            out.append(NSAttributedString(string: speaker + (tagged ? " ✓" : ""), attributes: base.merging([
                .font: NSFont.boldSystemFont(ofSize: 12), .link: URL(string: "ozen://tag/\(l.id)")!,
            ]) { $1 }))
            out.append(NSAttributedString(string: " (\(l.src)): ", attributes: base.merging([
                .font: NSFont.systemFont(ofSize: 11), .foregroundColor: NSColor.tertiaryLabelColor,
            ]) { $1 }))
            out.append(NSAttributedString(string: l.text + "\n", attributes: base.merging([.font: NSFont.systemFont(ofSize: 13)]) { $1 }))
        }
        text.textStorage?.setAttributedString(out)
        if atBottom { text.scrollToEndOfDocument(nil) }

        let s = json("stats.json") as? [String: Any] ?? [:]
        let tagged = s["tagged"] as? Int ?? 0
        let acc = (s["accuracy"] as? Double).map { "speaker accuracy \(Int($0 * 100))% on \(s["evaluated"] ?? 0) checks · " } ?? ""
        footer.stringValue = "  \(acc)\(tagged) lines tagged · click a name to fix who said it"
    }

    // Clicking a speaker name: pick who really said the line.
    func textView(_ view: NSTextView, clickedOnLink link: Any, at index: Int) -> Bool {
        guard let url = link as? URL, url.host == "tag" else { return false }
        let id = url.lastPathComponent
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
        let clear = NSMenuItem(title: "Clear tag", action: #selector(pick(_:)), keyEquivalent: "")
        clear.target = self
        clear.representedObject = [id, ""]
        menu.addItem(clear)
        if let event = NSApp.currentEvent { NSMenu.popUpContextMenu(menu, with: event, for: view) }
        return true
    }

    func knownNames() -> [String] {
        let registry = (try? FileManager.default.contentsOfDirectory(at: dir.appendingPathComponent("voices/voices"),
                                                                     includingPropertiesForKeys: nil)) ?? []
        let fromRegistry = registry.compactMap { f in
            (try? Data(contentsOf: f)).flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] }?["name"] as? String
        }
        let fromTags = (json("tags.json") as? [String: String] ?? [:]).values.filter { !$0.isEmpty }
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

    func tag(_ id: String, _ name: String) {
        pending[id] = name
        reload()
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/bin/zsh")
        // Values go through the environment, never into the command string.
        p.arguments = ["-lc", "cd \"$OZEN_DIR\" && uv run -q train.py tag \"$OZEN_ID\" \"$OZEN_NAME\""]
        p.environment = ProcessInfo.processInfo.environment.merging(["OZEN_DIR": dir.path, "OZEN_ID": id, "OZEN_NAME": name]) { $1 }
        p.terminationHandler = { _ in
            DispatchQueue.main.async {
                self.pending[id] = nil
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
