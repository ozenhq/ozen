#!/bin/sh
# Two ozen sandboxes syncing through a local relay, for trying sync by hand (OFE-58).
#   scripts/sync-dev.sh         run until Ctrl-C; edit one sandbox's files and watch the other
#   scripts/sync-dev.sh --demo  tag a line in A, wait for it in B (at most 5 s), exit 0 or 1; it may
#                               arrive through the relay or through Bonjour, whichever is first
# Needs the relay's checkout: OZEN_SYNC_REPO, default ../sync next to this one. The sandboxes use a
# throwaway vault key (OZEN_SYNC_KEY_FILE, honoured by debug builds only): never the Keychain's, so
# they can't join the vault of your real Macs. Both sync through the relay and, on this network, Bonjour.
set -eu
here=$(cd "$(dirname "$0")/.." && pwd)
relay_repo=${OZEN_SYNC_REPO:-$here/../sync}
[ -f "$relay_repo/Cargo.toml" ] || { echo "no relay checkout at $relay_repo (set OZEN_SYNC_REPO)" >&2; exit 2; }

echo "building ozen (debug) and the relay..."
(cd "$here" && cargo build -q)
(cd "$relay_repo" && cargo build -q --release)
ozen=$here/target/debug/ozen
relay_bin=$relay_repo/target/release/ozen-sync

dev=$(mktemp -d "${TMPDIR:-/tmp}/ozen-sync-dev.XXXXXX")
pids=
cleanup() {
    for p in $pids; do kill "$p" 2>/dev/null || true; done
    wait 2>/dev/null || true
    rm -rf "$dev"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

head -c 32 /dev/urandom > "$dev/key"
BIND=127.0.0.1:0 "$relay_bin" > "$dev/relay.log" 2>&1 &
pids="$pids $!"
port=
tries=0
while [ -z "$port" ] && [ $tries -lt 50 ]; do
    sleep 0.1
    port=$(sed -n 's/.*listening on 127\.0\.0\.1:\([0-9]*\).*/\1/p' "$dev/relay.log")
    tries=$((tries + 1))
done
[ -n "$port" ] || { cat "$dev/relay.log" >&2; echo "the relay didn't start" >&2; exit 1; }
url=ws://127.0.0.1:$port

for m in a b; do
    mkdir "$dev/$m"
    : > "$dev/$m/.sync-on" # what `ozen sync init` writes
    printf '{"id":"1@a","t":1.0,"text":"hello from a","v":1}\n' > "$dev/$m/lines.jsonl"
    # `ozen sync run` keeps going while someone asks (Ozen.app's status poll does): ask every 2 s
    (while :; do touch "$dev/$m/.sync-asked"; sleep 2; done) &
    pids="$pids $!"
    OZEN_DIR=$dev/$m OZEN_SYNC_URL=$url OZEN_SYNC_KEY_FILE=$dev/key "$ozen" sync run > "$dev/$m.log" 2>&1 &
    pids="$pids $!"
done

if [ "${1:-}" = --demo ]; then
    # both on the relay and seeing each other (link.rs writes "online": 2 to .sync-link.json)
    tries=0
    until [ "$(grep -l '"online":2' "$dev"/[ab]/.sync-link.json 2>/dev/null | wc -l)" -eq 2 ]; do
        [ $tries -ge 300 ] && { echo "demo: the sandboxes never both reached the relay" >&2; exit 1; }
        sleep 0.1
        tries=$((tries + 1))
    done
    start=$(date +%s)
    printf '{"1@a":{"v":5,"val":"Dana"}}' > "$dev/a/tags.json"
    while ! grep -q Dana "$dev/b/tags.json" 2>/dev/null; do
        if [ $(($(date +%s) - start)) -ge 5 ]; then
            echo "demo: the tag didn't reach sandbox B within 5 s" >&2
            tail -5 "$dev/a.log" "$dev/b.log" "$dev/relay.log" >&2
            exit 1
        fi
        sleep 0.1
    done
    echo "demo: the tag made in sandbox A reached sandbox B in $(($(date +%s) - start)) s"
    exit 0
fi

cat <<MSG
relay:      $url  (log: $dev/relay.log)
sandbox A:  $dev/a  (log: $dev/a.log)
sandbox B:  $dev/b  (log: $dev/b.log)
Edit A, watch B, e.g.:
  OZEN_DIR=$dev/a $ozen tag 1@a Dana     (or write $dev/a/tags.json)
  cat $dev/b/tags.json
Ctrl-C stops everything and deletes the sandboxes.
MSG
while :; do sleep 1; done
