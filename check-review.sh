#!/bin/sh
# Check that Review only asks about the last 10 minutes: builds menubar.swift without its app.run() block
# and drives refreshReview() directly, no window or recording.
set -e
cd "$(dirname "$0")"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
sed '/^let app = NSApplication.shared/,$d' menubar.swift > "$tmp/check.swift"
cat >> "$tmp/check.swift" <<'EOF'
let a = App()
let now = Date().timeIntervalSince1970
a.reviewQueue = ["old", "edge", "new"]
a.shown = ["old": ("S1", now - 1200, nil), "edge": ("S1", now - 598, nil), "new": ("S1", now - 30, nil)]
a.refreshReview()
precondition(a.review == ["edge", "new"] && a.reviewButton.title == "Review 2", "\(a.review)")
sleep(3)  // no file changes: "edge" must still age out
a.refreshReview()
precondition(a.review == ["new"] && a.reviewButton.title == "Review 1", "\(a.review)")
print("ok")
EOF
swiftc -o "$tmp/check" "$tmp/check.swift"
"$tmp/check" "$tmp"
