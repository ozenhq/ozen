# /// script
# requires-python = ">=3.11"
# dependencies = ["mlx-whisper", "speechbrain", "torchaudio"]
# ///
"""Watch the chunk dir; transcribe call + mic chunks with local Whisper, drop mic echo of
call/local (computer) audio, label each line by speaker,
append to transcript.txt and lines.jsonl (with voiceprints, for tagging in the menu bar panel)."""
import datetime
import functools
import json
import os
import pathlib
import re
import socket
import subprocess
import sys
import time

import mlx.core as mx
import mlx_whisper
import numpy as np
import torch
from mlx_whisper.audio import load_audio, log_mel_spectrogram, pad_or_trim
from mlx_whisper.decoding import detect_language
from huggingface_hub import snapshot_download
from mlx_whisper.load_models import load_model
from mlx_whisper.transcribe import ModelHolder

# mlx_whisper caches a single model, but every utterance uses two: stock turbo detects the language, then
# the Hebrew model transcribes. Swapping reloaded ~1.6GB twice per line and made the transcriber fall behind.
# ponytail: keeps every model used resident (two, ~3GB); bound the cache if more models are added.
ModelHolder.get_model = staticmethod(functools.cache(lambda path, dtype: load_model(path, dtype=dtype)))
from speechbrain.inference.speaker import EncoderClassifier

HERE = pathlib.Path(__file__).parent
chunks = pathlib.Path(sys.argv[1])
out = pathlib.Path(sys.argv[2])
socket.setdefaulttimeout(60)  # a stalled download must fail, not hang the transcriber forever


def cached(repo: str) -> str:
    """The local copy once downloaded, so loading never waits on the Hub's update check."""
    try:
        return snapshot_download(repo, local_files_only=True)
    except Exception:
        return repo  # not downloaded yet: fetch on first use


MODEL = cached("mlx-community/whisper-large-v3-turbo")  # English + language detection
# Hebrew-trained Whisper (ivrit.ai); stock turbo mangles conversational Hebrew and English terms inside it.
MODELS = {"he": cached("mlx-community/ivrit-ai-whisper-large-v3-turbo-mlx"), "en": MODEL}
VOCAB = HERE / "vocab.txt"  # names/terms Whisper should spell right (Kev, PR, ...); one per line or comma-separated
LEARNED = HERE / "learned.json"  # from your transcript fixes (train.py fix): words to hint, corrections to apply
NOISE = {  # what Whisper invents on noise, per language
    "en": {"thank you", "thanks", "you", "bye"},
    "he": {"תודה", "תודה רבה", "רבה", "תודה לכם", "ביי"},
}
SOURCE = {"call": "call", "mic": "room"}
ECHO_OVERLAP = 0.6  # mic turn mostly overlapping speaker output = echo, not a person in the room
ECHO_PAD = 0.3  # seconds; slack for capture-latency differences between streams
LANGS = ("he", "en")
SR = 16000
SILENCE_RMS = 0.003  # below this Whisper hallucinates ("Thank you."), so skip
SAME_SPEAKER = 0.4  # cosine similarity cutoff; replaced by the one train.py calibrates from your tags
MIN_EMBED_SEC = 1.0  # shorter clips give unreliable voiceprints: they never create or update a voice
SHORT_MARGIN = 0.1  # a short clip needs SAME_SPEAKER + this to take an existing label (else "?")

ECAPA = "speechbrain/spkrec-ecapa-voxceleb"
REGISTRY = HERE / "voices"  # clone of tupe12334/voices-embedding-registry, rebuilt by train.py from your tags
RECENT = HERE / "recent"  # last KEEP_AUDIO transcribed chunks, local only
KEEP_AUDIO = int(os.environ.get("OZEN_KEEP_AUDIO", "20"))
LINES = HERE / "lines.jsonl"  # every transcript line with its voiceprint; the panel tags these

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
    global registry_mtime, SAME_SPEAKER, last_pull
    if time.time() - last_pull > PULL_EVERY:
        last_pull = time.time()
        try:  # ponytail: offline or diverged just keeps the local prints; train.py reconciles on its next push
            subprocess.run(["git", "-C", str(REGISTRY), "pull", "--ff-only", "-q"], capture_output=True, timeout=30)
        except (OSError, subprocess.TimeoutExpired):
            pass
    files = sorted(REGISTRY.glob("voices/*.json"))
    config = REGISTRY / "config.json"
    mtime = max((f.stat().st_mtime for f in [*files, config] if f.exists()), default=0.0)
    if mtime == registry_mtime:
        return
    registry_mtime = mtime
    try:
        SAME_SPEAKER = float(json.loads(config.read_text())["same_speaker"])
    except (OSError, ValueError, KeyError):
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
    print(f"known voices: {[s[0] for s in speakers if not anon(s[0])]}, threshold {SAME_SPEAKER}", flush=True)


load_registry()
unknown = 0


def who(clip: np.ndarray) -> tuple[str, np.ndarray]:
    e = encoder.encode_batch(torch.from_numpy(clip)[None]).squeeze().numpy()
    e /= np.linalg.norm(e)
    if clip.size < MIN_EMBED_SEC * SR:
        # A short clip's print is too noisy to found or reshape a voice: label it only on a strong match.
        best = max(speakers, key=lambda s: float(s[1] @ e), default=None)
        return (best[0] if best is not None and float(best[1] @ e) >= SAME_SPEAKER + SHORT_MARGIN else "?"), e
    if speakers:
        best = max(speakers, key=lambda s: float(s[1] @ e))
        if float(best[1] @ e) >= SAME_SPEAKER:
            if anon(best[0]):  # named prints change only through tagging (train.py)
                c = best[1] * best[2] + e
                best[1], best[2] = c / np.linalg.norm(c), best[2] + 1
            return best[0], e
    global unknown
    unknown += 1
    speakers.append([f"S{unknown}", e, 1])
    return speakers[-1][0], e


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


last_lang: dict[str, str] = {}  # per source; short clips reuse it
last_text: dict[str, str] = {}  # per source; previous line, given to Whisper as context


def learned() -> dict:
    try:
        return json.loads(LEARNED.read_text())
    except (OSError, ValueError):
        return {}


def corrected(text: str) -> str:
    """Apply the corrections you made repeatedly (learned.json "replace"), whole words only."""
    for wrong, right in learned().get("replace", {}).items():
        text = re.sub(rf"(?<!\w){re.escape(wrong)}(?!\w)", right, text)
    return text


def hint(tag: str) -> str:
    """Prompt with the vocabulary + known people's names + words from your fixes + the previous line,
    so Whisper spells them."""
    words = [w.strip() for w in re.split(r"[,\n]", VOCAB.read_text()) if w.strip()] if VOCAB.exists() else []
    words += [s[0] for s in speakers if not anon(s[0])]
    words += learned().get("vocab", [])
    return (", ".join(dict.fromkeys(words)) + ". " + last_text.get(tag, "")[-200:]).strip()


FILLER = {w for phrases in NOISE.values() for p in phrases for w in p.split()}


def noise(text: str) -> bool:
    """"תודה. תודה רבה." on silence: every word is known filler. Checked across languages, because a clip
    detected as English can still come out in Hebrew (the hint carries Hebrew names and context)."""
    words = re.sub(r"[^\w\s]", " ", text.lower()).split()
    return not words or set(words) <= FILLER


def transcribe(clip: np.ndarray, tag: str) -> str:
    # Auto-detect per utterance, but only between the languages actually spoken;
    # open detection on short noisy audio picks random languages and invents words.
    # Under ~1.5s detection is unreliable ("שלום" came out as "Shalom"), so keep the source's last language.
    if clip.size < 1.5 * SR:
        lang = last_lang.get(tag, LANGS[0])
    else:
        model = ModelHolder.get_model(MODEL, mx.float16)
        mel = log_mel_spectrogram(pad_or_trim(mx.array(clip)), n_mels=model.dims.n_mels)
        _, probs = detect_language(model, mel)
        lang = last_lang[tag] = max(LANGS, key=lambda l: probs.get(l, 0))
    r = mlx_whisper.transcribe(clip, path_or_hf_repo=MODELS[lang], language=lang, initial_prompt=hint(tag),
                               condition_on_previous_text=False)
    # Drop segments Whisper itself flags as noise; these are the hallucinated lines.
    text = " ".join(
        s["text"].strip()
        for s in r["segments"]
        if s["no_speech_prob"] < 0.5 and s["avg_logprob"] > -0.8 and s["compression_ratio"] < 2.4
    ).strip()
    if noise(text):
        return ""
    last_text[tag] = text
    return text


active: list[tuple[float, float]] = []  # absolute times when call or local (computer) audio played
covered = {"call": 0.0, "local": 0.0}  # end time of the latest chunk seen per reference stream


def echo_fraction(t0: float, t1: float) -> float:
    return sum(max(0.0, min(t1, b + ECHO_PAD) - max(t0, a - ECHO_PAD)) for a, b in active) / max(t1 - t0, 1e-6)


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
                for start, clip in utterances(audio):
                    t0, t1 = t_chunk + start, t_chunk + start + len(clip) / SR
                    if tag != "mic":
                        active.append((t0, t1))
                    if tag == "local":
                        continue  # computer's own audio: reference only, never transcribed
                    if tag == "mic" and echo_fraction(t0, t1) >= ECHO_OVERLAP:
                        print(f"echo dropped {t1 - t0:.1f}s", flush=True)
                        continue  # speakers leaking into the mic
                    text = transcribe(clip, tag)
                    if not text:
                        continue
                    spk, e = who(clip)
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
                        text = corrected(heard)
                        ts = datetime.datetime.fromtimestamp(t_chunk + start).strftime("%H:%M:%S")
                        line = f"[{ts}] {spk} ({SOURCE.get(tag, tag)}): {text}"
                        fh.write(line + "\n")
                        print(line, flush=True)
                        rec = {"id": f"{ms}-{tag}-{i}", "t": round(t_chunk + start, 2), "d": round(end - start, 2),
                               "src": SOURCE.get(tag, tag),
                               "spk": spk, "text": text, "e": (esum / np.linalg.norm(esum)).round(5).tolist()}
                        if text != heard:
                            rec["heard"] = heard  # what Whisper said; fixes learn from this, not the correction
                        lj.write(json.dumps(rec, ensure_ascii=False) + "\n")
        except Exception as e:  # one bad chunk must not kill the live transcript
            print(f"skip {f.name}: {e}", file=sys.stderr, flush=True)
        if KEEP_AUDIO and tag != "local" and f.exists():  # recent audio for comparing models (uv run eval.py)
            RECENT.mkdir(exist_ok=True)
            f.replace(RECENT / f.name)
            for old in sorted(RECENT.glob("*.wav"))[:-KEEP_AUDIO]:
                old.unlink()
        f.unlink(missing_ok=True)
        if any(start_ms(g) > int(ms) for g in chunks.glob("*.wav")):
            break  # newer audio arrived: transcribe it before going further back
    # Keep echo windows as far back as the oldest chunk still waiting, so backlog mic chunks keep theirs.
    oldest = min((start_ms(g) / 1000 for g in chunks.glob("*.wav")), default=time.time())
    active = [(a, b) for a, b in active if b > min(oldest, time.time()) - 120]
    load_registry()
    time.sleep(1)
