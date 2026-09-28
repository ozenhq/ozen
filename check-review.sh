#!/bin/sh
# Check what Review asks about: builds menubar.swift without its app.run() block and opens its panel on sample
# lines (no recording). Unsure lines queue most uncertain first, whether a retrain labeled them or only the
# transcriber has seen them, and only for the last 10 minutes.
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
    a.clicked()
    precondition(a.review == ["new", "labeled", "edge"] && a.reviewButton.title == "Review 3", "\(a.review)")
    DispatchQueue.main.asyncAfter(deadline: .now() + 4) {  // no file changes: "edge" must still age out
        precondition(a.review == ["new", "labeled"] && a.reviewButton.title == "Review 2", "\(a.review)")
        print("ok")
        exit(0)
    }
}
app.run()
EOF
swiftc -o "$tmp/check" "$tmp/check.swift"
now=$(date +%s)
line() { echo "{\"id\": \"$1\", \"t\": $(($now - $2)), \"spk\": \"S1\", \"src\": \"room\", \"text\": \"hi\"$3}"; }
{
    line old 1200 ', "doubt": 0.01'   # unsure but too old to remember
    line edge 597 ', "doubt": 0.05'   # unsure, ages out during the check
    line new 30 ', "doubt": 0.02'     # unsure, not retrained yet
    line sure 20 ', "doubt": 0.3'     # the transcriber is sure
    line labeled 10 ''                # retrain's verdict: unsure (0.4 - 0.37 = 0.03)
    line relabeled 5 ', "doubt": 0.01' # a retrain since found it sure: its verdict wins
} > "$tmp/lines.jsonl"
echo '{"labeled": {"spk": "Dana", "sim": 0.37, "margin": 0.2, "unsure": true},
       "relabeled": {"spk": "Dana", "sim": 0.9, "margin": 0.5, "unsure": false}}' > "$tmp/labels.json"
echo '{"threshold": 0.4}' > "$tmp/stats.json"
"$tmp/check" "$tmp" | grep -x ok
