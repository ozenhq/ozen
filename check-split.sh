#!/bin/sh
# Check the Advanced split toggle: builds menubar.swift without its app.run() block and opens its panel on a
# folder with two queued chunks (no recording). Off by default: Start and Pause, no Process. On: Record,
# no Pause, and Process with the queue count. The toggle lives in this check binary's own defaults.
set -e
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"; defaults delete check-split split 2>/dev/null || true' EXIT
sed '/^let app = NSApplication.shared/,$d' menubar.swift > "$tmp/check.swift"
cat >> "$tmp/check.swift" <<'EOF'
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let a = App()
app.delegate = a
DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
    a.clicked()
    a.show(state: "stopped")
    precondition(!a.split && a.processButton.isHidden && !a.pauseButton.isHidden && a.startButton.title == "Start")
    a.showAdvanced()
    let box = a.advancedWindow!.contentView!.subviews.compactMap { $0 as? NSButton }.first!
    precondition(box.state == .off, "split is off by default")
    box.performClick(nil)
    precondition(a.split && !a.processButton.isHidden && a.pauseButton.isHidden && a.startButton.title == "Record")
    precondition(a.processButton.title == "Process 2" && a.processButton.isEnabled, a.processButton.title)
    a.show(state: "processing")
    precondition(a.status.stringValue == "Not recording · processing 2 chunks…" && a.startButton.isEnabled, a.status.stringValue)
    box.performClick(nil)
    precondition(!a.split && a.processButton.isHidden && a.startButton.title == "Start")
    print("ok")
    exit(0)
}
app.run()
EOF
swiftc -o "$tmp/check-split" "$tmp/check.swift"
mkdir "$tmp/chunks"
touch "$tmp/chunks/1-call.wav" "$tmp/chunks/1-mic.wav"
"$tmp/check-split" "$tmp" | grep -x ok
