#!/bin/bash
# ozen control: start | pause | resume | stop | status | app | bar
#   start/resume  record + transcribe (builds binaries if sources changed)
#   pause         stop recording; transcriber stays loaded so resume is instant
#   stop          stop recording, finish transcribing what's queued, then exit
#   status        prints recording | paused | stopping | stopped
#   app           build Ozen.app into ~/Applications (open it from Spotlight/Launchpad)
#   bar           build if needed and open Ozen.app (its buttons call this script)
cd "$(dirname "$0")"
REC='^\./rec chunks'  # anchored so pgrep never matches shells that merely mention the command
TR='uv run transcribe\.py chunks|python3 transcribe\.py chunks'
APP="$HOME/Applications/Ozen.app"

build() {
    [ rec -nt rec.swift ] || swiftc -O rec.swift -o rec
}

build_app() {
    local bin="$APP/Contents/MacOS/Ozen"
    [ "$bin" -nt menubar.swift ] && [ "$bin" -nt icon.swift ] && return
    mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
    swiftc -O menubar.swift -o "$bin" || return 1
    swift icon.swift "$APP/Contents/Resources" || echo "icon build failed; app still works" >&2
    cat >"$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>Ozen</string>
  <key>CFBundleDisplayName</key><string>Ozen</string>
  <key>CFBundleIdentifier</key><string>com.tupe12334.ozen</string>
  <key>CFBundleExecutable</key><string>Ozen</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundleShortVersionString</key><string>$(git rev-parse --short HEAD 2>/dev/null || echo dev)</string>
  <key>LSMinimumSystemVersion</key><string>15.0</string>
  <key>LSUIElement</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Ozen transcribes what you say in meetings, on this Mac only.</string>
  <key>NSAudioCaptureUsageDescription</key><string>Ozen transcribes the meeting audio, on this Mac only.</string>
</dict></plist>
PLIST
    codesign --force --deep -s - "$APP" 2>/dev/null  # ad-hoc: required for macOS to grant it permissions
    touch "$APP"  # refresh Finder/Spotlight
}

case "$1" in
start | resume)
    build || exit 1
    mkdir -p chunks
    if ! pgrep -qf "$TR"; then
        git -C voices pull -q --ff-only 2>/dev/null || echo "voices registry pull failed; using local copy" >>start.log
        nohup uv run transcribe.py chunks transcript.txt >>start.log 2>&1 </dev/null &
    fi
    pgrep -qf "$REC" || nohup ./rec chunks >>start.log 2>&1 </dev/null &
    ;;
pause)
    pkill -INT -f "$REC"  # SIGINT: recorder flushes its current chunk first
    ;;
stop)
    pkill -INT -f "$REC"
    touch .stopping
    # Let the transcriber drain queued chunks (max 2 min) so the last words aren't lost.
    (for _ in $(seq 120); do ls chunks/*.wav >/dev/null 2>&1 || break; sleep 1; done
     pkill -f "$TR"; rm -f .stopping) >/dev/null 2>&1 </dev/null &
    ;;
status)
    if pgrep -qf "$REC"; then echo recording
    elif [ -e .stopping ] && pgrep -qf "$TR"; then echo stopping
    elif pgrep -qf "$TR"; then echo paused
    else rm -f .stopping; echo stopped
    fi
    ;;
app)
    build_app && echo "installed $APP"
    ;;
bar)
    build_app || exit 1
    open "$APP"
    ;;
*)
    sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
