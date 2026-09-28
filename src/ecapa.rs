//! Voiceprints: speechbrain's ECAPA-TDNN speaker encoder (speechbrain/spkrec-ecapa-voxceleb) in Rust.
//!
//! The voices registry stores these prints, so this reproduces speechbrain's `EncoderClassifier.encode_batch`
//! step for step, on the same weights: Fbank features (speechbrain.lobes.features.Fbank, 80 mels), sentence
//! mean normalization, then the network (speechbrain.lobes.models.ECAPA_TDNN), including its quirks (reflect
//! padding, a filterbank whose triangles are one band wide on each side).
//!
//! `ozen embed` serves it to the Python transcriber: after a READY line, each request on stdin is a little-endian u32 sample
//! count and that many f32 samples (16 kHz mono); each reply on stdout is the 192 f32 print (not unit length).
use candle_core::{D, DType, Device, Result, Tensor};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

const DIR: &str = "models/ecapa"; // shared with speechbrain's savedir
const WEIGHTS: &str = "embedding_model.ckpt";
const URL: &str = "https://huggingface.co/speechbrain/spkrec-ecapa-voxceleb/resolve/0f99f2d0ebe89ac095bcc5903c4dd8f72b367286/embedding_model.ckpt";
/// First line `ozen embed` writes, so a caller can tell it from a binary that predates it (which prints usage).
const READY: &[u8] = b"ozen embed 1\n";
const SR: f32 = 16000.0;
const N_FFT: usize = 400; // 25 ms
const HOP: usize = 160; // 10 ms
const MELS: usize = 80;
const BINS: usize = N_FFT / 2 + 1;

pub struct Ecapa {
    w: HashMap<String, Tensor>,
    window: Vec<f32>,
    dft: Tensor, // [N_FFT, 2 * BINS]: cos then -sin
    mel: Tensor, // [BINS, MELS]
}

/// torch.linspace in f32: the first half counts up from start, the second half down from end.
fn linspace(start: f32, end: f32, n: usize) -> Vec<f32> {
    let step = (end - start) / (n - 1) as f32;
    (0..n)
        .map(|i| {
            if i < n / 2 {
                start + step * i as f32
            } else {
                end - step * (n - 1 - i) as f32
            }
        })
        .collect()
}

/// speechbrain's Filterbank matrix: triangles centered on mel-spaced frequencies, as wide as the gap to the next.
fn filterbank() -> Vec<f32> {
    let to_mel = |hz: f64| (2595.0 * (1.0 + hz / 700.0).log10()) as f32;
    let hz: Vec<f32> = linspace(to_mel(0.0), to_mel(SR as f64 / 2.0), MELS + 2)
        .into_iter()
        .map(|m| 700.0 * (10f32.powf(m / 2595.0) - 1.0))
        .collect();
    let freqs = linspace(0.0, SR / 2.0, BINS);
    let mut m = vec![0f32; BINS * MELS];
    for j in 0..MELS {
        let (center, band) = (hz[j + 1], hz[j + 1] - hz[j]);
        for (i, f) in freqs.iter().enumerate() {
            let slope = (f - center) / band;
            m[i * MELS + j] = (slope + 1.0).min(-slope + 1.0).max(0.0);
        }
    }
    m
}

impl Ecapa {
    pub fn load() -> std::result::Result<Self, String> {
        let path = Path::new(DIR).join(WEIGHTS);
        if !path.exists() {
            std::fs::create_dir_all(DIR).map_err(|e| e.to_string())?;
            let ok = Command::new("curl")
                .args(["-sfL", "--retry", "3", "-o"])
                .arg(&path)
                .arg(URL)
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                let _ = std::fs::remove_file(&path);
                return Err(format!("download {URL} failed"));
            }
        }
        let w: HashMap<String, Tensor> = candle_core::pickle::read_all(&path)
            .map_err(|e| format!("{}: {e}", path.display()))?
            .into_iter()
            .collect();
        Self::with(w).map_err(|e| e.to_string())
    }

    /// The front end (window, DFT, mel filterbank) around these weights.
    fn with(w: HashMap<String, Tensor>) -> Result<Self> {
        let n = N_FFT as f64;
        let window = (0..N_FFT)
            .map(|i| (0.54 - 0.46 * (2.0 * std::f64::consts::PI * i as f64 / n).cos()) as f32) // periodic Hamming
            .collect();
        let mut dft = vec![0f32; N_FFT * 2 * BINS];
        for t in 0..N_FFT {
            for k in 0..BINS {
                let a = 2.0 * std::f64::consts::PI * ((t * k) % N_FFT) as f64 / n;
                dft[t * 2 * BINS + k] = a.cos() as f32;
                dft[t * 2 * BINS + BINS + k] = -a.sin() as f32;
            }
        }
        let cpu = &Device::Cpu;
        Ok(Ecapa {
            w,
            window,
            dft: Tensor::from_vec(dft, (N_FFT, 2 * BINS), cpu)?,
            mel: Tensor::from_vec(filterbank(), (BINS, MELS), cpu)?,
        })
    }

    /// Log-mel features [frames, MELS], mean-normalized per sentence.
    fn features(&self, wav: &[f32]) -> Result<Tensor> {
        // torch.stft(center=True, pad_mode="constant"): N_FFT/2 zeros on each side
        let mut padded = vec![0f32; N_FFT / 2];
        padded.extend_from_slice(wav);
        padded.resize(padded.len() + N_FFT / 2, 0.0);
        let frames = 1 + wav.len() / HOP;
        let mut x = Vec::with_capacity(frames * N_FFT);
        for f in 0..frames {
            let s = &padded[f * HOP..f * HOP + N_FFT];
            x.extend(s.iter().zip(&self.window).map(|(a, b)| a * b));
        }
        let spec = Tensor::from_vec(x, (frames, N_FFT), &Device::Cpu)?.matmul(&self.dft)?;
        let power = (spec.narrow(1, 0, BINS)?.sqr()? + spec.narrow(1, BINS, BINS)?.sqr()?)?;
        let db = (power.matmul(&self.mel)?.clamp(1e-10f32, f32::MAX)?.log()?
            * (10.0 / std::f64::consts::LN_10))?;
        let floor = db.max_all()?.to_scalar::<f32>()? - 80.0; // top_db
        let db = db.maximum(floor)?;
        db.broadcast_sub(&db.mean_keepdim(0)?)
    }

    fn get(&self, k: &str) -> Result<&Tensor> {
        self.w
            .get(k)
            .ok_or_else(|| candle_core::Error::Msg(format!("missing weight {k}")))
    }

    /// speechbrain Conv1d: "same" length through reflect padding.
    fn conv(&self, x: &Tensor, p: &str, dilation: usize) -> Result<Tensor> {
        let w = self.get(&format!("{p}.weight"))?;
        let pad = dilation * (w.dim(2)? - 1) / 2;
        let x = if pad > 0 {
            let t = x.dim(2)?;
            let idx: Vec<u32> = (0..t + 2 * pad)
                .map(|i| {
                    let j = i as i64 - pad as i64;
                    let j = if j < 0 {
                        -j
                    } else if j >= t as i64 {
                        2 * (t as i64 - 1) - j
                    } else {
                        j
                    };
                    j as u32
                })
                .collect();
            x.index_select(&Tensor::new(idx, x.device())?, 2)?
        } else {
            x.clone()
        };
        // As one matmul (Accelerate): [out, in*k] x [in*k, T], rows ordered like the weight's (channel, tap).
        let (out, inp, k) = w.dims3()?;
        let t = x.dim(2)? - dilation * (k - 1);
        let taps: Vec<Tensor> = (0..k)
            .map(|j| x.narrow(2, j * dilation, t))
            .collect::<Result<_>>()?;
        let cols = Tensor::stack(&taps, 2)?.reshape((inp * k, t))?;
        let y = w.reshape((out, inp * k))?.matmul(&cols)?.unsqueeze(0)?;
        y.broadcast_add(&self.get(&format!("{p}.bias"))?.reshape((1, (), 1))?)
    }

    /// BatchNorm1d in eval mode.
    fn norm(&self, x: &Tensor, p: &str) -> Result<Tensor> {
        let r = |k: &str| self.get(&format!("{p}.{k}"))?.reshape((1, (), 1));
        let scale = (r("weight")? / (r("running_var")? + 1e-5)?.sqrt()?)?;
        x.broadcast_sub(&r("running_mean")?)?
            .broadcast_mul(&scale)?
            .broadcast_add(&r("bias")?)
    }

    /// TDNNBlock: conv, ReLU, batch norm.
    fn tdnn(&self, x: &Tensor, p: &str, dilation: usize) -> Result<Tensor> {
        let y = self.conv(x, &format!("{p}.conv.conv"), dilation)?.relu()?;
        self.norm(&y, &format!("{p}.norm.norm"))
    }

    /// SERes2NetBlock: 1x1 TDNN, Res2Net (8 groups, each fed the previous), 1x1 TDNN, squeeze-excite, + input.
    fn se_res2net(&self, x: &Tensor, p: &str, dilation: usize) -> Result<Tensor> {
        let y = self.tdnn(x, &format!("{p}.tdnn1"), 1)?;
        let mut outs: Vec<Tensor> = vec![];
        for (i, c) in y.chunk(8, 1)?.into_iter().enumerate() {
            let b = format!("{p}.res2net_block.blocks.{}", i.max(1) - 1);
            outs.push(match i {
                0 => c,
                1 => self.tdnn(&c, &b, dilation)?,
                _ => self.tdnn(&(c + outs.last().unwrap())?, &b, dilation)?,
            });
        }
        let y = self.tdnn(&Tensor::cat(&outs, 1)?, &format!("{p}.tdnn2"), 1)?;
        let s = y.mean_keepdim(2)?;
        let s = self
            .conv(&s, &format!("{p}.se_block.conv1.conv"), 1)?
            .relu()?;
        let s = (self
            .conv(&s, &format!("{p}.se_block.conv2.conv"), 1)?
            .neg()?
            .exp()?
            + 1.0)?
            .recip()?; // sigmoid
        y.broadcast_mul(&s)? + x
    }

    /// The print for 16 kHz mono audio: 192 values, not unit length.
    pub fn embed(&self, wav: &[f32]) -> Result<Vec<f32>> {
        let x = self.features(wav)?.t()?.unsqueeze(0)?.contiguous()?; // [1, MELS, frames]
        let mut xs = vec![self.tdnn(&x, "blocks.0", 1)?];
        for (i, d) in [(1, 2), (2, 3), (3, 4)] {
            xs.push(self.se_res2net(&xs[i - 1], &format!("blocks.{i}"), d)?);
        }
        let x = self.tdnn(&Tensor::cat(&xs[1..], 1)?, "mfa", 1)?;
        // Attentive statistics pooling with global context.
        let stats = |w: &Tensor| -> Result<(Tensor, Tensor)> {
            let mean = (w * &x)?.sum_keepdim(2)?;
            let var = (w * x.broadcast_sub(&mean)?.sqr()?)?.sum_keepdim(2)?;
            Ok((mean, var.clamp(1e-12f32, f32::MAX)?.sqrt()?))
        };
        let t = x.dim(2)?;
        let (mean, std) = stats(&Tensor::full(1.0 / t as f32, x.shape(), x.device())?)?;
        let ctx = Tensor::cat(&[&x, &mean.repeat((1, 1, t))?, &std.repeat((1, 1, t))?], 1)?;
        let a = self.tdnn(&ctx, "asp.tdnn", 1)?.tanh()?;
        let a = self.conv(&a, "asp.conv.conv", 1)?;
        let a = a.broadcast_sub(&a.max_keepdim(D::Minus1)?)?.exp()?;
        let a = a.broadcast_div(&a.sum_keepdim(D::Minus1)?)?; // softmax over time
        let (mean, std) = stats(&a)?;
        let pooled = self.norm(&Tensor::cat(&[mean, std], 1)?, "asp_bn.norm")?;
        self.conv(&pooled, "fc.conv", 1)?
            .flatten_all()?
            .to_dtype(DType::F32)?
            .to_vec1()
    }
}

/// `ozen embed`: answer print requests on stdin until it closes.
pub fn serve() -> std::result::Result<(), String> {
    let enc = Ecapa::load()?;
    let (mut inp, mut out) = (std::io::stdin().lock(), std::io::stdout().lock());
    out.write_all(READY)
        .and_then(|_| out.flush())
        .map_err(|e| e.to_string())?;
    let mut n = [0u8; 4];
    while inp.read_exact(&mut n).is_ok() {
        let mut buf = vec![0u8; u32::from_le_bytes(n) as usize * 4];
        inp.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let wav: Vec<f32> = buf
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
            .collect();
        let e = enc.embed(&wav).map_err(|e| e.to_string())?;
        let bytes: Vec<u8> = e.iter().flat_map(|v| v.to_le_bytes()).collect();
        out.write_all(&bytes)
            .and_then(|_| out.flush())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_match_speechbrain() {
        // speechbrain 1.1.1: Fbank(n_mels=80) then InputNormalization("sentence", std_norm=False) on this signal.
        let x: Vec<f32> = (0..8000)
            .map(|i| {
                let t = i as f64 / 16000.0;
                let tau = 2.0 * std::f64::consts::PI;
                (0.3 * (tau * 440.0 * t).sin() + 0.1 * (tau * 3000.0 * t * t).sin()) as f32
            })
            .collect();
        let f = Ecapa::with(HashMap::new()).unwrap().features(&x).unwrap();
        assert_eq!(f.dims(), [51, 80]);
        let v = f.to_vec2::<f32>().unwrap();
        for (got, want) in [
            (v[0][0], 43.687332),
            (v[10][7], -6.6724625),
            (v[49][79], 9.953125),
        ] {
            assert!((got - want).abs() < 1e-3, "{got} vs {want}");
        }
        let sum: f32 = v.iter().flatten().map(|x| x.abs()).sum();
        assert!((sum / 44671.215 - 1.0).abs() < 1e-5, "{sum}");
        let mel: f32 = filterbank().iter().sum();
        assert!((mel - 193.0572).abs() < 1e-3, "{mel}");
    }
}
