# /// script
# requires-python = ">=3.11"
# dependencies = ["mlx-whisper", "speechbrain", "torchaudio"]
# ///
"""Watch the chunk dir; transcribe call + mic chunks with local Whisper, drop mic echo of
call/local (computer) audio, label each line by speaker,
append to transcript.txt. Rename speakers via names.json, e.g. {"S1": "Ofek"}."""
import datetime
import json
import pathlib
import sys
import time

import mlx.core as mx
import mlx_whisper
import numpy as np
import torch
from mlx_whisper.audio import load_audio, log_mel_spectrogram, pad_or_trim
from mlx_whisper.decoding import detect_language
from mlx_whisper.transcribe import ModelHolder
from speechbrain.inference.speaker import EncoderClassifier

HERE = pathlib.Path(__file__).parent
chunks = pathlib.Path(sys.argv[1])
out = pathlib.Path(sys.argv[2])
MODEL = "mlx-community/whisper-large-v3-turbo"
SOURCE = {"call": "call", "mic": "room"}
ECHO_OVERLAP = 0.6  # mic turn mostly overlapping speaker output = echo, not a person in the room
ECHO_PAD = 0.3  # seconds; slack for capture-latency differences between streams
LANGS = ("he", "en")
SR = 16000
SILENCE_RMS = 0.003  # below this Whisper hallucinates ("Thank you."), so skip
SAME_SPEAKER = 0.4  # cosine similarity; ECAPA same-voice pairs sit well above, different voices below
MIN_EMBED_SEC = 1.0  # shorter segments give unreliable voiceprints; they inherit the previous speaker

ECAPA = "speechbrain/spkrec-ecapa-voxceleb"
REGISTRY = HERE / "voices"  # clone of tupe12334/voices-embedding-registry; see enroll.py
SESSION = HERE / "session_speakers.json"  # this run's voiceprints, read by enroll.py

encoder = EncoderClassifier.from_hparams(source=ECAPA, savedir=str(HERE / "models/ecapa"), run_opts={"device": "cpu"})
# ponytail: online nearest-centroid clustering, no re-clustering; a voice split early stays split.
speakers: list[list] = []  # [label, centroid, count]
for p in sorted(REGISTRY.glob("voices/*.json")):
    v = json.loads(p.read_text())
    if v.get("model") == ECAPA:
        e = np.array(v["embedding"], dtype=np.float32)
        speakers.append([v["name"], e / np.linalg.norm(e), v.get("count", 1)])
print(f"known voices: {[s[0] for s in speakers]}", flush=True)
unknown = 0


def who(clip: np.ndarray) -> str:
    e = encoder.encode_batch(torch.from_numpy(clip)[None]).squeeze().numpy()
    e /= np.linalg.norm(e)
    if speakers:
        best = max(speakers, key=lambda s: float(s[1] @ e))
        if float(best[1] @ e) >= SAME_SPEAKER:
            c = best[1] * best[2] + e
            best[1], best[2] = c / np.linalg.norm(c), best[2] + 1
            return best[0]
    global unknown
    unknown += 1
    speakers.append([f"S{unknown}", e, 1])
    return speakers[-1][0]


def save_session() -> None:
    SESSION.write_text(json.dumps({s[0]: {"embedding": s[1].tolist(), "count": s[2]} for s in speakers}))


def names() -> dict:
    try:
        return json.loads((HERE / "names.json").read_text())
    except (OSError, ValueError):
        return {}


def utterances(audio: np.ndarray, frame=0.03, max_gap=0.35, min_len=0.3):
    """Split on pauses so each piece is one speaker turn; Whisper segments span speaker changes."""
    n = int(frame * SR)
    rms = np.sqrt(np.mean(audio[: len(audio) // n * n].reshape(-1, n) ** 2, axis=1))
    voiced = np.flatnonzero(rms > max(SILENCE_RMS, 0.15 * np.percentile(rms, 95)))
    if not voiced.size:
        return
    groups = np.split(voiced, np.flatnonzero(np.diff(voiced) * frame > max_gap) + 1)
    for g in groups:
        start, end = max(0, g[0] - 3) * n, min(len(audio), (g[-1] + 4) * n)  # ~0.1s padding
        if (end - start) / SR >= min_len:
            yield start / SR, audio[start:end]


def transcribe(clip: np.ndarray) -> str:
    # Auto-detect per utterance, but only between the languages actually spoken;
    # open detection on short noisy audio picks random languages and invents words.
    model = ModelHolder.get_model(MODEL, mx.float16)
    mel = log_mel_spectrogram(pad_or_trim(mx.array(clip)), n_mels=model.dims.n_mels)
    _, probs = detect_language(model, mel)
    lang = max(LANGS, key=lambda l: probs.get(l, 0))
    r = mlx_whisper.transcribe(clip, path_or_hf_repo=MODEL, language=lang, condition_on_previous_text=False)
    # Drop segments Whisper itself flags as noise; these are the hallucinated lines.
    text = " ".join(
        s["text"].strip()
        for s in r["segments"]
        if s["no_speech_prob"] < 0.5 and s["avg_logprob"] > -0.8 and s["compression_ratio"] < 2.4
    ).strip()
    return "" if text.lower().strip(" .!") in {"thank you", "thanks", "you", "bye"} else text


active: list[tuple[float, float]] = []  # absolute times when call or local (computer) audio played
covered = {"call": 0.0, "local": 0.0}  # end time of the latest chunk seen per reference stream


def echo_fraction(t0: float, t1: float) -> float:
    return sum(max(0.0, min(t1, b + ECHO_PAD) - max(t0, a - ECHO_PAD)) for a, b in active) / max(t1 - t0, 1e-6)


print(f"transcribing {chunks} -> {out}", flush=True)
while True:
    for f in sorted(chunks.glob("*.wav")):
        ms, tag = f.stem.split("-")
        t_chunk = int(ms) / 1000
        # Mic echo check needs the call/local audio for the same time window first.
        if tag == "mic" and min(covered.values()) < t_chunk + 14 and time.time() - f.stat().st_mtime < 40:
            continue
        try:
            audio = np.array(load_audio(str(f)))
            if tag in covered:
                covered[tag] = max(covered[tag], t_chunk + len(audio) / SR)
            if audio.size and np.sqrt(np.mean(audio**2)) > SILENCE_RMS:
                lines, prev = [], None  # [start_sec, speaker, text], merged while speaker repeats
                for start, clip in utterances(audio):
                    t0, t1 = t_chunk + start, t_chunk + start + len(clip) / SR
                    if tag != "mic":
                        active.append((t0, t1))
                    if tag == "local":
                        continue  # computer's own audio: reference only, never transcribed
                    if tag == "mic" and echo_fraction(t0, t1) >= ECHO_OVERLAP:
                        print(f"echo dropped {t1 - t0:.1f}s", flush=True)
                        continue  # speakers leaking into the mic
                    text = transcribe(clip)
                    if not text:
                        continue
                    spk = who(clip) if clip.size >= MIN_EMBED_SEC * SR or prev is None else prev
                    if lines and lines[-1][1] == spk:
                        lines[-1][2] += " " + text
                    else:
                        lines.append([start, spk, text])
                    prev = spk
                n = names()
                with out.open("a") as fh:
                    for start, spk, text in lines:
                        ts = datetime.datetime.fromtimestamp(t_chunk + start).strftime("%H:%M:%S")
                        line = f"[{ts}] {n.get(spk, spk)} ({SOURCE.get(tag, tag)}): {text}"
                        fh.write(line + "\n")
                        print(line, flush=True)
                if lines:
                    save_session()
        except Exception as e:  # one bad chunk must not kill the live transcript
            print(f"skip {f.name}: {e}", file=sys.stderr, flush=True)
        f.unlink(missing_ok=True)
    active = [(a, b) for a, b in active if b > time.time() - 120]
    time.sleep(1)
