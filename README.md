# ozen

Local meeting copilot for macOS (אוזן, "ear"). While you're in a Zoom / Meet / Teams call, ozen
listens and transcribes the call, knows who is speaking, and lets an assistant (Claude Code in a
terminal) see your screen and answer questions about the meeting as it happens.
Everything runs on your Mac: no bot joins the meeting and no audio leaves the machine.

## What it does

- **Hears the call and the room.** The recorder (`src/bin/rec.rs`, run as `target/recorder/ozen` so macOS
  lists it as ozen) uses ScreenCaptureKit to record three streams in 15s chunks:
  - `call`: audio from meeting apps only (Zoom, Chrome, Teams, Slack, FaceTime, Discord)
  - `mic`: your microphone
  - `local`: every other app, e.g. macOS Speak Selection reading text aloud. Never transcribed; it only
    tells the transcriber when the computer itself is talking.
- **Transcribes Hebrew and English.** On-device Whisper (MLX) per utterance, language picked between `he` and `en`
  for each one: Hebrew goes to [ivrit.ai's Hebrew-trained turbo](https://huggingface.co/mlx-community/ivrit-ai-whisper-large-v3-turbo-mlx),
  English to stock large-v3-turbo. Each call is hinted with `vocab.txt` (terms and names to spell right, e.g. Kev,
  PR, code review; edit freely), the known people's names, and the previous line. Known filler that Whisper invents
  on noise ("Thank you.", "תודה רבה") is dropped.
- **Drops echo.** A mic utterance that mostly overlaps call or local audio is speaker bleed, not a person in
  the room, so it's discarded. That covers the computer reading text aloud and remote voices leaking into the mic.
- **Knows who's speaking.** Each utterance gets an ECAPA voiceprint (speechbrain) and is matched against voices
  already heard, so a person keeps one label for the whole meeting. Known people come from the
  [voices registry](https://github.com/tupe12334/voices-embedding-registry) and show by name.
- **Learns from your tags.** Click any speaker name in the menu bar panel to set who really said that line.
  Each tag runs `train.py`: every person's voiceprint becomes the average of all lines tagged as them
  (stored in the registry, so tags accumulate across meetings), untagged lines are relabeled with the new
  prints, and the live transcriber reloads them.
- **Improves itself: the loop.** Every retrain also
  1. calibrates the same-voice threshold from your tags (the cutoff that best separates same-person from
     different-person similarities), which the live transcriber picks up immediately;
  2. scores every untagged line and queues the ones it's least sure about (near the threshold, or nearly tied
     between two people). They show an orange **?**, and **Review N** jumps to the most uncertain one and asks
     who said it;
  3. logs leave-one-out accuracy to the registry's `history.jsonl`; the footer shows it with the starting value.

  Answering Review is the fastest way to improve it: in a simulation with four similar voices, 12 tags picked by
  Review got untagged-line accuracy to 100% with 2 lines still unsure, versus 98% with 35 unsure for 12 random tags.

- **Learns from your text fixes.** Click a line's text in the panel to correct what was said (empty restores it).
  `ozen fix` keeps the fix in `fixes.json` and relearns `learned.json`, which the live transcriber reads:
  words your fixes add go into Whisper's hint (up to 30, most used first), and a correction you make twice
  (e.g. "פי אר" → "PR") is applied to new lines automatically (the line keeps Whisper's own text as `heard`, which later fixes learn
  from, so undoing a wrong correction cancels it). A correction is skipped while any fix keeps that phrase as right. Each fix also keeps its chunk audio in `fixes/`
  with the right text (`fixes/dataset.jsonl`), ready for fine-tuning a model later.

- **Runs from the menu bar.** `ozen-bar` puts an ear icon in the top menu bar: click it for the live transcript
  (updates every 2s, Hebrew lines right-to-left) with **Start / Pause / Resume / Stop** buttons; right-click for
  the same controls. The icon shows the state: ear (stopped), filled ear (recording), pause (paused).
- **Timeline view.** Switch the panel to **Timeline** for one lane per speaker (with their total talk time) and a
  bar for every line they spoke, on a scrollable time axis; **− / +** zoom. Silences over 2 minutes shrink to a
  short break marker. Hover a bar for what was said; click it to jump to that line in the transcript.
- **Records always, or only meetings.** The **Always / Meetings** toggle in the panel (and right-click menu):
  - *Always*: records until you pause or stop, including the room mic and any audio from meeting apps.
  - *Meetings*: starts by itself when a meeting app (Zoom, Chrome/Meet, Teams, Slack, FaceTime, Discord) is using
    the microphone, and stops 20s after it releases it. Detected through Core Audio's per-process state, so it
    works for any call in those apps. Pausing or stopping by hand holds until the next meeting starts or ends.

- **Hands meetings to an agent.** Switch the panel to **Meetings** for every past meeting (a silence of 10+ minutes
  starts a new one). Select one or more (⌘/⇧-click) and press **Open** to put their transcripts in a fresh folder
  under `context/`, then start Claude Code or Hermes there in a new Terminal window; the folder's `AGENTS.md` (and
  `CLAUDE.md`) tells the agent what it holds, and its `claude.command` / `hermes.command` reopen it with a double-click. **Auto add with Kev** also adds every other meeting that local [Kev](https://github.com/jaredpalmer/kev)
  (`localhost:8009`) judges part of the same project or topic; its scores show before you pick the agent.

Output is `transcript.txt`:

```
[15:06:52] Ofek Gabay (room): אימבדין שלי, אני לא מוצא את זה
[15:09:41] S2 (call): Let's move to the launch timeline.
```

## Setup

Requires macOS 15+ on Apple Silicon, Xcode command line tools (`swiftc`), [Rust](https://rustup.rs) (`cargo`), [`uv`](https://docs.astral.sh/uv/) and `ffmpeg`.

```sh
git clone https://github.com/tupe12334/ozen ~/ozen
git clone https://github.com/tupe12334/voices-embedding-registry ~/ozen/voices
cd ~/ozen && cargo run --release -- app   # builds the ozen CLI and ~/Applications/Ozen.app
```

Then open **Ozen** from Spotlight, Launchpad or Finder like any app. It lives in the menu bar (no Dock icon);
press Start there. After pulling new code, run `cargo run --release -- app` again to rebuild both.

On the first Start, macOS asks **Ozen** for **Screen & System Audio Recording** and **Microphone** access;
grant both (System Settings > Privacy & Security), then press Start again. The build signs the app and recorder
with a local self-signed certificate (created once in `~/Library/Keychains/ozen-signing.keychain-db`), so the
permissions survive rebuilds. The Whisper and ECAPA models download on first use.

## Use

| Command | What it does |
|---|---|
| `target/release/ozen start` / `pause` / `resume` / `stop` | Same as the menu bar buttons. Pause keeps the transcriber loaded; stop finishes transcribing queued audio first |
| `target/release/ozen status` | `recording`, `paused`, `stopping` or `stopped` |
| `target/release/ozen health` | One line per problem the menu bar warns about: recording blocked, silent mic, transcriber down or behind |
| `target/release/ozen fix <line-id> "right text"` | Correct a line's text (what the panel does); empty clears. Relearns hint words and corrections |
| `target/release/ozen meetings` | Past meetings, newest first: id, start, minutes, lines, first words |
| `target/release/ozen gather [--kev] ID...` | Write those meetings into `context/<now>/` and print the folder; `--kev` adds the ones Kev judges related |
| `target/release/ozen app` | Build and install `~/Applications/Ozen.app` |
| `target/release/ozen bar` | Build if needed and open Ozen.app |
| `target/release/ozen look [N]` | Screenshot to `screen-small.png` and print the last N transcript lines (tagged speakers, fixed text). Use it to answer "what's on screen / what was just said" |
| `uv run train.py tag <line-id> "Dana Levi"` | Tag a line (what the panel does); empty name clears. Retrains and pushes the registry |
| `uv run train.py retrain` | Rebuild voiceprints, relabels and accuracy from all tags |
| `uv run eval.py [N]` | Transcribe the last N real chunks (kept in `recent/`, 20 max, local only) with stock vs Hebrew vs Hebrew+vocab, to judge changes on your own speech. `OZEN_KEEP_AUDIO=0` keeps none |
| `uv run train.py show [N]` | Last N lines with tag-corrected speakers |

## Limits

- Lines arrive ~15–30s after speech (chunked, not streaming).
- English terms spoken inside Hebrew are the weakest spot; add them to `vocab.txt`. Distant voices in the room are hard to hear.
- You talking over the computer voice or a remote speaker can be dropped as echo.
- Overlapping speakers are merged into one line; utterances under 1s inherit the previous speaker.
- Live speaker matching is online (no re-clustering); tagging a few lines fixes past and future labels.

## Privacy

In *Always* mode the mic records everything said near the Mac, not only meetings; use *Meetings* mode to limit it.
This records and transcribes other people. Tell participants, and follow your local recording laws.
Voiceprints are biometric data: keep the registry private and enroll only people who agreed.
