# If ozen recorded you

Someone you talked to uses ozen, an app that records and transcribes conversations on their Mac. This page says what that means for you, in plain words.

## What ozen records

- **Calls.** It records the sound of the call apps on the Mac (Zoom, Meet, Teams and others) and what the Mac's microphone hears.
- **The room, sometimes all day.** In *Always* mode the microphone records everything said near the Mac, not only meetings. The Mac's other sounds, like a video playing, are recorded too.
- **A transcript.** From the sound, ozen writes down what was said, when, and who it thinks said it.
- **Voiceprints.** To tell voices apart, ozen stores a voiceprint for each line: numbers that describe how a voice sounds, not a recording of it. A voiceprint is biometric data, like a fingerprint.
- **Screenshots, when asked.** If the user asks ozen to look at their screen, it saves a screenshot of it.

Sources: [what ozen does](../README.md#what-it-does), [`src/ecapa.rs`](../src/ecapa.rs).

## Where it's kept

- **On the user's own Mac.** There's no ozen account, and no ozen server keeps recordings or transcripts.
- **Sound is mostly deleted.** Sound is recorded in 15-second pieces, and each piece is deleted once it's written down. ozen keeps:
  - the newest 20 pieces of call and microphone sound (at most the last 5 minutes) and of the Mac's other sound, so the user can check the transcription;
  - the piece behind any line the user corrected, to teach the transcriber.
- **Copies for AI assistants.** When the user opens a meeting in an AI assistant, ozen also writes a plain-text copy of that meeting, and of any related ones the user adds, into a `context` folder on the Mac.
- **Voiceprints also go to GitHub.** So that all the user's Macs recognize the same people, voiceprints are also uploaded to a GitHub repository the user chooses. ozen's instructions say to keep that repository private, but that's up to the user.

Sources: [`src/transcribe.rs`](../src/transcribe.rs), [`src/meetings.rs`](../src/meetings.rs), [the voices registry](../README.md#what-it-does), [ozen's privacy notes](../README.md#privacy).

## Between the user's Macs

If the user turns on sync, their Macs share the transcript with each other. They do it directly when they're on the same network, and otherwise through a relay server that only passes messages along.

The messages are encrypted with a key that only those Macs have. The relay can't read them and doesn't store them, not even in encrypted form.

Source: [how sync works](https://github.com/ozenhq/sync/blob/main/docs/architecture.md).

## When an AI assistant is involved

The user can connect an AI assistant, such as Claude or Hermes, to ozen and ask it about their meetings.

- **Whole history.** A connected assistant can read the whole transcript history, not only the meeting the user asked about.
- **What gets sent.** What it reads goes to that AI company and falls under that company's own terms. So does a screenshot, if the assistant can run commands on the Mac and asks for one.
- **Live meetings.** For a meeting opened while it's still going, ozen updates the plain-text copy every 15 seconds until 10 minutes after the last line.

Sources: [`src/mcp.rs`](../src/mcp.rs), [`src/meetings.rs`](../src/meetings.rs), [commands](../README.md#use).

## Asking for a meeting to be deleted

Ask the person who recorded you. Their AI assistant can delete a meeting from the transcript, along with its speaker names, corrections and labels. With sync on, the deletion reaches their other Macs too.

Deleting a meeting this way doesn't remove everything. Ask them to also clear:

- the plain-text log, `transcript.txt`;
- sound kept for corrections, and the last few minutes of sound;
- copies in the `context` folder, and screenshots;
- voiceprints already uploaded to GitHub, which stay in that repository's history until removed there.

Sources: [`delete_meeting` in `src/mcp.rs`](../src/mcp.rs), [`src/crdt.rs`](../src/crdt.rs).
