// Menu bar ear icon: left-click shows the live transcript, right-click offers Quit.
// Usage: ozen-bar [dir containing transcript.txt] [--open]
import AppKit

let args = CommandLine.arguments.dropFirst().filter { !$0.hasPrefix("--") }
let dir = URL(fileURLWithPath: args.first ?? FileManager.default.currentDirectoryPath)
let transcriptURL = dir.appendingPathComponent("transcript.txt")
let maxLines = 400

final class App: NSObject, NSApplicationDelegate {
    let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
    let popover = NSPopover()
    let scroll = NSTextView.scrollableTextView()
    var text: NSTextView { scroll.documentView as! NSTextView }
    var lastSize = -1

    func applicationDidFinishLaunching(_ n: Notification) {
        let button = item.button!
        button.image = NSImage(systemSymbolName: "ear", accessibilityDescription: "ozen transcript")
        button.target = self
        button.action = #selector(clicked)
        button.sendAction(on: [.leftMouseUp, .rightMouseUp])

        scroll.frame = NSRect(x: 0, y: 0, width: 560, height: 640)
        text.isEditable = false
        text.textContainerInset = NSSize(width: 10, height: 10)
        let vc = NSViewController()
        vc.view = scroll
        popover.contentViewController = vc
        popover.behavior = .transient

        Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in self?.reload() }
        if CommandLine.arguments.contains("--open") { DispatchQueue.main.asyncAfter(deadline: .now() + 1) { self.clicked() } }
    }

    @objc func clicked() {
        if NSApp.currentEvent?.type == .rightMouseUp {
            let menu = NSMenu()
            menu.addItem(withTitle: "Quit ozen bar", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
            item.menu = menu
            item.button?.performClick(nil)  // shows the menu
            item.menu = nil  // keep left-click for the popover
            return
        }
        if popover.isShown {
            popover.performClose(nil)
        } else {
            lastSize = -1
            reload()
            popover.show(relativeTo: item.button!.bounds, of: item.button!, preferredEdge: .minY)
            NSApp.activate()
            text.scrollToEndOfDocument(nil)
        }
    }

    func reload() {
        guard popover.isShown || lastSize == -1 else { return }
        let data = (try? Data(contentsOf: transcriptURL)) ?? Data()
        guard data.count != lastSize else { return }
        lastSize = data.count
        let atBottom = scroll.verticalScroller.map { $0.floatValue > 0.98 } ?? true

        let lines = String(decoding: data, as: UTF8.self).split(separator: "\n").suffix(maxLines)
        let out = NSMutableAttributedString()
        for line in lines.isEmpty ? ["No transcript yet. Start capture with ./start.sh"] : Array(lines) {
            // "[hh:mm:ss] Speaker (source): text" -> bold header, text RTL when it is Hebrew
            let s = String(line)
            let split = s.range(of: "): ")
            let header = split.map { String(s[..<$0.upperBound]) } ?? ""
            let body = split.map { String(s[$0.upperBound...]) } ?? s
            let para = NSMutableParagraphStyle()
            para.paragraphSpacing = 6
            if body.unicodeScalars.contains(where: { (0x0590...0x05FF).contains($0.value) }) {
                para.baseWritingDirection = .rightToLeft
                para.alignment = .right
            }
            let base: [NSAttributedString.Key: Any] = [.paragraphStyle: para, .foregroundColor: NSColor.labelColor]
            out.append(NSAttributedString(string: header, attributes: base.merging([
                .font: NSFont.boldSystemFont(ofSize: 12), .foregroundColor: NSColor.secondaryLabelColor,
            ]) { $1 }))
            out.append(NSAttributedString(string: body + "\n", attributes: base.merging([.font: NSFont.systemFont(ofSize: 13)]) { $1 }))
        }
        text.textStorage?.setAttributedString(out)
        if atBottom { text.scrollToEndOfDocument(nil) }
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)  // menu bar only, no Dock icon
let delegate = App()
app.delegate = delegate
app.run()
