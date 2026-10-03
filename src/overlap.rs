//! Speech turns in a chunk, and several people in one turn: two talking at once, or one cutting in without a pause.
//! The transcriber splits a chunk on pauses into utterances; an utterance whose voiceprints disagree across it is
//! separated into one track per voice (src/separate.rs, MossFormer2, 2 voices), and each track is split on its own
//! pauses and where its voice changes, so every voice is transcribed and labeled on its own and keeps its timing.
//! SepFormer was tried first: fine on clean mixes, but through a real room into the laptop mic it split single voices
//! into two loud tracks; MossFormer2 kept single voices whole and recovered both voices in every real room pair tried.
use crate::separate::Separator;

pub const SR: usize = 16000;
pub const SILENCE_RMS: f32 = 0.003; // below this Whisper hallucinates ("Thank you."), so skip
// A frame counts as speech only above -38 dBFS. The per-chunk relative threshold alone let steady room noise
// (measured -47 dB median, -41.7 dB max frame) through as one 15s "utterance" per chunk, and on it the Hebrew
// model hallucinates Knesset openers ("אדוני היושב-ראש…") and "Okay. Okay.". Speech from ~0.5m measured -31 dB
// at its 10th percentile frame, so the floor sits between the two.
const SPEECH_RMS: f32 = 0.0125;
// A louder room (fans spinning up under load) moved that noise to -38 dB median, right on SPEECH_RMS, so over half
// its frames passed and every 15s mic chunk came back as "Okay.". Speech must also be 6 dB over the noise floor.
const NOISE_MARGIN: f32 = 2.0;
const WINDOW: usize = SR * 3 / 2; // 1.5s; voiceprints this short are noisy, but only decide whether to try separating
const HOP: usize = SR * 3 / 4;
// Both checks measured on real voices (LibriSpeech mixes, and pairs played into a room and recorded by ozen):
// MIN_SOURCE and LEAK tuned with eval_overlap.py (all sets, 20 windows each): 0.35/0.1 beat 0.25/0.3 in every set,
// with more words recovered and fewer invented (overlaps: +355 words, +36 extra words vs +324, +90)
const MIN_SOURCE: f32 = 0.35; // the quieter track's share of the clip vs the louder one's, below which it's residue
// of one voice (single voices reaching separation: <=0.21; two voices: 0.2-1.0)
const SAME_TRACKS: f32 = 0.6; // tracks this alike are one voice split in two. Not the same-voice cutoff: separated
// tracks leak into each other, so two people's tracks score up to ~0.65; one voice's two tracks stayed under 0.35
const LEAK: f32 = 0.1; // a track's frame this much quieter than the other track's frame is leak of the other voice
const TRACK_MIN: f64 = 0.6; // seconds; on real meetings (AMI) shorter separated pieces were mostly invented words
const FRAME: usize = 480; // 30 ms

/// embed(audio) -> unit voiceprint.
pub type Embed<'a> = dyn FnMut(&[f32]) -> Vec<f32> + 'a;

pub fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt() as f32
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn unit(mut v: Vec<f32>) -> Vec<f32> {
    let n = dot(&v, &v).sqrt();
    if n > 0.0 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

/// RMS of each whole 30 ms frame.
pub fn frame_rms(audio: &[f32]) -> Vec<f32> {
    audio
        .as_chunks::<FRAME>()
        .0
        .iter()
        .map(|f| rms(f))
        .collect()
}

/// numpy's default percentile: linear interpolation between the closest ranks.
pub fn percentile(v: &[f32], q: f64) -> f32 {
    let mut s = v.to_vec();
    s.sort_by(f32::total_cmp);
    let pos = q / 100.0 * (s.len() - 1) as f64;
    let (lo, hi) = (pos.floor() as usize, pos.ceil() as usize);
    s[lo] + (s[hi] - s[lo]) * (pos - lo as f64) as f32
}

/// Split on pauses so each piece is one speaker turn; Whisper segments span speaker changes. `floor` is the
/// source's learned background level: a frame is speech only at NOISE_MARGIN times it or more.
/// (start seconds, piece) with ~0.1s padding.
pub fn utterances(audio: &[f32], floor: f32, min_len: f64) -> Vec<(f64, &[f32])> {
    let r = frame_rms(audio);
    if r.is_empty() {
        return vec![]; // shorter than one frame (a fragment cut off at stop)
    }
    let cut = SPEECH_RMS
        .max(NOISE_MARGIN * floor)
        .max(0.15 * percentile(&r, 95.0));
    let voiced: Vec<usize> = (0..r.len()).filter(|i| r[*i] > cut).collect();
    let mut groups: Vec<(usize, usize)> = vec![];
    for &i in &voiced {
        match groups.last_mut() {
            Some((_, last)) if (i - *last) as f64 * 0.03 <= 0.35 => *last = i,
            _ => groups.push((i, i)),
        }
    }
    groups
        .into_iter()
        .filter_map(|(first, last)| {
            let (s, e) = (
                first.saturating_sub(3) * FRAME,
                audio.len().min((last + 4) * FRAME),
            );
            ((e - s) as f64 / SR as f64 >= min_len).then(|| (s as f64 / SR as f64, &audio[s..e]))
        })
        .collect()
}

fn windows(clip: &[f32]) -> impl Iterator<Item = usize> {
    (0..(clip.len() + 1).saturating_sub(WINDOW)).step_by(HOP)
}

/// Cheap gate before separating: do voiceprints of short windows across the clip disagree?
fn mixed(clip: &[f32], embed: &mut Embed, same: f32) -> bool {
    let loud = 0.3 * rms(clip);
    let prints: Vec<Vec<f32>> = windows(clip)
        .map(|i| &clip[i..i + WINDOW])
        .filter(|x| rms(x) > loud) // windows that are mostly pause say nothing
        .map(&mut *embed)
        .collect();
    prints.len() >= 2
        && prints
            .iter()
            .any(|a| prints.iter().any(|b| dot(a, b) < same))
}

/// The clip's voices as separate full-length tracks, or just [clip] when it's one voice.
fn tracks(clip: &[f32], sep: Option<&Separator>, embed: &mut Embed, same: f32) -> Vec<Vec<f32>> {
    let Some(sep) = sep.filter(|_| mixed(clip, embed, same)) else {
        return vec![clip.to_vec()]; // not loaded yet, or one voice
    };
    let ts = match sep.separate(clip) {
        Ok(ts) => ts,
        Err(e) => {
            println!("separation failed: {e}");
            return vec![clip.to_vec()];
        }
    };
    // Output levels are normalized, so measure each track's share of the clip: least squares clip ~ a*t1 + b*t2
    let d = |a: &[f32], b: &[f32]| {
        a.iter()
            .zip(b)
            .map(|(x, y)| *x as f64 * *y as f64)
            .sum::<f64>()
    };
    let (aa, ab, bb) = (d(&ts[0], &ts[0]), d(&ts[0], &ts[1]), d(&ts[1], &ts[1]));
    let (ac, bc) = (d(&ts[0], clip), d(&ts[1], clip));
    let det = aa * bb - ab * ab;
    if det.abs() <= 1e-9 * aa * bb {
        return vec![clip.to_vec()]; // the tracks are one signal: one voice
    }
    let share = [(ac * bb - bc * ab) / det, (bc * aa - ac * ab) / det];
    let mut loud: Vec<f32> = share
        .iter()
        .zip(&ts)
        .map(|(k, t)| k.abs() as f32 * rms(t))
        .collect();
    loud.sort_by(f32::total_cmp);
    if loud[1] == 0.0 || loud[0] / loud[1] < MIN_SOURCE {
        return vec![clip.to_vec()]; // one voice; the other track is residue
    }
    if dot(&embed(&ts[0]), &embed(&ts[1])) >= SAME_TRACKS {
        return vec![clip.to_vec()]; // one voice split in two
    }
    // each at its level in the clip, so silence thresholds hold
    share
        .iter()
        .zip(ts)
        .map(|(k, t)| t.iter().map(|v| v * *k as f32).collect())
        .collect()
}

/// (offset, piece) per voice turn: the clip itself, or, when several people talk in it, each separated track split
/// on its own pauses and where its voice changes, in time order (overlapping pieces overlap in time).
pub fn voices(
    clip: &[f32],
    sep: Option<&Separator>,
    embed: &mut Embed,
    same: f32,
) -> Vec<(f64, Vec<f32>)> {
    let ts = tracks(clip, sep, embed, same);
    if ts.len() == 1 {
        return vec![(0.0, clip.to_vec())];
    }
    println!(
        "{} voices at once in {:.1}s",
        ts.len(),
        clip.len() as f64 / SR as f64
    );
    let mut out: Vec<(f64, Vec<f32>)> = vec![];
    for t in unleak(&ts) {
        for (o, u) in utterances(&t, 0.0, TRACK_MIN) {
            out.extend(turns(o, u, embed, same));
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0)); // stable, like Python's sorted
    out
}

/// Silence each track where the other one is much louder: there it only carries leak of the other voice, which
/// Whisper would transcribe a second time. Where both are loud, both people are talking.
fn unleak(ts: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let n = ts.iter().map(Vec::len).min().unwrap_or(0) / FRAME * FRAME;
    let r: Vec<Vec<f32>> = ts.iter().map(|t| frame_rms(&t[..n])).collect();
    ts.iter()
        .enumerate()
        .map(|(k, t)| {
            let mut t = t.clone();
            for (f, piece) in t[..n].as_chunks_mut::<FRAME>().0.iter_mut().enumerate() {
                if r[k][f] < LEAK * r[1 - k][f] {
                    piece.fill(0.0);
                }
            }
            t
        })
        .collect()
}

/// Split a separated piece where its voice changes. The separator can also swap tracks with no pause, e.g. when the
/// second person starts talking, leaving the first one's opening and the second one's words in one track.
fn turns(offset: f64, piece: &[f32], embed: &mut Embed, same: f32) -> Vec<(f64, Vec<f32>)> {
    let starts: Vec<usize> = windows(piece).collect();
    if starts.len() < 2 {
        return vec![(offset, piece.to_vec())];
    }
    let p: Vec<Vec<f32>> = starts
        .iter()
        .map(|&i| embed(&piece[i..i + WINDOW]))
        .collect();
    let side = |x: &[Vec<f32>]| {
        let mut m = vec![0f32; x[0].len()];
        for v in x {
            m.iter_mut()
                .zip(v)
                .for_each(|(a, b)| *a += b / x.len() as f32);
        }
        unit(m)
    };
    // ponytail: best single change point, then recurse on both sides; O(windows^2), no extra embeds
    let sims: Vec<f32> = (1..p.len())
        .map(|k| dot(&side(&p[..k]), &side(&p[k..])))
        .collect();
    let (k, best) = sims
        .iter()
        .enumerate()
        .fold((0, f32::INFINITY), |acc, (i, s)| {
            if *s < acc.1 { (i + 1, *s) } else { acc }
        });
    if best >= same {
        return vec![(offset, piece.to_vec())];
    }
    let cut = starts[k] + (WINDOW - HOP) / 2; // middle of the stretch the windows on both sides of the change share
    let mut out = turns(offset, &piece[..cut], embed, same);
    out.extend(turns(
        offset + cut as f64 / SR as f64,
        &piece[cut..],
        embed,
        same,
    ));
    out
}

#[cfg(test)]
#[path = "overlap_tests.rs"]
mod tests;
