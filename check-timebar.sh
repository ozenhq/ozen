#!/bin/sh
# Check the Timebar window: builds menubar.swift without its app.run() block, presses Timebar… in the panel, and
# checks the page drew what `ozen timebar` returned. A stub stands in for the ozen CLI, so nothing records.
set -e
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed '/^let app = NSApplication.shared/,$d' menubar.swift > "$tmp/check.swift"
cat >> "$tmp/check.swift" <<'EOF'
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let a = App()
app.delegate = a
DispatchQueue.main.asyncAfter(deadline: .now() + 1) {
    a.timebarButton.performClick(nil)
    precondition(a.timebarWindow?.isVisible == true, "Timebar… didn't open its window")
    // Wait for the page to load and be fed `ozen timebar` (slow on a busy machine), then check what it drew.
    let deadline = Date().addingTimeInterval(30)
    Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { _ in
        a.timebarView.evaluateJavaScript("[...document.querySelectorAll('.stat b')].slice(0, 4).map(b => b.textContent).join()") { r, _ in
            let stats = r as? String ?? ""  // recorded, done, waiting, skipped
            if stats == "3,1,1,1" { print("ok"); exit(0) }
            precondition(Date() < deadline, "stats: \(stats)")
        }
    }
}
app.run()
EOF
cp chunks.html "$tmp/"
swiftc -o "$tmp/check" "$tmp/check.swift"
mkdir -p "$tmp/target/release"
now=$(date +%s)
cat > "$tmp/timebar.json" <<EOF
{"now": $now, "chunks": [
  {"t": $((now - 100)), "tag": "mic", "state": "done", "sec": 15, "done": $((now - 40)), "took": 3, "lines": 1},
  {"t": $((now - 60)), "tag": "call", "state": "error", "sec": 0, "done": $((now - 20)), "took": 0.1, "lines": 0, "error": "bad wav"},
  {"t": $((now - 30)), "tag": "local", "state": "waiting"}]}
EOF
printf '#!/bin/sh\ncase "$1" in timebar) cat "%s/timebar.json" ;; status) echo stopped ;; esac\n' "$tmp" > "$tmp/target/release/ozen"
chmod +x "$tmp/target/release/ozen"
"$tmp/check" "$tmp" | grep -x ok
