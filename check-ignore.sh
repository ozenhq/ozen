#!/bin/sh
# Check that ignored voices stay apart: builds menubar.swift without its app.run() block, then on sample tags
# the tag menu offers a new ignored voice plus each one already ignored, the Voices window gives each its own
# Stop ignoring, and the transcript dims both.
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
    let titles = a.tagMenu(for: "dana").items.map(\.title).filter { $0.hasPrefix("Ignore") || $0.hasPrefix("Same") }
    precondition(titles == ["Ignore this voice", "Same voice as Ignored", "Same voice as Ignored 2"], "\(titles)")
    precondition(!a.knownNames().contains { isIgnored($0) }, "\(a.knownNames())")
    a.voices = [["name": "Ignored", "kind": "ignored", "lines": 1], ["name": "Ignored 2", "kind": "ignored", "lines": 1]]
    a.buildVoices()
    let rows = a.voicesStack.arrangedSubviews.compactMap { $0 as? NSStackView }.map { row in
        row.views.compactMap { ($0 as? NSButton).map { "\($0.identifier!.rawValue): \($0.title)" } } }
    precondition(rows == [["Ignored: Stop ignoring…"], ["Ignored 2: Stop ignoring…"]], "\(rows)")
    precondition(isIgnored("Ignored 12") && !isIgnored("Ignored TV") && !isIgnored("Dana"))
    print("ok")
    exit(0)
}
app.run()
EOF
swiftc -o "$tmp/check" "$tmp/check.swift"
now=$(date +%s)
for id in dana tv radio; do echo "{\"id\": \"$id\", \"t\": $now, \"spk\": \"S1\", \"src\": \"room\", \"text\": \"hi\"}"; done > "$tmp/lines.jsonl"
echo '{"dana": "Dana", "tv": "Ignored", "radio": "Ignored 2"}' > "$tmp/tags.json"
"$tmp/check" "$tmp" | grep -x ok
