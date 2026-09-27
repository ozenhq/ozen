#!/bin/bash
# Start live meeting capture: audio recorder + Whisper transcriber. Ctrl-C stops both.
set -e
cd "$(dirname "$0")"
[ rec -nt rec.swift ] || swiftc -O rec.swift -o rec
[ ozen-bar -nt menubar.swift ] || swiftc -O menubar.swift -o ozen-bar
pgrep -x ozen-bar >/dev/null || nohup ./ozen-bar "$PWD" >/dev/null 2>&1 &  # menu bar transcript viewer; outlives this script
mkdir -p chunks
rm -f names.json session_speakers.json  # S-labels are per run; stale mappings would mislabel
git -C voices pull -q --ff-only || echo "voices registry pull failed; using local copy"
./rec chunks &
REC=$!
trap 'kill $REC 2>/dev/null' EXIT
uv run transcribe.py chunks transcript.txt
