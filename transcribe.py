# /// script
# requires-python = ">=3.11"
# dependencies = ["mlx-whisper", "speechbrain", "torchaudio", "clearvoice"]
# ///
"""Watch the chunk dir; transcribe call + mic chunks with local Whisper, drop mic echo of
call/local (computer) audio, label each line by speaker,
append to transcript.txt and lines.jsonl (with voiceprints, for tagging in the menu bar panel)."""
import datetime
import json
import os
import pathlib
import re
import subprocess
import sys
import time

import numpy as np
import torch
from mlx_whisper.audio import load_audio

from speechbrain.inference.speaker import EncoderClassifier

import asr  # Whisper setup shared with the eval (asr.py)
import overlap  # several people in one utterance (overlap.py)

HERE = pathlib.Path(__file__).parent
chunks = pathlib.Path(sys.argv[1])
out = pathlib.Path(sys.argv[2])


VOCAB = HERE / "vocab.txt"  # names/terms Whisper should spell right (Kev, PR, ...); one per line or comma-separated
LEARNED = HERE / "learned.json"  # from your transcript fixes (train.py fix): words to hint, corrections to apply
SOURCE = {"call": "call", "mic": "room"}
ECHO_OVERLAP = 0.6  # mic turn mostly overlapping speaker output = echo, not a person in the room
ECHO_PAD = 0.3  # seconds; slack for capture-latency differences between streams
SR = asr.SR
SILENCE_RMS = overlap.SILENCE_RMS
SAME_SPEAKER = 0.4  # cosine similarity cutoff; replaced by the one train.py calibrates from your tags
MIN_EMBED_SEC = 1.0  # shorter clips give unreliable voiceprints: they never create or update a voice
SHORT_MARGIN = 0.1  # a short clip needs SAME_SPEAKER + this to take an existing label (else "?")

ECAPA = "speechbrain/spkrec-ecapa-voxceleb"
REGISTRY = HERE / "voices"  # clone of tupe12334/voices-embedding-registry, rebuilt by train.py from your tags
RECENT = HERE / "recent"  # last KEEP_AUDIO transcribed chunks (computer audio in recent/local), local only
KEEP_AUDIO = int(os.environ.get("OZEN_KEEP_AUDIO", "20"))
LINES = HERE / "lines.jsonl"  # every transcript line with its voiceprint; the panel tags these
IGNORE = "Ignored"  # voices you tagged to ignore (a video playing nearby); `ozen retrain` (src/ignore.rs) writes their prints
IGNORES = HERE / "ignore.json"
IGNORE_MARGIN = 0.1  # same as src/ignore.rs MARGIN: dropping someone's speech costs more than keeping noise
ignored = np.zeros((0, 192), dtype=np.float32)

encoder = EncoderClassifier.from_hparams(source=ECAPA, savedir=str(HERE / "models/ecapa"), run_opts={"device": "cpu"})
# ponytail: online nearest-centroid clustering, no re-clustering; a voice split early stays split.
speakers: list[list] = []  # [label, centroid, count]; named ones come from the registry
registry_mtime = 0.0
PULL_EVERY = 300  # seconds between registry pulls, so tags made on other machines arrive while running
last_pull = 0.0


def anon(label: str) -> bool:
    return re.fullmatch(r"S\d+", label) is not None  # session label, not a person's name


def load_registry() -> None:
    """(Re)load named voiceprints; train.py rewrites them after every tag, so pick up changes live."""
    global registry_mtime, SAME_SPEAKER, last_pull, ignored
    if time.time() - last_pull > PULL_EVERY:
        last_pull = time.time()
        try:  # ponytail: offline or diverged just keeps the local prints; train.py reconciles on its next push
            subprocess.run(["git", "-C", str(REGISTRY), "pull", "--ff-only", "-q"], capture_output=True, timeout=30)
        except (OSError, subprocess.TimeoutExpired):
            pass
    files = sorted(REGISTRY.glob("voices/*.json"))
    config = REGISTRY / "config.json"
    mtime = max((f.stat().st_mtime for f in [*files, config, IGNORES] if f.exists()), default=0.0)
    if mtime == registry_mtime:
        return
    registry_mtime = mtime
    try:
        SAME_SPEAKER = float(json.loads(config.read_text())["same_speaker"])
    except (OSError, ValueError, KeyError):
        pass
    try:
        ignored = np.array(json.loads(IGNORES.read_text()), dtype=np.float32).reshape(-1, 192)
        ignored /= np.linalg.norm(ignored, axis=1, keepdims=True)
    except (OSError, ValueError):
        pass
    named = {s[0]: s for s in speakers if not anon(s[0])}
    for f in files:
        v = json.loads(f.read_text())
        if v.get("model") != ECAPA:
            continue
        e = np.array(v["embedding"], dtype=np.float32)
        e /= np.linalg.norm(e)
        if v["name"] in named:
            named[v["name"]][1:] = [e, v.get("count", 1)]
        else:
            speakers.append([v["name"], e, v.get("count", 1)])
    print(f"known voices: {[s[0] for s in speakers if not anon(s[0])]}, {len(ignored)} ignored lines, "
          f"threshold {SAME_SPEAKER}", flush=True)


load_registry()
unknown = 0
RUN = int(time.time())  # S1, S2... are per run: lines carry it so the panel can group a label's lines


def embed(clip: np.ndarray) -> np.ndarray:
    e = encoder.encode_batch(torch.from_numpy(clip.astype(np.float32))[None]).squeeze().numpy()
    return e / np.linalg.norm(e)


def who(clip: np.ndarray, e: np.ndarray) -> str:
    # ponytail: nearest ignored line, O(ignored lines) per utterance; fine for thousands
    near = float((ignored @ e).max()) if len(ignored) else -1.0
    if near >= SAME_SPEAKER + IGNORE_MARGIN and near > max((float(s[1] @ e) for s in speakers if not anon(s[0])), default=-1.0):
        return IGNORE  # closer to a voice you ignored than to anyone you know
    if clip.size < MIN_EMBED_SEC * SR:
        # A short clip's print is too noisy to found or reshape a voice: label it only on a strong match.
        best = max(speakers, key=lambda s: float(s[1] @ e), default=None)
        return best[0] if best is not None and float(best[1] @ e) >= SAME_SPEAKER + SHORT_MARGIN else "?"
    if speakers:
        best = max(speakers, key=lambda s: float(s[1] @ e))
        if float(best[1] @ e) >= SAME_SPEAKER:
            if anon(best[0]):  # named prints change only through tagging (train.py)
                c = best[1] * best[2] + e
                best[1], best[2] = c / np.linalg.norm(c), best[2] + 1
            return best[0]
    global unknown
    unknown += 1
    speakers.append([f"S{unknown}", e, 1])
    return speakers[-1][0]


def doubt(e: np.ndarray) -> float | None:
    """How unsure train.py would be about this line (same formula), so Review can ask before the next retrain."""
    sims = sorted((float(s[1] @ e) for s in speakers if not anon(s[0])), reverse=True)
    if not sims:
        return None
    margin = sims[0] - sims[1] if len(sims) > 1 else sims[0] - SAME_SPEAKER
    return round(min(abs(sims[0] - SAME_SPEAKER), margin), 3)


last_lang: dict[str, str] = {}  # per source; short clips reuse it
last_text: dict[str, str] = {}  # per source; previous line, given to Whisper as context


def learned() -> dict:
    try:
        return json.loads(LEARNED.read_text())
    except (OSError, ValueError):
        return {}


def hint_words() -> list[str]:
    """The vocabulary + known people's names + words from your fixes, given to Whisper so it spells them."""
    words = [w.strip() for w in re.split(r"[,\n]", VOCAB.read_text()) if w.strip()] if VOCAB.exists() else []
    words += [s[0] for s in speakers if not anon(s[0])]
    return words + learned().get("vocab", [])


def transcribe(clip: np.ndarray, tag: str) -> str:
    words = hint_words()  # the prompt adds the previous line as context
    text, last_lang[tag] = asr.recognize(clip, asr.prompt(words, last_text.get(tag, "")),
                                         last_lang.get(tag, asr.LANGS[0]), words)
    if text:
        last_text[tag] = text
    return text


# absolute times when call or local (computer) audio played, with its voiceprint (None under MIN_EMBED_SEC)
active: list[tuple[float, float, np.ndarray | None]] = []
covered = {"call": 0.0, "local": 0.0}  # end time of the latest chunk seen per reference stream


def echo(t0: float, t1: float, e: np.ndarray | None) -> tuple[float, float | None]:
    """How much of a mic turn the call/computer audio overlaps, and how close the turn's voice is to the voices
    playing then (None when none of them has a print). Echo sounds like its source (~0.8 on speaker bleed);
    you talking over a remote speaker doesn't."""
    hits = [(max(0.0, min(t1, b + ECHO_PAD) - max(t0, a - ECHO_PAD)), p) for a, b, p in active]
    prints = [p for d, p in hits if d > 0 and p is not None]
    return (sum(d for d, _ in hits) / max(t1 - t0, 1e-6),
            max(float(p @ e) for p in prints) if prints and e is not None else None)


def start_ms(f: pathlib.Path) -> int:
    return int(f.stem.split("-")[0])


def pending() -> list[pathlib.Path]:
    """Newest first, so the live meeting is transcribed before any backlog. The mic chunk of a 15s window
    sorts after that window's call/local chunks (started ms apart), which its echo check needs."""
    return sorted(chunks.glob("*.wav"), key=lambda f: start_ms(f) - (1000 if f.stem.endswith("-mic") else 0), reverse=True)


print(f"transcribing {chunks} -> {out}", flush=True)
while True:
    for f in pending():
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
                lines, prev = [], None  # [start, speaker, text, print sum, end], merged while speaker repeats
                turns = overlap.utterances(audio) if tag == "local" else (
                    (u + o, p) for u, c in overlap.utterances(audio) for o, p in overlap.voices(c, embed, SAME_SPEAKER))
                for start, clip in turns:
                    t0, t1 = t_chunk + start, t_chunk + start + len(clip) / SR
                    long = len(clip) >= MIN_EMBED_SEC * SR  # shorter prints are too noisy to judge echo by
                    e = embed(clip) if long or tag != "local" else None
                    if tag != "mic":
                        active.append((t0, t1, e if long else None))
                    if tag == "local":
                        continue  # computer's own audio: reference only, never transcribed
                    if tag == "mic":
                        share, near = echo(t0, t1, e if long else None)
                        if share >= ECHO_OVERLAP and (near is None or near >= SAME_SPEAKER):
                            print(f"echo dropped {t1 - t0:.1f}s", flush=True)
                            continue  # speakers leaking into the mic
                        # Evidence for a missed echo: how much computer audio overlapped, how close its voice was,
                        # and whether the local reference even reached this far (negative = it hadn't been read yet).
                        print(f"mic kept {t1 - t0:.1f}s in {f.name}: echo overlap {share:.2f}, voice similarity "
                              f"{'-' if near is None else f'{near:.2f}'}, local reference {covered['local'] - t1:+.1f}s "
                              f"past it", flush=True)
                    text = transcribe(clip, tag)
                    if not text:
                        continue
                    spk = who(clip, e)
                    if spk == IGNORE:
                        print(f"ignored voice dropped {t1 - t0:.1f}s", flush=True)
                        continue
                    if spk == "?" and prev is not None:
                        spk = prev  # short clip mid-turn: most likely the same person continuing
                    w = len(clip) / SR if len(clip) >= MIN_EMBED_SEC * SR else 0.01  # short clips barely count
                    end = start + len(clip) / SR
                    if lines and lines[-1][1] == spk:
                        lines[-1][2] += " " + text
                        lines[-1][3] += w * e
                        lines[-1][4] = end
                    else:
                        lines.append([start, spk, text, w * e, end])
                    prev = spk
                with out.open("a") as fh, LINES.open("a") as lj:
                    for i, (start, spk, heard, esum, end) in enumerate(lines):
                        text = asr.corrected(heard, learned().get("replace", {}))
                        ts = datetime.datetime.fromtimestamp(t_chunk + start).strftime("%H:%M:%S")
                        line = f"[{ts}] {spk} ({SOURCE.get(tag, tag)}): {text}"
                        fh.write(line + "\n")
                        print(line, flush=True)
                        e = esum / np.linalg.norm(esum)
                        rec = {"id": f"{ms}-{tag}-{i}", "t": round(t_chunk + start, 2), "d": round(end - start, 2),
                               "src": SOURCE.get(tag, tag), "run": RUN, "spk": spk, "text": text, "e": e.round(5).tolist()}
                        if (dt := doubt(e)) is not None:
                            rec["doubt"] = dt
                        if text != heard:
                            rec["heard"] = heard  # what Whisper said; fixes learn from this, not the correction
                        lj.write(json.dumps(rec, ensure_ascii=False) + "\n")
        except Exception as e:  # one bad chunk must not kill the live transcript
            print(f"skip {f.name}: {e}", file=sys.stderr, flush=True)
        if KEEP_AUDIO and f.exists():
            # mic/call: recent audio for comparing models (uv run eval.py). local: computer audio only,
            # kept apart so a missed echo can be replayed with the reference the transcriber had.
            keep = RECENT / "local" if tag == "local" else RECENT
            keep.mkdir(parents=True, exist_ok=True)
            f.replace(keep / f.name)
            for old in sorted(keep.glob("*.wav"))[:-KEEP_AUDIO]:
                old.unlink()
        f.unlink(missing_ok=True)
        if any(start_ms(g) > int(ms) for g in chunks.glob("*.wav")):
            break  # newer audio arrived: transcribe it before going further back
    # Keep echo windows as far back as the oldest chunk still waiting, so backlog mic chunks keep theirs.
    oldest = min((start_ms(g) / 1000 for g in chunks.glob("*.wav")), default=time.time())
    active = [x for x in active if x[1] > min(oldest, time.time()) - 120]
    load_registry()
    time.sleep(1)
