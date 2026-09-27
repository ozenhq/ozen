# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy"]
# ///
"""Speaker tagging and voiceprint training.

    uv run train.py tag <line-id> "Dana Levi"   # tag one transcript line (empty name clears), then retrain
    uv run train.py retrain                     # rebuild voiceprints from all tags
    uv run train.py show [N]                    # last N lines with the best known speaker

Retraining makes each person's voiceprint the average of every line tagged as them (kept in the
registry under samples/, so tags accumulate across meetings), relabels untagged lines with the
new prints (labels.json), measures leave-one-out accuracy over the tags (stats.json), and pushes
the registry. The more lines you tag, the better the prints and the accuracy get.
"""
import datetime
import json
import pathlib
import re
import subprocess
import sys

import numpy as np

HERE = pathlib.Path(__file__).parent
REPO = HERE / "voices"
LINES, TAGS, LABELS, STATS = (HERE / n for n in ("lines.jsonl", "tags.json", "labels.json", "stats.json"))
ECAPA = "speechbrain/spkrec-ecapa-voxceleb"
SAME_SPEAKER = 0.4  # same threshold the live transcriber uses


def read(path: pathlib.Path, default):
    try:
        return json.loads(path.read_text())
    except (OSError, ValueError):
        return default


def slug(name: str) -> str:
    return re.sub(r"[^\w]+", "-", name.strip().lower()).strip("-")


def unit(v: np.ndarray) -> np.ndarray:
    return v / np.linalg.norm(v)


def load_lines() -> dict:
    if not LINES.exists():
        return {}
    return {r["id"]: r for r in map(json.loads, LINES.read_text().splitlines()) if r.get("e")}


def retrain() -> None:
    lines, tags = load_lines(), read(TAGS, {})
    (REPO / "voices").mkdir(parents=True, exist_ok=True)
    (REPO / "samples").mkdir(exist_ok=True)

    # samples[name] = {line id: {"w": weight, "e": print}}; registry keeps tags from earlier meetings.
    samples: dict[str, dict] = {}
    for f in (REPO / "voices").glob("*.json"):
        v = read(f, {})
        if v.get("model") != ECAPA:
            continue
        s = read(REPO / "samples" / f.name, None)
        if s is None:  # voice enrolled before tagging existed: keep its print as one weighted sample
            s = {"legacy": {"w": v.get("count", 1), "e": v["embedding"]}}
        samples[v["name"]] = s
    for sid, name in tags.items():  # a retagged or cleared line leaves its old person
        for n, s in samples.items():
            if n != name:
                s.pop(sid, None)
    for sid, name in tags.items():
        if name and sid in lines:
            samples.setdefault(name, {})[sid] = {"w": 1, "e": lines[sid]["e"]}
    samples = {n: s for n, s in samples.items() if s}

    prints = {n: unit(sum(x["w"] * np.array(x["e"]) for x in s.values())) for n, s in samples.items()}
    for n, s in samples.items():
        rec = {"name": n, "model": ECAPA, "count": sum(x["w"] for x in s.values()),
               "embedding": prints[n].round(6).tolist()}
        (REPO / "voices" / f"{slug(n)}.json").write_text(json.dumps(rec, indent=1, ensure_ascii=False) + "\n")
        (REPO / "samples" / f"{slug(n)}.json").write_text(json.dumps(s) + "\n")
    for f in (REPO / "voices").glob("*.json"):  # everyone's tags were cleared: drop the person
        if read(f, {}).get("model") == ECAPA and read(f, {})["name"] not in samples:
            f.unlink()
            (REPO / "samples" / f.name).unlink(missing_ok=True)

    # Leave-one-out: predict each tagged line from prints built without it.
    correct = evaluated = 0
    for n, s in samples.items():
        for sid, x in s.items():
            if sid == "legacy" or len(s) < 2:
                continue
            e = np.array(x["e"])
            cand = dict(prints)
            cand[n] = unit(sum(y["w"] * np.array(y["e"]) for k, y in s.items() if k != sid))
            evaluated += 1
            correct += max(cand, key=lambda k: float(cand[k] @ e)) == n
    stats = {"accuracy": round(correct / evaluated, 3) if evaluated else None, "evaluated": evaluated,
             "tagged": sum(1 for v in tags.values() if v),
             "people": {n: len([k for k in s if k != "legacy"]) for n, s in samples.items()}}
    STATS.write_text(json.dumps(stats, ensure_ascii=False, indent=1))

    labels = {}
    for sid, r in lines.items():
        if sid in tags or not prints:
            continue
        e = np.array(r["e"])
        best = max(prints, key=lambda k: float(prints[k] @ e))
        if float(prints[best] @ e) >= SAME_SPEAKER:
            labels[sid] = best
    LABELS.write_text(json.dumps(labels, ensure_ascii=False))

    git = lambda *a: subprocess.run(["git", "-C", str(REPO), *a], capture_output=True, text=True)
    git("add", "-A", "voices", "samples")
    if git("diff", "--cached", "--quiet").returncode:
        git("commit", "-m", f"Retrain voiceprints: {stats['tagged']} tagged lines, accuracy {stats['accuracy']}")
        if git("push", "-q").returncode:
            print("registry push failed; committed locally", file=sys.stderr)
    print(json.dumps(stats, ensure_ascii=False))


def show(n: int) -> None:
    tags, labels = read(TAGS, {}), read(LABELS, {})
    for r in list(load_lines().values())[-n:]:
        ts = datetime.datetime.fromtimestamp(r["t"]).strftime("%H:%M:%S")
        spk = tags.get(r["id"]) or labels.get(r["id"]) or r["spk"]
        print(f"[{ts}] {spk}{' ✓' if r['id'] in tags else ''} ({r['src']}): {r['text']}")


cmd = sys.argv[1] if len(sys.argv) > 1 else "show"
if cmd == "tag":
    tags = read(TAGS, {})
    tags[sys.argv[2]] = sys.argv[3].strip() if len(sys.argv) > 3 else ""
    TAGS.write_text(json.dumps(tags, ensure_ascii=False, indent=1))
    retrain()
elif cmd == "retrain":
    retrain()
elif cmd == "show":
    show(int(sys.argv[2]) if len(sys.argv) > 2 else 40)
else:
    sys.exit(__doc__)
