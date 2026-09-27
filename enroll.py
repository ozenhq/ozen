"""Save a speaker from the current meeting into the voices registry under a real name.

    python3 enroll.py S3 "Dana Levi"

Takes S3's voiceprint from session_speakers.json, merges it into voices/voices/<slug>.json
(weighted by sample count, so repeated enrollments sharpen the print), maps S3 -> name in
names.json for the running transcript, then commits and pushes the registry.
"""
import json
import math
import pathlib
import re
import subprocess
import sys

HERE = pathlib.Path(__file__).parent
REPO = HERE / "voices"
ECAPA = "speechbrain/spkrec-ecapa-voxceleb"

label, name = sys.argv[1], sys.argv[2]
session = json.loads((HERE / "session_speakers.json").read_text())
if label not in session:
    sys.exit(f"{label} not in this meeting; known: {', '.join(session)}")
new = session[label]["embedding"]
count = session[label]["count"]

slug = re.sub(r"[^\w]+", "-", name.strip().lower()).strip("-")
path = REPO / "voices" / f"{slug}.json"
path.parent.mkdir(exist_ok=True)
if path.exists():
    old = json.loads(path.read_text())
    new = [a * old["count"] + b * count for a, b in zip(old["embedding"], new)]
    count = old["count"] + count
norm = math.sqrt(sum(x * x for x in new))
new = [round(x / norm, 6) for x in new]
path.write_text(json.dumps({"name": name, "model": ECAPA, "count": count, "embedding": new}, indent=1) + "\n")

names_path = HERE / "names.json"
names = json.loads(names_path.read_text()) if names_path.exists() else {}
names[label] = name
names_path.write_text(json.dumps(names, ensure_ascii=False, indent=1))

git = lambda *a: subprocess.run(["git", "-C", str(REPO), *a], check=True)
git("add", str(path))
git("commit", "-m", f"Enroll voice: {name} ({count} samples)")
git("push")
print(f"{label} -> {name}: {path.relative_to(HERE)} ({count} samples), pushed")
