# /// script
# requires-python = ">=3.11"
# dependencies = ["speechbrain", "torchaudio", "mlx-whisper", "clearvoice"]
# ///
"""Several people in one utterance: two talking at once, or one cutting in without a pause. The transcriber
splits utterances on pauses, so these used to come out as one line under one speaker. `voices` separates such a
clip into one track per voice (MossFormer2, 2 voices), each as long as the clip, so every voice is transcribed and
labeled on its own and keeps its timing. SepFormer (speechbrain) was tried first: fine on clean mixes, but through a
real room into the laptop mic it split single voices into two loud tracks and returned two copies of the louder of two
voices; MossFormer2 kept single voices whole and recovered both voices in every real room pair tried.

    uv run overlap.py   # self-check on macOS voices: one voice stays whole; overlap and quick turns give each voice
"""
import threading

import numpy as np
import torch

SR = 16000
SILENCE_RMS = 0.003  # below this Whisper hallucinates ("Thank you."), so skip
# A frame counts as speech only above -38 dBFS. The per-chunk relative threshold alone let steady room noise
# (measured -47 dB median, -41.7 dB max frame) through as one 15s "utterance" per chunk, and on it the Hebrew
# model hallucinates Knesset openers ("אדוני היושב-ראש…") and "Okay. Okay.". Speech from ~0.5m measured -31 dB
# at its 10th percentile frame, so the floor sits between the two.
SPEECH_RMS = 0.0125
MODEL = "MossFormer2_SS_16K"  # ~640MB, downloaded to models/ by preload()
WINDOW, HOP = 1.5, 0.75  # seconds; voiceprints this short are noisy, but only decide whether to try separating
# Both checks measured on real voices (LibriSpeech mixes, and pairs played into a room and recorded by ozen):
MIN_SOURCE = 0.25  # the quieter track's share of the clip vs the louder one's, below which it's residue of one voice
# (single voices reaching separation: <=0.21; two voices: 0.2-1.0)
SAME_TRACKS = 0.6  # tracks this alike are one voice split in two. Not the same-voice cutoff: separated tracks leak
# into each other, so two people's tracks score up to ~0.65; one voice's two tracks stayed under 0.35
LEAK = 0.3  # a track's frame this much quieter than the other track's frame is leak of the other voice
TRACK_MIN = 0.6  # seconds; on real meetings (AMI) shorter separated pieces were mostly invented words ("Amen.")
_separator = None


def separator():
    global _separator
    if _separator is None:  # runs on the GPU (MPS) when there is one
        import pathlib

        from clearvoice.network_wrapper import network_wrapper
        from clearvoice.networks import CLS_MossFormer2_SS_16K
        # Built by hand rather than through ClearVoice(), whose config keeps checkpoints under a cwd-relative path
        w = network_wrapper()
        w.model_name = MODEL
        w.load_args_ss()
        w.args.task, w.args.network = "speech_separation", MODEL
        w.args.checkpoint_dir = str(pathlib.Path(__file__).parent / "models/checkpoints" / MODEL)
        # Its default decodes in 2s windows whose track order can flip (turns() splits those). Decoding whole clips
        # keeps the order but merged more real room pairs into one voice: word recall 0.67 vs 0.73 on the room set
        _separator = CLS_MossFormer2_SS_16K(w.args)
    return _separator


def preload() -> None:
    """Download and load the separator in the background. Until it's ready, clips stay whole: a first run's
    download takes minutes, and the live transcript must not wait for it."""
    def load():
        try:
            separator()
        except Exception as e:  # offline on first run: keep transcribing without separating
            print(f"voice separation unavailable: {e}", flush=True)
    threading.Thread(target=load, daemon=True).start()


def separate(clip: np.ndarray) -> list[np.ndarray]:
    with torch.no_grad():
        out = separator().decode_data(clip[None].astype(np.float32))
    return [np.asarray(t, dtype=np.float32).reshape(-1)[: len(clip)] for t in out]


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


def tracks(clip: np.ndarray, embed, same: float) -> list[np.ndarray]:
    """The clip's voices as separate full-length tracks, or just [clip] when it's one voice.
    embed(audio) -> unit voiceprint; same: the same-voice similarity cutoff."""
    if _separator is None or not mixed(clip, embed, same):
        return [clip]  # not loaded yet (see preload), or one voice
    ts = separate(clip)
    # Output levels are normalized, so measure each track's share of the clip: fit clip ~ a*t1 + b*t2
    share = np.linalg.lstsq(np.stack(ts, 1), clip, rcond=None)[0]
    loud = sorted(rms(k * t) for k, t in zip(share, ts))
    if loud[-1] == 0 or loud[0] / loud[-1] < MIN_SOURCE:
        return [clip]  # one voice; the other track is residue
    if float(embed(ts[0]) @ embed(ts[1])) >= SAME_TRACKS:
        return [clip]  # one voice split in two
    return [k * t for k, t in zip(share, ts)]  # each at its level in the clip, so silence thresholds hold


def voices(clip: np.ndarray, embed, same: float) -> list[tuple[float, np.ndarray]]:
    """(offset, piece) per voice turn: the clip itself, or, when several people talk in it, each separated track
    split on its own pauses and where its voice changes, in time order (overlapping pieces overlap in time)."""
    ts = tracks(clip, embed, same)
    if len(ts) == 1:
        return [(0.0, clip)]
    print(f"{len(ts)} voices at once in {len(clip) / SR:.1f}s", flush=True)
    return sorted((p for t in unleak(ts) for u in utterances(t, min_len=TRACK_MIN) for p in turns(*u, embed, same)), key=lambda p: p[0])


def unleak(ts: list[np.ndarray], frame: int = 480) -> list[np.ndarray]:
    """Silence each track where the other one is much louder: there it only carries leak of the other voice,
    which Whisper would transcribe a second time. Where both are loud, both people are talking."""
    n = min(len(t) for t in ts) // frame * frame
    r = [np.sqrt((t[:n].reshape(-1, frame) ** 2).mean(1)) for t in ts]
    out = []
    for k, t in enumerate(ts):
        keep = np.repeat(r[k] >= LEAK * r[1 - k], frame)
        out.append(np.concatenate([t[:n] * keep, t[n:]]))
    return out


def turns(offset: float, piece: np.ndarray, embed, same: float) -> list[tuple[float, np.ndarray]]:
    """Split a separated piece where its voice changes. The separator can also swap tracks with no pause, e.g. when
    the second person starts talking, leaving the first one's opening and the second one's words in one track."""
    w, h = int(WINDOW * SR), int(HOP * SR)
    starts = range(0, len(piece) - w + 1, h)
    if len(starts) < 2:
        return [(offset, piece)]
    p = np.stack([embed(piece[i:i + w]) for i in starts])
    # ponytail: best single change point, then recurse on both sides; O(windows^2) dot products, no extra embeds
    def side(x):
        m = x.mean(0)
        return m / np.linalg.norm(m)
    k = min(range(1, len(p)), key=lambda k: float(side(p[:k]) @ side(p[k:])))
    if float(side(p[:k]) @ side(p[k:])) >= same:
        return [(offset, piece)]
    cut = starts[k] + (w - h) // 2  # middle of the stretch the windows on both sides of the change share
    return turns(offset, piece[:cut], embed, same) + turns(offset + cut / SR, piece[cut:], embed, same)


def utterances(audio: np.ndarray, frame=0.03, max_gap=0.35, min_len=0.3):
    """Split on pauses so each piece is one speaker turn; Whisper segments span speaker changes."""
    n = int(frame * SR)
    if len(audio) < n:
        return  # shorter than one frame (a fragment cut off at stop): nothing to split, and no rms to rank
    rms = np.sqrt(np.mean(audio[: len(audio) // n * n].reshape(-1, n) ** 2, axis=1))
    voiced = np.flatnonzero(rms > max(SPEECH_RMS, 0.15 * np.percentile(rms, 95)))
    if not voiced.size:
        return
    groups = np.split(voiced, np.flatnonzero(np.diff(voiced) * frame > max_gap) + 1)
    for g in groups:
        start, end = max(0, g[0] - 3) * n, min(len(audio), (g[-1] + 4) * n)  # ~0.1s padding
        if (end - start) / SR >= min_len:
            yield start / SR, audio[start:end]


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
    separator()

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
    cases = {  # clip, the speakers in it
        "one voice": (a, [a]),
        "two at once": (at(a, 0, n) + at(b, 2, n), [a, b]),
        "quick turns": (np.concatenate([a, c]), [a, c]),
    }
    for name, (clip, refs) in cases.items():
        pieces = voices(clip, embed, same)
        # every piece is one of the speakers, and every speaker is heard in some piece
        best = [max(range(len(refs)), key=lambda k: float(embed(p) @ embed(refs[k]))) for _, p in pieces if len(p) >= SR]
        print(f"{name}: {len(pieces)} piece(s), speakers heard: {sorted(set(best))}")
        assert sorted(set(best)) == list(range(len(refs))), name
    print("ok")
