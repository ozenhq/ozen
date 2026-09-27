#!/bin/bash
# Snapshot for answering a question mid-meeting: screen image + recent transcript.
cd "$(dirname "$0")"
screencapture -x -D1 screen.png && sips -Z 1280 screen.png --out screen-small.png >/dev/null
echo "screen: $PWD/screen-small.png"
uv run -q train.py show "${1:-40}"  # labels corrected by your tags
