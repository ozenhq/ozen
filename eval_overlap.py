"""Score overlap.py on real speech: windows where two people talk at once, and solo windows, transcribed through
asr.recognize (the live transcriber's Whisper) with utterances kept whole vs separated by overlap.voices.

    uv run --group eval eval_overlap.py [ami] [he] [call] [--n 20]   # default: all three sets, 20 overlap windows each

- ami:  real meetings, one far-field room mic, real overlaps (AMI, meetings EN2002a-c)
- he:   real Hebrew speakers (FLEURS he_il), pairs of different people mixed 1s apart
- call: real English speakers (LibriSpeech), pairs mixed 1s apart, through Opus 24kbps like a video call

Each set downloads once to eval/data/ (250-450MB). Prints word recall and extra words vs the reference text per
set: separation should raise recall on overlaps and leave solos alone. Whisper is seeded per clip, so a rerun
prints the same table, and its text is cached per clip (eval/data/asr-cache.jsonl), so a rerun after tuning
overlap.py only transcribes the pieces that changed (and MossFormer2's tracks are cached in eval/data/separated/).
"""
import hashlib
import io
import json
import pathlib
import random
import re
import subprocess
import sys
import tempfile
import urllib.request
from collections import Counter

import mlx.core as mx
import numpy as np
import pyarrow.parquet as pq
import soundfile as sf
import torch
from speechbrain.inference.speaker import EncoderClassifier

import asr
import overlap

HERE = pathlib.Path(__file__).parent
DATA = HERE / "eval/data"
SR = 16000
SETS = {  # name: (parquet url, language)
    "ami": ("https://huggingface.co/datasets/edinburghcstr/ami/resolve/main/sdm/test-00000-of-00004.parquet", "en"),
    "he": ("https://huggingface.co/api/datasets/google/fleurs/parquet/he_il/test/0.parquet", "he"),
    "call": ("https://huggingface.co/api/datasets/openslr/librispeech_asr/parquet/all/test.clean/0.parquet", "en"),
}
try:
    SAME = float(json.loads((HERE / "voices/config.json").read_text())["same_speaker"])
except (OSError, ValueError, KeyError):
    SAME = 0.26  # the calibrated cutoff when this was written
VOCAB = [w.strip() for w in re.split(r"[,\n]", (HERE / "vocab.txt").read_text()) if w.strip()]

encoder = EncoderClassifier.from_hparams(source="speechbrain/spkrec-ecapa-voxceleb", savedir=str(HERE / "models/ecapa"),
                                         run_opts={"device": "cpu"})


def embed(x: np.ndarray) -> np.ndarray:
    e = encoder.encode_batch(torch.from_numpy(np.ascontiguousarray(x, np.float32))[None]).squeeze().numpy()
    return e / np.linalg.norm(e)


def rows(name: str) -> list[dict]:
    f = DATA / f"{name}.parquet"
    if not f.exists():
        DATA.mkdir(parents=True, exist_ok=True)
        print(f"downloading {name} set to {f}", flush=True)
        urllib.request.urlretrieve(SETS[name][0], f.with_suffix(".part"))
        f.with_suffix(".part").rename(f)
    return pq.read_table(f).to_pylist()


def audio(r: dict) -> np.ndarray:
    x = sf.read(io.BytesIO(r["audio"]["bytes"]), dtype="float32")[0]
    return x if x.ndim == 1 else x.mean(1)


def text(r: dict) -> str:
    return r.get("text") or r["transcription"]


def mix(a: np.ndarray, b: np.ndarray) -> np.ndarray:
    """b starts 1s into a, at a's level."""
    b = b / max(np.abs(b).max(), 1e-9) * np.abs(a).max()
    n = max(len(a), len(b) + SR)
    return np.pad(a, (0, n - len(a))) + np.pad(b, (SR, n - len(b) - SR))


def opus(x: np.ndarray) -> np.ndarray:
    """What a call's codec does to the audio."""
    with tempfile.TemporaryDirectory() as d:
        sf.write(f"{d}/a.wav", x, SR)
        for cmd in (["-i", f"{d}/a.wav", "-c:a", "libopus", "-b:a", "24k", "-application", "voip", f"{d}/a.ogg"],
                    ["-i", f"{d}/a.ogg", "-ar", str(SR), "-ac", "1", f"{d}/b.wav"]):
            subprocess.run(["ffmpeg", "-loglevel", "error", "-y", *cmd], check=True)
        return sf.read(f"{d}/b.wav", dtype="float32")[0]


def joined(a: dict, b: dict) -> tuple[np.ndarray, str]:
    """AMI segment a, then b's audio past a's end (same mic), and both texts."""
    x = audio(a)
    if b["end_time"] > a["end_time"]:
        x = np.concatenate([x, audio(b)[int((a["end_time"] - b["begin_time"]) * SR):]])
    return x, text(a) + " " + text(b)


def windows(name: str, n: int) -> tuple[list, list]:
    """(overlap windows, solo windows) as (audio, reference text)."""
    rs = rows(name)
    random.seed(0)
    if name == "ami":  # real overlaps: two speakers' segments overlapping >= 1s, nobody else in the window
        rs.sort(key=lambda r: (r["meeting_id"], r["begin_time"]))
        pairs, solos = [], []
        for i, a in enumerate(rs):
            near = [c for c in rs[max(0, i - 30):i + 30] if c is not a and c["meeting_id"] == a["meeting_id"]
                    and c["begin_time"] < a["end_time"] and c["end_time"] > a["begin_time"]]
            if not near and 3 <= a["end_time"] - a["begin_time"] <= 10:
                solos.append((a,))
            for b in near:
                lo, hi = a["begin_time"], max(a["end_time"], b["end_time"])
                others = [c for c in near if c is not b and c["begin_time"] < hi and c["end_time"] > lo]
                if (b["begin_time"] >= lo and b["speaker_id"] != a["speaker_id"] and not others
                        and min(a["end_time"], b["end_time"]) - b["begin_time"] >= 1 and 3 <= hi - lo <= 12):
                    pairs.append((a, b))
        pairs, solos = random.sample(pairs, min(n, len(pairs))), random.sample(solos, min(n, len(solos)))
        return [joined(*p) for p in pairs], [(audio(a), text(a)) for a, in solos]  # decode only the sampled ones
    # read speech: pair utterances by different people (voiceprints far apart; FLEURS has no speaker ids)
    pool = [r for r in random.sample(rs[::5], min(len(rs[::5]), 4 * n)) if 3 * SR < len(audio(r)) < 12 * SR]
    prints = [embed(audio(r)) for r in pool]
    used, pairs = set(), []
    for i in range(len(pool)):
        j = next((j for j in range(i + 1, len(pool)) if j not in used and float(prints[i] @ prints[j]) < 0.2), None)
        if i not in used and j is not None and len(pairs) < n:
            pairs.append((mix(audio(pool[i]), audio(pool[j])), text(pool[i]) + " " + text(pool[j])))
            used |= {i, j}
    solos = [(audio(r), text(r)) for k, r in enumerate(pool) if k not in used][:n]
    if name == "call":
        pairs, solos = [(opus(x), t) for x, t in pairs], [(opus(x), t) for x, t in solos]
    return pairs, solos


CACHE = DATA / "asr-cache.jsonl"  # Whisper's text per clip: a rerun after tuning overlap.py only transcribes new pieces
CONTEXT = f"{VOCAB}|{hashlib.sha1((HERE / 'asr.py').read_bytes()).hexdigest()}"
cache = {}
if CACHE.exists():
    for line in CACHE.read_text().splitlines():
        cache.update([json.loads(line)])


SEPARATED = DATA / "separated"  # MossFormer2's raw tracks per clip; overlap.py's checks and splitting still run live
separate = overlap.separate


def cached_separate(clip: np.ndarray) -> list[np.ndarray]:
    f = SEPARATED / f"{hashlib.sha1(clip.astype(np.float32).tobytes()).hexdigest()}.npy"
    if not f.exists():
        SEPARATED.mkdir(parents=True, exist_ok=True)
        np.save(f, np.stack(separate(clip)))
    return list(np.load(f))


overlap.separate = cached_separate


def recognize(clip: np.ndarray, lang: str) -> str:
    if len(clip) < 0.3 * SR:
        return ""
    key = hashlib.sha1(clip.astype(np.float32).tobytes() + f"{lang}|{CONTEXT}".encode()).hexdigest()
    if key not in cache:
        mx.random.seed(0)  # as asr.py's worker: the same clip always gives the same text
        cache[key] = asr.recognize(clip, asr.prompt(VOCAB), lang, VOCAB)[0]
        with CACHE.open("a") as f:
            f.write(json.dumps([key, cache[key]], ensure_ascii=False) + "\n")
    return cache[key]


def words(s: str) -> Counter:
    return Counter(re.sub(r"[^\w' ]", " ", s.lower()).split())


def score(cases: list, lang: str) -> dict:
    tally = {"whole": [0, 0, 0], "separated": [0, 0, 0], "split": 0}
    for x, ref in cases:
        x = x / max(np.abs(x).max(), 1e-9) * 0.5  # a close mic's level; the sets' gains are arbitrary
        clips = [c for _, c in overlap.utterances(x)]
        pieces = [p for c in clips for _, p in overlap.voices(c, embed, SAME)]
        tally["split"] += len(pieces) > len(clips)
        want = words(ref)
        for way, parts in (("whole", clips), ("separated", pieces)):
            got = words(" ".join(recognize(p, lang) for p in parts))
            t = tally[way]
            t[0] += sum(min(got[w], k) for w, k in want.items())
            t[1] += sum(want.values())
            t[2] += sum(max(0, k - want[w]) for w, k in got.items())
    return tally


if __name__ == "__main__":
    args = sys.argv[1:]
    n = int(args[args.index("--n") + 1]) if "--n" in args else 20
    names = [a for a in args if a in SETS] or list(SETS)
    overlap.separator()
    print(f"{'set':6} {'windows':9} {'n':>3} {'split':>5}  recall whole -> separated   extra words whole -> separated")
    for name in names:
        pairs, solos = windows(name, n)
        for kind, cases in (("overlap", pairs), ("solo", solos)):
            t = score(cases, SETS[name][1])
            (hw, nw, ew), (hs, ns, es) = t["whole"], t["separated"]
            print(f"{name:6} {kind:9} {len(cases):3} {t['split']:5}  {hw / nw:.2f} -> {hs / ns:.2f}"
                  f"{'':15}{ew} -> {es}", flush=True)
