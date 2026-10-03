# ozen

Local meeting copilot for macOS (אוזן, "ear"). While you're in a Zoom / Meet / Teams call, ozen
listens and transcribes the call, knows who is speaking, and lets an assistant (Claude Code in a
terminal) see your screen and answer questions about the meeting as it happens.
Everything runs on your Mac: no bot joins the meeting and no audio leaves the machine.

## What it does

- **Hears the call and the room.** The recorder (`src/bin/rec/main.rs`, run as `target/recorder/ozen` so macOS
  lists it as ozen) uses ScreenCaptureKit to record three streams in 15s chunks:
  - `call`: audio from meeting apps only (Zoom, Chrome, Teams, Slack, FaceTime, Discord)
  - `mic`: your microphone
  - `local`: every other app, e.g. a video or the `say` command. Never transcribed; it only
    tells the transcriber when the computer itself is talking.
- **Transcribes Hebrew and English.** On-device Whisper (large-v3-turbo, run on the GPU in Rust by `src/whisper.rs`) per utterance, language picked between `he` and `en`
  for each one: Hebrew goes to [ivrit.ai's Hebrew-trained turbo](https://huggingface.co/mlx-community/ivrit-ai-whisper-large-v3-turbo-mlx),
  English to stock large-v3-turbo. Each call is hinted with the vocabulary (terms and names to spell right, e.g. Kev,
  PR, code review; `vocab.json`, edited through the MCP tools; an older `vocab.txt` moves there on the first edit) and the previous line. Known filler that Whisper invents
  on noise ("Thank you.", "תודה רבה") is dropped, and so are lines where it loops one phrase ("Amen. Amen.
  Amen."). Older lines these filters would drop are hidden from the panel (ids in `junk.json`; delete it to show
  them again).
- **Drops echo.** A mic utterance that mostly overlaps call or local audio, in the voice that was playing then,
  is speaker bleed, not a person in the room, so it's discarded. That covers the computer reading text aloud and
  remote voices leaking into the mic. Someone in the room talking over the call keeps their line: their voice
  doesn't match what was playing.
- **Knows who's speaking.** Each utterance gets an ECAPA voiceprint (speechbrain's model, run in Rust by `src/ecapa.rs`) and is matched against voices
  already heard, so a person keeps one label for the whole meeting. Known people come from the
  [voices registry](https://github.com/tupe12334/voices-embedding-registry) and show by name.
- **Hears people talking at once.** In the room, on the call, or both: when voiceprints across an utterance
  disagree (two people at once, or one cutting in without a pause), `src/overlap.rs` separates it into one track per
  voice ([MossFormer2](https://github.com/modelscope/ClearerVoice-Studio), run in Rust on the GPU by `src/separate.rs`), splits each track where its
  voice changes, and each voice becomes its own line with its own speaker and time, so overlapping lines overlap in
  the timeline too. Only such utterances are separated, so a single speaker costs nothing extra. Separation is
  skipped while more than `OZEN_SEPARATE_BACKLOG` chunks (default 6) wait, so the transcript never falls behind.
  `ozen eval-overlap` scores it on real speech.
- **Learns from your tags.** Click any speaker name in the menu bar panel to set who really said that line.
  Each tag runs `ozen tag`, which retrains (`src/train.rs`): every person's voiceprint becomes the average of all lines tagged as them
  (stored in the registry, so tags accumulate across meetings), untagged lines are relabeled with the new
  prints, and the live transcriber reloads them.
- **Shares voices across your Macs.** The registry is a git repo that every Mac clones into `~/ozen/voices`.
  `ozen start` pulls it, the running transcriber pulls again every few minutes (`PULL_EVERY` in `src/transcribe.rs`),
  and every retrain starts from the newest registry and pushes when it's done. Tags made on two Macs at the same
  time both survive: a push that loses the race rebuilds on top of the other Mac's and pushes again. Offline, the
  retrain commits locally and the next one pushes it. Because each retrain resets `~/ozen/voices` to the remote
  before rebuilding, change voices by tagging, never by editing files there. A Mac's own tags live in its
  `tags.json` and `lines.jsonl`, which never leave it; that's what the rebuild re-applies.
- **Keeps your data ready to sync.** Lines, tags, fixes, places and the vocabulary are saved as CRDTs: every
  record has a version, deletes leave a tombstone, and new ids name the Mac that made them, so two Macs' folders
  merge without conflicts, whatever the order. Files from older versions load as they are. `ozen merge DIR`
  merges another ozen folder (another Mac's, a backup) into this one. What's synced and how: [src/crdt.rs](src/crdt.rs).
- **Ignores voices you don't want.** A video playing next to the Mac isn't part of the meeting: click its speaker
  name and pick **Ignore this voice**, or **Ignore all N lines by S3** to mark every nearby line of that speaker at once.
  Each ignore is its own voice (Ignored, Ignored 2…); pick **Same voice as Ignored 2** when it's that video again.
  Those lines, and earlier ones that sound like them, turn grey and leave the timeline, and the transcriber stops
  writing that voice from then on. Ignored prints stay on this Mac (`ignore.json`), never in the registry. To undo,
  tag the line as a person or **Clear tag**, or use a voice's **Stop ignoring** in **Voices…**.
- **Shows how far behind transcription is.** **Timebar…** in the panel (or the right-click menu) draws every
  recorded 15s chunk per stream on a local-time bar with a line at now: done, still waiting, or skipped on an error.
  Scroll or drag back through history, pinch or ⌘-scroll to zoom, **Now** to follow live. Under it, how long after
  recording each chunk was done and how many were waiting at each moment; above it, the backlog, median delay,
  speed against real time, chunks done per minute and when the backlog clears at that pace. The transcriber logs each
  chunk it finishes to `pace.jsonl` (local only, last 14 days); chunks from before that log show as done at an unknown time.
- **Manages voices in one place.** **Voices…** in the panel (or the right-click menu) lists everyone ozen has heard:
  people with their line counts and last few lines, this run's unnamed speakers (S1, S2…), and the ignored voices.
  Rename a person (an existing name merges the two), name or ignore an unnamed speaker in one go, ignore or forget a
  person, or stop ignoring. Forget and merge ask first; forgetting clears the tags on this Mac only. Click a line to
  see it in the transcript.
- **Improves itself: the loop.** Every retrain also
  1. calibrates the same-voice threshold from your tags (the cutoff that best separates same-person from
     different-person similarities), which the live transcriber picks up immediately;
  2. scores every untagged line and queues the ones it's least sure about (near the threshold, or nearly tied
     between two people; the transcriber scores new lines the same way as they arrive, so they don't wait
     for a tag). They show an orange **?**, and **Review N** jumps to the most uncertain one and asks
     who said it. Review only asks about the last 10 minutes: past that, nobody remembers who said what;
  3. logs leave-one-out accuracy to the registry's `history.jsonl`; the footer shows it with the starting value.

  Answering Review is the fastest way to improve it: in a simulation with four similar voices, 12 tags picked by
  Review got untagged-line accuracy to 100% with 2 lines still unsure, versus 98% with 35 unsure for 12 random tags.

- **Learns from your text fixes.** Click a line's text in the panel to correct what was said (empty restores it).
  `ozen fix` keeps the fix in `fixes.json` and relearns `learned.json`, which the live transcriber reads:
  words your fixes add go into Whisper's hint (up to 30, most used first), and a correction you make twice
  (e.g. "פי אר" → "PR") is applied to new lines automatically (the line keeps Whisper's own text as `heard`, which later fixes learn
  from, so undoing a wrong correction cancels it). A correction is skipped while any fix keeps that phrase as right. Each fix also keeps its chunk audio in `fixes/`
  with the right text (`fixes/dataset.jsonl`), ready for fine-tuning a model later.

- **Runs from the menu bar.** Ozen.app (`ozen bar`) puts an ear icon in the top menu bar: click it for the live transcript
  (updates every 2s, Hebrew lines right-to-left) with **Start / Pause / Resume / Stop** buttons; right-click for
  the same controls. The icon is a red filled ear only while recording; otherwise it's monochrome (ear stopped, pause paused,
  hourglass finishing transcription) and the panel says **Not recording** and why.
- **Transcription on or off (advanced).** The gear button in the panel (or **Advanced…** in the right-click
  menu) has a **Transcribe** switch, on by default, that applies right away. Off, recording goes on but nothing is
  transcribed (the audio waits in `chunks/`, taking disk space); back on, it transcribes what's waiting, then keeps up
  live, or stops once done if nothing is recording. Pause is hidden while off: without a transcriber loaded it would
  be the same as Stop. **Low priority** in the same window runs the transcriber at macOS background priority
  (`ozen priority low`), so it gives way to your other apps' CPU, disk and GPU work; the transcript can lag behind.
- **Timeline view.** Switch the panel to **Timeline** for the latest meeting (10 minutes of silence ends one, as in
  **Meetings**): when it ran and how long, then one lane per speaker with their talk time, share of the talk and a
  bar comparing it to whoever talked most. Unnamed voices that spoke under a minute, and lines nobody is sure of,
  share one **Other voices** lane. Every line is a bar showing its words when there's room. The meeting starts fitted
  to the panel; **− / +** zoom. Silences over 2 minutes shrink to a short break marker. Hover a bar for what was
  said; click it to jump to that line in the transcript.
- **Records always, or only meetings.** The **Always / Meetings** toggle in the panel (and right-click menu):
  - *Always*: records until you pause or stop, including the room mic and any audio from meeting apps.
  - *Meetings*: starts by itself when a meeting app (Zoom, Chrome/Meet, Teams, Slack, FaceTime, Discord) is using
    the microphone, and stops 20s after it releases it. Detected through Core Audio's per-process state, so it
    works for any call in those apps. Pausing or stopping by hand holds until the next meeting starts or ends.
    Relaunching the app never stops a recording in progress, and right after launch it waits for your location
    before a place decides anything.
- **Switches by place.** **Places…** (in the panel and the right-click menu) lists labeled places, each set to
  *Auto record*, *Record meetings only* or *Auto off*. While you're inside a place's radius, its setting replaces
  Always / Meetings; arriving or leaving applies right away, and a manual pause or start holds until then. Locate a
  place by typing its latitude and longitude, with **Use current location**, with **Pick on map** and a click, or
  by dragging its pin. Home and Work are there from the start with no location, so they do nothing until you set
  them. Places are saved to `places.json` in the ozen folder as plain JSON (fields and default radius: `Place` in
  `src/places.rs`). Where you are and which place that is are decided in Rust: `ozen place` (polled by the panel)
  keeps a `locate` watcher (`src/bin/locate.rs`, CoreLocation) running while some place has coordinates, and
  **Use current location** runs `ozen places here N`. The watcher carries Ozen's bundle id and signing certificate,
  so it uses the app's location permission; the app itself only shows the permission prompt. If the watcher
  can't get a location while the app can, the app supplies it until the watcher recovers, and `ozen health` warns only
  when neither can. The map is Apple's (MapKit): drag a pin to move a place, or **Pick on map** and click.

- **Hands meetings to an agent.** Switch the panel to **Meetings** for every past meeting (a silence of 10+ minutes
  starts a new one). Select one or more (⌘/⇧-click) and press **Open** to put their transcripts in a fresh folder
  under `context/`, then start Claude Code or Hermes there in a new Terminal window; the folder's `AGENTS.md` (and
  `CLAUDE.md`) tells the agent what it holds, and its `claude.command` / `hermes.command` reopen it with a double-click. **Auto add with Kev** also adds every other meeting that local [Kev](https://github.com/jaredpalmer/kev)
  (`localhost:8009`, override with `OZEN_KEV`) judges part of the same project or topic; its scores show before you pick the agent.
- **Ask about the meeting happening now.** **Ask AI** in the panel starts Claude Code or Hermes on the current
  meeting (one whose last line is under 10 minutes old), in `context/live/`. A background `ozen live-sync` rewrites
  that folder with the latest lines every 15s and exits when the meeting ends; the agent is told to reread it, and that
  `ozen look` shows your screen.
- **Lets any agent read and edit what ozen keeps.** `ozen mcp` is an MCP server: meetings, transcript lines (read,
  add notes, fix text, set speakers, delete), people, places, vocabulary and recording control. Tools are in
  [src/mcp.rs](src/mcp.rs). Add it to Claude Code with `claude mcp add -s user ozen -- ~/ozen/target/release/ozen mcp`,
  or to any MCP client as the command `~/ozen/target/release/ozen` with the argument `mcp`.

Output is `transcript.txt`:

```
[15:06:52] Ofek Gabay (room): אימבדין שלי, אני לא מוצא את זה
[15:09:41] S2 (call): Let's move to the launch timeline.
```

## Setup

Requires macOS 15+ on Apple Silicon, Xcode command line tools, [Rust](https://rustup.rs) (`cargo`) and `ffmpeg`.

```sh
git clone https://github.com/ozenhq/ozen ~/ozen
git clone https://github.com/tupe12334/voices-embedding-registry ~/ozen/voices
cd ~/ozen && cargo run --release -- app   # builds the ozen CLI and ~/Applications/Ozen.app
```

Or, without Rust: download the DMG (built by [release.yml](.github/workflows/release.yml) for each `v*` tag on
main), open it and drag **Ozen** onto **Applications**. It isn't notarized, so the first open is blocked: go to System
Settings > Privacy & Security and press **Open Anyway**. On launch it unpacks its prebuilt CLI into `~/ozen`
(a git checkout there is left alone). It still needs `ffmpeg`. The same release as a tarball, for scripts:
`curl -L <tarball link> | tar xz -C ~` unpacks `~/ozen`.

Then open **Ozen** from Spotlight, Launchpad or Finder like any app. It lives in the menu bar (no Dock icon);
press Start there. After pulling new code, run `cargo run --release -- app` again to rebuild both.
On another Mac, run the same three commands; voices tagged on either one reach the other within a few minutes.

On the first Start, macOS asks **Ozen** for **Screen & System Audio Recording** and **Microphone** access;
grant both (System Settings > Privacy & Security), then press Start again. **Location Services** is asked for
only when you first locate a place; without it, places never match and recording follows Always / Meetings. The build signs the app and recorder
with a local self-signed certificate (created once in `~/Library/Keychains/ozen-signing.keychain-db`), so the
permissions survive rebuilds. The Whisper and ECAPA models download on first use. The MossFormer2 separator (~640MB) downloads in the
background on the first start; until it's ready, people talking at once stay merged in one line.

## Use

| Command | What it does |
|---|---|
| `target/release/ozen start` / `pause` / `resume` / `stop` | Same as the menu bar buttons. Pause keeps the transcriber loaded; stop finishes transcribing queued audio first |
| `target/release/ozen record` | Record without transcribing: chunks wait in `chunks/` until `process`. `stop` ends it |
| `target/release/ozen process` / `process stop` | Transcribe the waiting chunks without recording, then exit (while recording, keeps transcribing until the recording stops) / stop transcribing |
| `target/release/ozen status` | `recording`, `paused`, `stopping`, `processing` or `stopped` |
| `target/release/ozen health` | One line per problem the menu bar warns about: recording blocked, silent mic, transcriber down or behind |
| `target/release/ozen fix <line-id> "right text"` | Correct a line's text (what the panel does); empty clears. Relearns hint words and corrections |
| `target/release/ozen meetings` | Past meetings, newest first: id, start, minutes, lines, first words |
| `target/release/ozen gather [--kev] ID...` | Write those meetings into `context/<now>/`, print the files then the folder; `--kev` adds the ones Kev judges related |
| `target/release/ozen live [--open claude\|hermes]` | Write the meeting happening now into `context/live/` and print the folder; a background `live-sync` keeps it current every 15s until the meeting ends. `--open` starts that agent there |
| `target/release/ozen open DIR claude\|hermes\|finder` | Start that agent (or Finder) in a folder written by `gather` or `live` |
| `target/release/ozen ignore <line-id>...` | Tag lines as a voice to ignore (what **Ignore all** in the panel does). Retrains |
| `target/release/ozen mcp` | MCP server on stdio for agents (see [What it does](#what-it-does)) |
| `target/release/ozen app` | Build and install `~/Applications/Ozen.app` |
| `target/release/ozen bar` | Build if needed and open Ozen.app |
| `target/release/ozen look [N]` | Screenshot to `screen-small.png` and print the last N transcript lines (tagged speakers, fixed text). Use it to answer "what's on screen / what was just said" |
| `target/release/ozen tag <line-id> "Dana Levi"` | Tag a line (what the panel does); empty name clears. Retrains and pushes the registry |
| `target/release/ozen timebar` | JSON of every chunk (start, stream, done / waiting / error / old, delay stats) that **Timebar…** draws |
| `target/release/ozen voices` | JSON list of people, this run's unnamed speakers and ignored voices (what **Voices…** shows) |
| `target/release/ozen name "Dana Levi" <line-id>...` | Tag those lines as a person, e.g. an unnamed speaker's lines. Retrains |
| `target/release/ozen rename <from> <to>` / `forget <name>` | Move a person's tags to another name (merging into an existing one) / clear them on this Mac. `forget "Ignored 2"` stops ignoring that voice. Retrains |
| `target/release/ozen retrain` | Rebuild voiceprints, relabels, ignored voices and accuracy from all tags |
| `target/release/ozen eval [--vocab 0,10,30] [--repeat 0,1,2] [--real] [--fresh]` | Score learning settings (hint-word cap, repeats before an automatic correction) on fixed spoken lines, best first. See [Tuning how fixes teach](#tuning-how-fixes-teach) |
| `target/release/ozen compare [N]` | Transcribe the last N real chunks (kept in `recent/`, 20 max, local only; the computer's own audio goes to `recent/local/` for replaying a missed echo) with stock vs Hebrew vs Hebrew+vocab, to judge changes on your own speech. Shows what the live filters keep, or `(dropped: …)` with Whisper's raw text. `OZEN_KEEP_AUDIO=0` keeps none |
| `target/release/ozen eval-overlap [ami] [he] [call] [--n 20]` | Score people-talking-at-once separation (`src/overlap.rs`) on real speech: AMI meetings, Hebrew speakers, call-codec audio. Recall and extra words with utterances whole vs separated, for overlaps and solos; downloads each set once to `eval/data/` |
| `cargo nextest run` | Every test: Rust unit tests (each file's in `<file>_tests.rs` beside it, loaded with `#[cfg(test)] #[path = "<file>_tests.rs"] mod tests;`), `tests/cli.rs` running the CLI on sample folders (`OZEN_DIR`) the way the panel calls it, and `insta` snapshots (`cargo insta review` after an intended change) |
| `target/release/ozen show [N]` | Last N lines with tag-corrected speakers and fixed text |

## Tuning how fixes teach

`ozen eval` learns from half of a fixed set of lines (as if you had fixed them) and scores each setting on the
other half: word error rate, English terms spelled right, word error rate on plain Hebrew, and words invented on
quiet noise. The lines are in `eval/cases.jsonl`, spoken by macOS's Hebrew voice (Carmit); `--real` uses your own
fixes instead (`fixes/dataset.jsonl`). It's deterministic: Whisper runs with the live transcriber's decode and
filters, seeded per clip, and results are cached by audio, prompt and that code, so a rerun prints the same table and
a sweep only transcribes new prompts. Once a setting wins, set it as `LEARN` in `src/fixes.rs`.

The synthetic lines are few and in one voice. Treat a small gap between settings as noise, and confirm a winner
with `--real` once you have a few dozen fixes.

## Limits

- Lines arrive ~15–30s after speech (chunked, not streaming).
- English terms spoken inside Hebrew are the weakest spot; add them to the vocabulary. Distant voices in the room are hard to hear.
- You talking over the computer voice or a remote speaker can be dropped as echo when your voices sound alike.
- macOS Speak Selection isn't heard on `local` (ScreenCaptureKit doesn't capture that system voice), so text it reads
  aloud is transcribed as a room speaker. Marking that voice with **Ignore this voice** can drop it.
- Up to two voices at once are separated; a third merges into one of them. Overlaps in utterances under ~2s
  aren't detected, and utterances under 1s inherit the previous speaker.
- Live speaker matching is online (no re-clustering); tagging a few lines fixes past and future labels.

## Privacy

In *Always* mode the mic records everything said near the Mac, not only meetings; use *Meetings* mode to limit it.
This records and transcribes other people. Tell participants, and follow your local recording laws.
Voiceprints are biometric data: keep the registry private and enroll only people who agreed.
`places.json` holds where you live and work. It stays on the Mac and is gitignored; don't copy it into shared
folders. Your location is never sent anywhere, but viewing the Places map fetches tiles for that area from
OpenStreetMap's servers.
