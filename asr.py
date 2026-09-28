"""Speech to text for one clip: the Whisper setup the live transcriber (transcribe.py) and the eval (`ozen eval`)
share, so a configuration measured by the eval is the one that runs in meetings.

    uv run asr.py    # eval worker: JSON lines on stdin -> JSON lines on stdout, same order

Request: {"audio": path, "start": s, "duration": s, "words": [hint words], "replace": {wrong: right}, "lang": "he",
          "model": "stock" | "hebrew" (optional: that model in `lang`, no language detection; `ozen compare`)}
         or {"heard": text, "replace": {...}} to only apply corrections (no audio, no model)
Reply:   {"heard": Whisper's text, "text": after replace, "lang": language used,
          "raw": Whisper's text before any filter (only with "model")}
Whisper's temperature fallback samples with a fixed seed per clip, so the output depends only on the request.
"""
import collections
import json
import pathlib
import re
import select
import sys

import numpy as np

SR = 16000
LANGS = ("he", "en")
_whisper = None


def whisper():
    """`ozen whisper` (src/whisper.rs): Whisper large-v3-turbo, stock and ivrit.ai's Hebrew, both kept loaded."""
    global _whisper
    if _whisper is None:
        import subprocess
        _whisper = subprocess.Popen([str(pathlib.Path(__file__).parent / "target/release/ozen"), "whisper"],
                                    stdin=subprocess.PIPE, stdout=subprocess.PIPE)
        # the first start downloads the models (~3.2GB) unless mlx_whisper already did
        ready = select.select([_whisper.stdout], [], [], 3600)[0]
        if not ready or _whisper.stdout.readline() != b"ozen whisper 1\n":  # an older ozen prints its usage
            raise SystemExit("ozen whisper unavailable; rebuild target/release/ozen from this checkout")
    return _whisper


def load_audio(path: str) -> np.ndarray:
    """16 kHz mono float32 through ffmpeg, as mlx_whisper's load_audio did."""
    import subprocess
    out = subprocess.run(["ffmpeg", "-nostdin", "-i", path, "-threads", "0", "-f", "s16le", "-ac", "1",
                          "-acodec", "pcm_s16le", "-ar", str(SR), "-"], capture_output=True)
    if out.returncode:
        raise RuntimeError(f"Failed to load audio: {out.stderr.decode()}")
    return np.frombuffer(out.stdout, np.int16).astype(np.float32) / 32768.0


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


def decode(clip: np.ndarray, prompt: str, lang: str, model: str | None = None) -> tuple[str, str, str]:
    """(text Whisper is confident in, language, raw text before any filter). `model` pins "stock" or "hebrew"
    in `lang` (`ozen compare`). Otherwise `lang` is used for clips under ~1.5s, where detection is unreliable
    ("שלום" came out as "Shalom"); longer clips pick between the languages actually spoken, since open
    detection on short noisy audio picks random languages and invents words. Segments Whisper itself flags as
    noise (the hallucinated lines) are dropped from the text. Runs in `ozen whisper`."""
    w, x = whisper(), np.ascontiguousarray(clip, "<f4")
    try:
        w.stdin.write((json.dumps({"n": len(x), "prompt": prompt, "lang": lang, "model": model}) + "\n").encode())
        w.stdin.write(x.tobytes())
        w.stdin.flush()
        r = json.loads(w.stdout.readline())
    except (OSError, ValueError):  # SystemExit passes the per-chunk handler: ozen restarts the transcriber
        raise SystemExit("ozen whisper stopped")
    return r["text"], r["lang"], r["raw"]


def kept(text: str, hint: list[str]) -> str:
    """The text, or "" when it is noise, the hint echoed back, or a loop."""
    return "" if noise(text) or hint_echo(text, hint) or loop(text) else text


def recognize(clip: np.ndarray, prompt: str, lang: str, hint: list[str] = ()) -> tuple[str, str]:
    """(text, language): what `decode` is confident in, `kept` for the transcript."""
    text, lang, _ = decode(clip, prompt, lang)
    return kept(text, hint), lang


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
        words = q.get("words", [])
        text, lang, raw = decode(clip, prompt(words), q.get("lang", LANGS[0]), q.get("model"))
        heard = kept(text, words)
        reply = {"heard": heard, "text": corrected(heard, q.get("replace", {})), "lang": lang}
        if q.get("model"):
            reply["raw"] = raw
        print(json.dumps(reply, ensure_ascii=False), flush=True)
