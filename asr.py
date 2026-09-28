"""Speech to text for one clip: the Whisper setup the live transcriber (transcribe.py) and the eval (`ozen eval`)
share, so a configuration measured by the eval is the one that runs in meetings.

    uv run asr.py    # eval worker: JSON lines on stdin -> JSON lines on stdout, same order

Request: {"audio": path, "start": s, "duration": s, "words": [hint words], "replace": {wrong: right}, "lang": "he",
          "model": "stock" | "hebrew" (optional: that model in `lang`, no language detection; `ozen compare`)}
         or {"heard": text, "replace": {...}} to only apply corrections (no audio, no model)
Reply:   {"heard": Whisper's text, "text": after replace, "lang": language used,
          "raw": Whisper's text before any filter (only with "model")}
The worker seeds MLX before every clip, so its output depends only on the request.
"""
import collections
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


def loop(text: str) -> bool:
    """"Amen. Amen. Amen. Amen." on noise: Whisper stuck on one short phrase, which the compression-ratio check
    misses when the phrase is short. A phrase said 3+ times that makes up most of the line is that loop.
    Also checked on merged lines, since one "Yeah." per utterance merges into "Yeah. Yeah. Yeah."

    >>> loop("Amen. Amen. Amen. Amen. Yeah."), loop("Yeah. Yeah."), loop("זה קורה קורה קורה.")
    (True, False, False)
    """
    phrases = [p.strip().lower() for p in re.split(r"[.,!?;:]+", text) if p.strip()]
    top = collections.Counter(phrases).most_common(1)
    return bool(top) and top[0][1] >= 3 and top[0][1] / len(phrases) >= 0.6


last_raw = ""


def recognize(clip: np.ndarray, prompt: str, lang: str, hint: list[str] = (), model: str | None = None) -> tuple[str, str]:
    """(text, language). `lang` is used for clips under ~1.5s, where detection is unreliable ("שלום" came out
    as "Shalom"); longer clips pick between the languages actually spoken, since open detection on short
    noisy audio picks random languages and invents words."""
    if model is None and clip.size >= 1.5 * SR:
        detector = ModelHolder.get_model(MODEL, mx.float16)
        mel = log_mel_spectrogram(pad_or_trim(mx.array(clip)), n_mels=detector.dims.n_mels)
        _, probs = detect_language(detector, mel)
        lang = max(LANGS, key=lambda l: probs.get(l, 0))
    repo = {"stock": MODEL, "hebrew": MODELS["he"]}.get(model, MODELS[lang])
    r = mlx_whisper.transcribe(clip, path_or_hf_repo=repo, language=lang, initial_prompt=prompt,
                               condition_on_previous_text=False)
    global last_raw
    last_raw = r["text"].strip()  # before any filter, for `ozen compare` to show what was dropped
    # Drop segments Whisper itself flags as noise; these are the hallucinated lines.
    text = " ".join(
        s["text"].strip()
        for s in r["segments"]
        if s["no_speech_prob"] < 0.5 and s["avg_logprob"] > -0.8 and s["compression_ratio"] < 2.4
    ).strip()
    return ("" if noise(text) or hint_echo(text, hint) or loop(text) else text), lang


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
        heard, lang = recognize(clip, prompt(words), q.get("lang", LANGS[0]), words, q.get("model"))
        reply = {"heard": heard, "text": corrected(heard, q.get("replace", {})), "lang": lang}
        if q.get("model"):
            reply["raw"] = last_raw
        print(json.dumps(reply, ensure_ascii=False), flush=True)
