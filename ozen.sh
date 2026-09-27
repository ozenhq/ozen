#!/bin/bash
# ozen control: start | pause | resume | stop | status | bar
#   start/resume  record + transcribe (builds binaries if sources changed)
#   pause         stop recording; transcriber stays loaded so resume is instant
#   stop          stop recording, finish transcribing what's queued, then exit
#   status        prints recording | paused | stopping | stopped
#   bar           launch the menu bar app (which calls this script for its buttons)
cd "$(dirname "$0")"
REC='^\./rec chunks'  # anchored so pgrep never matches shells that merely mention the command
TR='uv run transcribe\.py chunks|python3 transcribe\.py chunks'

build() {
    [ rec -nt rec.swift ] || swiftc -O rec.swift -o rec
    [ ozen-bar -nt menubar.swift ] || swiftc -O menubar.swift -o ozen-bar
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
bar)
    build || exit 1
    pgrep -qx ozen-bar || nohup ./ozen-bar "$PWD" >/dev/null 2>&1 </dev/null &
    ;;
*)
    sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
