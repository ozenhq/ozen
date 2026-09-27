# /// script
# requires-python = ">=3.11"
# dependencies = ["speechbrain", "torchaudio", "mlx-whisper"]
# ///
"""Several people in one utterance: two talking at once, or one cutting in without a pause. The transcriber
splits utterances on pauses, so these used to come out as one line under one speaker. `voices` separates such a
clip into one track per voice (SepFormer, 2 voices, trained on noisy reverberant rooms), each as long as the clip,
so every voice is transcribed and labeled on its own and keeps its timing.

    uv run overlap.py   # self-check on macOS voices: one voice stays whole; overlap and quick turns split in two
"""
import numpy as np
import torch

SR = 16000
SEPFORMER = "speechbrain/sepformer-whamr16k"
WINDOW, HOP = 1.5, 0.75  # seconds; voiceprints this short are noisy, but only decide whether to try separating
MIN_SOURCE = 0.25  # the quieter separated track must be this loud vs the louder one, else it's residue of one voice
_separator = None


def separator():
    global _separator
    if _separator is None:  # loaded on first need; ~100MB download on first use
        from speechbrain.inference.separation import SepformerSeparation
        device = "mps" if torch.backends.mps.is_available() else "cpu"  # ~4x faster than CPU on Apple Silicon
        _separator = SepformerSeparation.from_hparams(
            source=SEPFORMER, savedir=str(__import__("pathlib").Path(__file__).parent / "models/sepformer"),
            run_opts={"device": device})
    return _separator


def rms(x: np.ndarray) -> float:
    return float(np.sqrt(np.mean(x**2))) if x.size else 0.0


def mixed(clip: np.ndarray, embed, same: float) -> bool:
    """Cheap gate before separating: do voiceprints of short windows across the clip disagree?"""
    w, h = int(WINDOW * SR), int(HOP * SR)
    windows = [clip[i:i + w] for i in range(0, len(clip) - w + 1, h)]
    windows = [x for x in windows if rms(x) > 0.3 * rms(clip)]  # windows that are mostly pause say nothing
    if len(windows) < 2:
        return False
    p = np.stack([embed(x) for x in windows])
    return float((p @ p.T).min()) < same


def voices(clip: np.ndarray, embed, same: float) -> list[np.ndarray]:
    """The clip's voices as separate full-length tracks, or just [clip] when it's one voice.
    embed(audio) -> unit voiceprint; same: the same-voice similarity cutoff."""
    if not mixed(clip, embed, same):
        return [clip]
    with torch.no_grad():
        out = separator().separate_batch(torch.from_numpy(clip.astype(np.float32))[None])
    tracks = [t for t in out.squeeze(0).T.cpu().numpy()]
    loud = sorted(rms(t) for t in tracks)
    if loud[-1] == 0 or loud[0] / loud[-1] < MIN_SOURCE:
        return [clip]  # one voice; the other track is residue
    if float(embed(tracks[0]) @ embed(tracks[1])) >= same:
        return [clip]  # one voice split in two
    peak = float(np.abs(clip).max())
    # SepFormer's output level is arbitrary: bring each track back to the clip's level so silence thresholds hold
    return [t * (peak / max(float(np.abs(t).max()), 1e-9)) for t in tracks]


if __name__ == "__main__":
    import pathlib
    import subprocess
    import tempfile

    from mlx_whisper.audio import load_audio
    from speechbrain.inference.speaker import EncoderClassifier

    here = pathlib.Path(__file__).parent
    enc = EncoderClassifier.from_hparams(source="speechbrain/spkrec-ecapa-voxceleb", savedir=str(here / "models/ecapa"),
                                         run_opts={"device": "cpu"})

    def embed(x):
        e = enc.encode_batch(torch.from_numpy(x.astype(np.float32))[None]).squeeze().numpy()
        return e / np.linalg.norm(e)

    tmp = pathlib.Path(tempfile.mkdtemp())

    def say(voice, text):
        f = tmp / f"{voice}.aiff"
        subprocess.run(["say", "-v", voice, "-o", str(f), text], check=True)
        return np.array(load_audio(str(f)))

    a = say("Samantha", "The launch is moved to next Tuesday because the payments team needs more time for testing.")
    b = say("Daniel", "I think we should tell the customers today, before they hear about it from someone else.")
    c = say("Fred", "Can everyone see my screen now?")
    at = lambda x, s, n: np.pad(x, (int(s * SR), max(0, n - len(x) - int(s * SR))))[:n]  # noqa: E731
    n = len(b) + 2 * SR
    same = 0.26  # the calibrated cutoff at the time of writing (voices/config.json)
    cases = {
        "one voice": (a, 1),
        "two at once": (at(a, 0, n) + at(b, 2, n), 2),
        "quick turns": (np.concatenate([a, c]), 2),
    }
    for name, (clip, want) in cases.items():
        got = voices(clip, embed, same)
        print(f"{name}: {len(got)} voice(s)")
        assert len(got) == want, name
        if want == 2:
            refs = [a, b] if name == "two at once" else [a, c]
            sims = sorted(max(float(embed(t) @ embed(r)) for t in got) for r in refs)
            print(f"  each speaker's best track match: {[round(s, 2) for s in sims]}")
            assert sims[0] >= same, "a separated track should sound like its speaker"
    print("ok")
