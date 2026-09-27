# ozen

Local meeting copilot for macOS (אוזן, "ear"). While you're in a Zoom / Meet / Teams call, ozen
listens and transcribes the call, knows who is speaking, and lets an assistant (Claude Code in a
terminal) see your screen and answer questions about the meeting as it happens.
Everything runs on your Mac: no bot joins the meeting and no audio leaves the machine.

## What it does

- **Hears the call and the room.** `rec` uses ScreenCaptureKit to record three streams in 15s chunks:
  - `call`: audio from meeting apps only (Zoom, Chrome, Teams, Slack, FaceTime, Discord)
  - `mic`: your microphone
  - `local`: every other app, e.g. macOS Speak Selection reading text aloud. Never transcribed; it only
    tells the transcriber when the computer itself is talking.
- **Transcribes Hebrew and English.** Whisper large-v3-turbo (MLX, on-device) per utterance, with the language
  picked between `he` and `en` for each one, so mixed-language meetings work.
- **Drops echo.** A mic utterance that mostly overlaps call or local audio is speaker bleed, not a person in
  the room, so it's discarded. That covers the computer reading text aloud and remote voices leaking into the mic.
- **Knows who's speaking.** Each utterance gets an ECAPA voiceprint (speechbrain) and is matched against voices
  already heard, so a person keeps one label for the whole meeting. Known people come from the
  [voices registry](https://github.com/tupe12334/voices-embedding-registry) and show by name.

- **Shows the transcript in the menu bar.** `ozen-bar` puts an ear icon in the top menu bar; click it for the live
  transcript (updates every 2s, Hebrew lines right-to-left). Right-click to quit. `start.sh` launches it.

Output is `transcript.txt`:

```
[15:06:52] Ofek Gabay (room): אימבדין שלי, אני לא מוצא את זה
[15:09:41] S2 (call): Let's move to the launch timeline.
```

## Setup

Requires macOS 15+ on Apple Silicon, Xcode command line tools (`swiftc`), [`uv`](https://docs.astral.sh/uv/) and `ffmpeg`.

```sh
git clone https://github.com/tupe12334/ozen ~/ozen
git clone https://github.com/tupe12334/voices-embedding-registry ~/ozen/voices
~/ozen/start.sh
```

On first run macOS asks your terminal for **Screen & System Audio Recording** and **Microphone** access;
grant both (System Settings > Privacy & Security), then run it again. The Whisper and ECAPA models download
on first use.

## Use

| Command | What it does |
|---|---|
| `./start.sh` | Start recording and transcribing (Ctrl-C stops). Run detached: `nohup ./start.sh > start.log 2>&1 &` |
| `./ozen-bar [dir] [--open]` | Menu bar transcript viewer (started by `start.sh`); `--open` shows the panel at launch |
| `./look.sh [N]` | Screenshot to `screen-small.png` and print the last N transcript lines. Use it to answer "what's on screen / what was just said" |
| `python3 enroll.py S3 "Dana Levi"` | Save S3's voiceprint to the registry under that name and push it. Repeat to add more samples |

Stop a detached run: `pkill -f "rec chunks"; pkill -f transcribe.py`.

## Limits

- Lines arrive ~15–30s after speech (chunked, not streaming).
- You talking over the computer voice or a remote speaker can be dropped as echo.
- Overlapping speakers are merged into one line; utterances under 1s inherit the previous speaker.
- Speaker matching is online (no re-clustering), so a voice that was split early stays split. Enroll to fix it.

## Privacy

This records and transcribes other people. Tell participants, and follow your local recording laws.
Voiceprints are biometric data: keep the registry private and enroll only people who agreed.
