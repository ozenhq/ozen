# /// script
# requires-python = ">=3.11"
# dependencies = ["numpy"]
# ///
"""Speaker tagging and voiceprint training.

    uv run train.py tag <line-id> "Dana Levi"   # tag one transcript line (empty name clears), then retrain
    uv run train.py retrain                     # rebuild voiceprints from all tags
    uv run train.py show [N]                    # last N lines with the best known speaker

Retraining makes each person's voiceprint the average of every line tagged as them (kept in the
registry under samples/, so tags accumulate across meetings), measures leave-one-out accuracy
over the tags, calibrates the same-voice threshold from them (config.json, read live by the
transcriber), relabels untagged lines with a confidence, queues the least certain ones for you
to tag next (stats.json "review"), logs the trend (history.jsonl), and pushes the registry.
That is the loop: tag what it asks -> better prints and threshold -> fewer uncertain lines.
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
DEFAULT_THRESHOLD = 0.4  # until there are enough tags to calibrate one
UNSURE = 0.08  # a line this close to the threshold, or to a second person, gets queued for review
REVIEW_MAX = 50


def calibrate(genuine: list[float], impostor: list[float]) -> float:
    """Match threshold that best separates same-person from different-person similarities in the tags."""
    if len(genuine) < 3 or len(impostor) < 3:
        return DEFAULT_THRESHOLD
    g, i = np.array(genuine), np.array(impostor)
    grid = np.arange(0.25, 0.66, 0.01)
    score = [((g >= t).mean() + (i < t).mean()) / 2 for t in grid]  # balanced accuracy
    return round(float(grid[int(np.argmax(score))]), 2)


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


def git(*a: str) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(["git", "-C", str(REPO), *a], capture_output=True, text=True, timeout=60)
    except subprocess.TimeoutExpired:  # offline: a sync failure must never lose the tag
        return subprocess.CompletedProcess(a, 1)


def retrain(retry: bool = True) -> None:
    # Start from the newest registry. Local registry state is disposable: this machine's tags live in
    # tags.json and lines.jsonl (append-only), so rebuilding re-applies them on top of everyone else's.
    if git("fetch", "-q").returncode == 0:
        git("reset", "-q", "--hard", "@{u}")
    else:
        print("registry fetch failed; training on the local copy", file=sys.stderr)
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

    # Leave-one-out: predict each tagged line from prints built without it. The same pass collects
    # genuine (own print) and impostor (other prints) similarities to calibrate the match threshold.
    correct = evaluated = 0
    genuine, impostor = [], []
    for n, s in samples.items():
        for sid, x in s.items():
            if sid == "legacy" or len(s) < 2:
                continue
            e = np.array(x["e"])
            cand = dict(prints)
            cand[n] = unit(sum(y["w"] * np.array(y["e"]) for k, y in s.items() if k != sid))
            evaluated += 1
            correct += max(cand, key=lambda k: float(cand[k] @ e)) == n
            genuine.append(float(cand[n] @ e))
            impostor += [float(cand[m] @ e) for m in cand if m != n]
    threshold = calibrate(genuine, impostor)
    config = {"same_speaker": threshold, "calibrated_on": {"genuine": len(genuine), "impostor": len(impostor)}}
    (REPO / "config.json").write_text(json.dumps(config, indent=1) + "\n")

    labels, review = {}, []
    for sid, r in lines.items():
        if sid in tags or not prints:
            continue
        e = np.array(r["e"])
        ranked = sorted(((float(prints[k] @ e), k) for k in prints), reverse=True)
        best, name = ranked[0]
        margin = best - ranked[1][0] if len(ranked) > 1 else best - threshold
        # Unsure = near the threshold or nearly tied between two people: tagging these teaches the most.
        doubt = min(abs(best - threshold), margin)
        unsure = doubt < UNSURE
        labels[sid] = {"spk": name if best >= threshold else None, "sim": round(best, 3),
                       "margin": round(margin, 3), "unsure": unsure}
        if unsure:
            review.append((doubt, sid))
    LABELS.write_text(json.dumps(labels, ensure_ascii=False))

    stats = {"accuracy": round(correct / evaluated, 3) if evaluated else None, "evaluated": evaluated,
             "tagged": sum(1 for v in tags.values() if v), "threshold": threshold,
             "review": [sid for _, sid in sorted(review)][:REVIEW_MAX], "unsure": len(review),
             "people": {n: len([k for k in s if k != "legacy"]) for n, s in samples.items()}}
    hist_path = REPO / "history.jsonl"
    hist = [json.loads(x) for x in hist_path.read_text().splitlines()] if hist_path.exists() else []
    point = {k: stats[k] for k in ("tagged", "evaluated", "accuracy", "threshold")}
    if not hist or {k: hist[-1].get(k) for k in point} != point:
        hist.append({"at": datetime.datetime.now().isoformat(timespec="seconds"), **point})
        with hist_path.open("a") as fh:
            fh.write(json.dumps(hist[-1]) + "\n")
    first = next((h["accuracy"] for h in hist if h["accuracy"] is not None), None)
    stats["accuracy_first"] = first
    STATS.write_text(json.dumps(stats, ensure_ascii=False, indent=1))

    git("add", "-A", "voices", "samples", "config.json", "history.jsonl")
    if git("diff", "--cached", "--quiet").returncode:
        git("commit", "-m", f"Retrain voiceprints: {stats['tagged']} tagged lines, accuracy {stats['accuracy']}, "
                            f"threshold {threshold}")
    if git("push", "-q").returncode:
        if retry:  # another machine pushed since the fetch: rebuild on top of it, once
            return retrain(retry=False)
        print("registry push failed; committed locally, rebuilt and pushed on the next retrain", file=sys.stderr)
    print(json.dumps({k: v for k, v in stats.items() if k != "review"}, ensure_ascii=False))


def show(n: int) -> None:
    tags, labels = read(TAGS, {}), read(LABELS, {})
    for r in sorted(load_lines().values(), key=lambda r: r["t"])[-n:]:
        ts = datetime.datetime.fromtimestamp(r["t"]).strftime("%H:%M:%S")
        lab = labels.get(r["id"]) or {}
        spk = tags.get(r["id"]) or lab.get("spk") or r["spk"]
        mark = " ✓" if tags.get(r["id"]) else (" ?" if lab.get("unsure") else "")
        print(f"[{ts}] {spk}{mark} ({r['src']}): {r['text']}")


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
