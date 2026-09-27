# /// script
# requires-python = ">=3.11"
# dependencies = ["mlx-whisper"]
# ///
"""Compare transcription setups on the last real chunks kept in recent/ (OZEN_KEEP_AUDIO).

    uv run eval.py [N]    # N most recent chunks with speech (default 6)

Prints each chunk's text from the stock model, the Hebrew model, and the Hebrew model with the
vocab.txt hint, so a model or prompt change is judged on your own speech, not on synthetic audio.
"""
import pathlib
import re
import sys

import mlx_whisper
import numpy as np
from mlx_whisper.audio import load_audio

HERE = pathlib.Path(__file__).parent
STOCK = "mlx-community/whisper-large-v3-turbo"
HEBREW = "mlx-community/ivrit-ai-whisper-large-v3-turbo-mlx"
vocab = ", ".join(w.strip() for w in re.split(r"[,\n]", (HERE / "vocab.txt").read_text()) if w.strip())

n = int(sys.argv[1]) if len(sys.argv) > 1 else 6
speech = [f for f in sorted((HERE / "recent").glob("*.wav"))
          if np.sqrt(np.mean(np.array(load_audio(str(f))) ** 2)) > 0.006][-n:]
if not speech:
    sys.exit("no speech in recent/ yet: talk for a bit, then rerun")
for f in speech:
    audio = np.array(load_audio(str(f)))
    print(f"== {f.name}")
    for label, model, prompt in [("stock", STOCK, None), ("hebrew", HEBREW, None), ("hebrew+vocab", HEBREW, vocab)]:
        r = mlx_whisper.transcribe(audio, path_or_hf_repo=model, language="he", initial_prompt=prompt,
                                   condition_on_previous_text=False)
        print(f"  {label:13s}{r['text'].strip()}", flush=True)
