# /// script
# requires-python = ">=3.11"
# dependencies = ["mlx-whisper"]
# ///
"""Speech to text for one clip: the Whisper setup the live transcriber (transcribe.py) and the eval (`ozen eval`)
share, so a configuration measured by the eval is the one that runs in meetings.

    uv run asr.py    # eval worker: JSON lines on stdin -> JSON lines on stdout, same order

Request: {"audio": path, "start": s, "duration": s, "words": [hint words], "replace": {wrong: right}, "lang": "he"}
         or {"heard": text, "replace": {...}} to only apply corrections (no audio, no model)
Reply:   {"heard": Whisper's text, "text": after replace, "lang": language used}
The worker seeds MLX before every clip, so its output depends only on the request.
"""
import functools
import json
import re
import socket
import sys

import mlx.core as mx
import mlx_whisper
import numpy as np
from huggingface_hub import snapshot_download
from mlx_whisper.audio import load_audio, log_mel_spectrogram, pad_or_trim
from mlx_whisper.decoding import detect_language
from mlx_whisper.load_models import load_model
from mlx_whisper.transcribe import ModelHolder

# mlx_whisper caches a single model, but every utterance uses two: stock turbo detects the language, then
# the Hebrew model transcribes. Swapping reloaded ~1.6GB twice per line and made the transcriber fall behind.
# ponytail: keeps every model used resident (two, ~3GB); bound the cache if more models are added.
ModelHolder.get_model = staticmethod(functools.cache(lambda path, dtype: load_model(path, dtype=dtype)))
socket.setdefaulttimeout(60)  # a stalled download must fail, not hang the transcriber forever
SR = 16000
LANGS = ("he", "en")


def cached(repo: str) -> str:
    """The local copy once downloaded, so loading never waits on the Hub's update check."""
    try:
        return snapshot_download(repo, local_files_only=True)
    except Exception:
        return repo  # not downloaded yet: fetch on first use


MODEL = cached("mlx-community/whisper-large-v3-turbo")  # English + language detection
# Hebrew-trained Whisper (ivrit.ai); stock turbo mangles conversational Hebrew and English terms inside it.
MODELS = {"he": cached("mlx-community/ivrit-ai-whisper-large-v3-turbo-mlx"), "en": MODEL}
def words_of(text: str) -> list[str]:
    return re.sub(r"[^\w\s]", " ", text.lower()).split()


NOISE = {  # what Whisper invents on noise, per language
    "en": {"thank you", "thanks", "you", "bye"},
    # ivrit.ai's Hebrew model is trained heavily on Knesset recordings: on clicks and bumps it answers with
    # the parliamentary openers ("חברי הכנסת,", "אדוני היושב-ראש, חברות וחברי הכנסת").
    "he": {"תודה", "תודה רבה", "רבה", "תודה לכם", "ביי",
           "חברי הכנסת", "חברות וחברי הכנסת", "חברות וחברות הכנסת", "אדוני היושב-ראש", "גבירתי היושבת-ראש"},
}
FILLER = {w for phrases in NOISE.values() for p in phrases for w in words_of(p)}  # same split as the check


def noise(text: str) -> bool:
    """"תודה. תודה רבה." on silence: every word is known filler. Checked across languages, because a clip
    detected as English can still come out in Hebrew (the prompt carries Hebrew context)."""
    words = words_of(text)
    return not words or set(words) <= FILLER


def hint_echo(text: str, hint: list[str]) -> bool:
    """"Kev. Kev. Kev." on noise: Whisper repeats terms from its own prompt. A line made only of hint words (and
    filler) that repeats itself is that echo. A single "Kev." can be real speech, so it stays."""
    words = words_of(text)
    return len(words) > len(set(words)) and set(words) <= {w for h in hint for w in words_of(h)} | FILLER


def recognize(clip: np.ndarray, prompt: str, lang: str, hint: list[str] = ()) -> tuple[str, str]:
    """(text, language). `lang` is used for clips under ~1.5s, where detection is unreliable ("שלום" came out
    as "Shalom"); longer clips pick between the languages actually spoken, since open detection on short
    noisy audio picks random languages and invents words."""
    if clip.size >= 1.5 * SR:
        model = ModelHolder.get_model(MODEL, mx.float16)
        mel = log_mel_spectrogram(pad_or_trim(mx.array(clip)), n_mels=model.dims.n_mels)
        _, probs = detect_language(model, mel)
        lang = max(LANGS, key=lambda l: probs.get(l, 0))
    r = mlx_whisper.transcribe(clip, path_or_hf_repo=MODELS[lang], language=lang, initial_prompt=prompt,
                               condition_on_previous_text=False)
    # Drop segments Whisper itself flags as noise; these are the hallucinated lines.
    text = " ".join(
        s["text"].strip()
        for s in r["segments"]
        if s["no_speech_prob"] < 0.5 and s["avg_logprob"] > -0.8 and s["compression_ratio"] < 2.4
    ).strip()
    return ("" if noise(text) or hint_echo(text, hint) else text), lang


def prompt(words: list[str], previous: str = "") -> str:
    """Whisper's initial prompt: hint words (deduplicated, in order), then the previous line as context."""
    return (", ".join(dict.fromkeys(words)) + ". " + previous[-200:]).strip()


def corrected(text: str, replace: dict[str, str]) -> str:
    """Apply the corrections you made repeatedly (learned.json "replace"), whole words only."""
    for wrong, right in replace.items():
        text = re.sub(rf"(?<!\w){re.escape(wrong)}(?!\w)", right, text)
    return text


if __name__ == "__main__":
    for row in sys.stdin:
        q = json.loads(row)
        if "audio" not in q:
            print(json.dumps({"heard": q["heard"], "text": corrected(q["heard"], q.get("replace", {}))},
                             ensure_ascii=False), flush=True)
            continue
        audio = np.array(load_audio(q["audio"]))
        s = int(q.get("start", 0) * SR)
        clip = audio[s: s + int(q["duration"] * SR)] if q.get("duration") else audio[s:]
        mx.random.seed(0)  # temperature fallback samples; seeded, the same request always gives the same text
        words = q.get("words", [])
        heard, lang = recognize(clip, prompt(words), q.get("lang", LANGS[0]), words)
        print(json.dumps({"heard": heard, "text": corrected(heard, q.get("replace", {})), "lang": lang},
                         ensure_ascii=False), flush=True)
