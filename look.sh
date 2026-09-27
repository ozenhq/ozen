#!/bin/bash
# Snapshot for answering a question mid-meeting: screen image + recent transcript.
cd "$(dirname "$0")"
screencapture -x -D1 screen.png && sips -Z 1280 screen.png --out screen-small.png >/dev/null
echo "screen: $PWD/screen-small.png"
tail -n "${1:-40}" transcript.txt 2>/dev/null
