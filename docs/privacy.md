# If ozen recorded you

Someone in your meeting uses ozen, an app that transcribes meetings on their Mac. This page says what that means for you, in plain words. Each point links to the code or page that backs it.

## What's recorded

- The meeting's audio: what comes out of the call app and what the Mac's microphone hears ([how it records](../README.md#what-it-does)).
- From that audio, ozen keeps a transcript: what was said, when, and who it thinks said it.
- To tell speakers apart, it keeps a voiceprint for each line: numbers that describe a voice, not a recording of it ([`src/ecapa.rs`](../src/ecapa.rs)). Voiceprints are biometric data.

## Where it's kept

- **On the user's own Mac.** The transcript and voiceprints stay in a folder there. There's no ozen account and no ozen server that keeps meetings.
- **Audio is mostly deleted.** Each 15-second piece of audio is deleted once it's transcribed. The newest 20 pieces (about 5 minutes) are kept so the user can compare transcription models. The audio of a line whose text the user corrected is kept to teach the transcriber ([`src/transcribe.rs`](../src/transcribe.rs)).
- **Voiceprints, today, also on GitHub.** So that all of the user's Macs recognize the same people, voiceprints are also pushed to a private GitHub repository the user owns ([voices registry](../README.md#what-it-does)). This stops once sync can carry them instead.

## Between the user's Macs

If the user turns on sync, their Macs share the transcript with each other:

- directly, when the Macs are on the same network;
- otherwise, through a relay that only passes sealed messages along.

The messages are encrypted with a key only those Macs hold, and the relay stores nothing, not even the encrypted messages. Details: [how sync works](https://github.com/ozenhq/sync/blob/main/docs/architecture.md).

## When the user asks an AI about the meeting

The user can open a meeting in an AI assistant (Claude or Hermes) to ask questions about it ([`src/meetings.rs`](../src/meetings.rs) `open`). The meeting's text then goes to that AI service, under that service's own terms. The same goes for a screenshot of the user's screen if they ask the assistant to look ([`ozen look`](../README.md#use)). Nothing goes to an AI service unless the user uses an assistant with ozen.

## Asking for a meeting to be deleted

Ask the person who recorded you. They can delete a meeting from the transcript, along with its speaker names, corrections and labels, through ozen's assistant tools ([`delete_meeting`](../src/mcp.rs)). With sync on, the deletion reaches their other Macs too, because a delete is kept as a marker that wins over the old copy ([`src/crdt.rs`](../src/crdt.rs)).

Deleting a meeting doesn't touch the plain-text `transcript.txt` log or audio kept for corrections, so ask them to clear those as well. Voiceprints already pushed to GitHub stay in that repository's history.
