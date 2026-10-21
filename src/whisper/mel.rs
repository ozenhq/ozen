//! The log-mel front end of Whisper: the STFT as one matmul with a cos/sin basis, then the Slaney mel filters.
use super::{HOP, N_FFT, SR};
use candle_core::{Device, Result, Tensor};

/// log-mel spectrogram [frames, 128], exactly as mlx_whisper.audio.log_mel_spectrogram.
pub(super) struct Mel {
    dft: Tensor,     // [N_FFT, 2 * bins]: cos then -sin
    filters: Tensor, // [bins, 128]
    window: Vec<f32>,
}

/// librosa.filters.mel(sr=16000, n_fft=400, n_mels) (Slaney scale and norm): what mel_filters.npz holds.
pub(super) fn mel_filters(n_mels: usize) -> Vec<f32> {
    let (f_sp, min_hz, logstep) = (200.0 / 3.0, 1000.0, 6.4f64.ln() / 27.0);
    let min_mel = min_hz / f_sp;
    let to_mel = |hz: f64| {
        if hz >= min_hz {
            min_mel + (hz / min_hz).ln() / logstep
        } else {
            hz / f_sp
        }
    };
    let to_hz = |m: f64| {
        if m >= min_mel {
            min_hz * (logstep * (m - min_mel)).exp()
        } else {
            f_sp * m
        }
    };
    let bins = N_FFT / 2 + 1;
    let top = to_mel(SR as f64 / 2.0);
    let mel_f: Vec<f64> = (0..n_mels + 2)
        .map(|i| to_hz(top * i as f64 / (n_mels + 1) as f64))
        .collect();
    let fft: Vec<f64> = (0..bins)
        .map(|k| (SR as f64 / 2.0) * k as f64 / (bins - 1) as f64)
        .collect();
    let mut w = vec![0f32; bins * n_mels]; // [bins, n_mels]
    for i in 0..n_mels {
        let norm = 2.0 / (mel_f[i + 2] - mel_f[i]);
        for (k, f) in fft.iter().enumerate() {
            let lower = (f - mel_f[i]) / (mel_f[i + 1] - mel_f[i]);
            let upper = (mel_f[i + 2] - f) / (mel_f[i + 2] - mel_f[i + 1]);
            w[k * n_mels + i] = (lower.min(upper).max(0.0) * norm) as f32;
        }
    }
    w
}

impl Mel {
    pub(super) fn load() -> Result<Self> {
        let bins = N_FFT / 2 + 1;
        let filters = Tensor::from_vec(mel_filters(128), (bins, 128), &Device::Cpu)?;
        let mut dft = vec![0f32; N_FFT * 2 * bins];
        for t in 0..N_FFT {
            for k in 0..bins {
                let a = 2.0 * std::f64::consts::PI * ((t * k) % N_FFT) as f64 / N_FFT as f64;
                dft[t * 2 * bins + k] = a.cos() as f32;
                dft[t * 2 * bins + bins + k] = -a.sin() as f32;
            }
        }
        let window = (0..N_FFT)
            .map(|i| {
                (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / N_FFT as f64).cos()) as f32
            }) // np.hanning(401)[:-1]
            .collect();
        let dft = Tensor::from_vec(dft, (N_FFT, 2 * bins), &Device::Cpu)?;
        Ok(Mel {
            dft,
            filters,
            window,
        })
    }

    /// `padding` zeros appended first, like log_mel_spectrogram(audio, padding=...).
    pub(super) fn compute(&self, audio: &[f32], padding: usize) -> Result<Tensor> {
        let mut x = audio.to_vec();
        x.resize(audio.len() + padding, 0.0);
        let p = N_FFT / 2; // reflect padding
        let mut padded: Vec<f32> = x[1..=p].iter().rev().copied().collect();
        padded.extend_from_slice(&x);
        padded.extend(x[x.len() - p - 1..x.len() - 1].iter().rev());
        let frames = (padded.len() - N_FFT + HOP) / HOP - 1; // the last frame is dropped
        let mut f = Vec::with_capacity(frames * N_FFT);
        for i in 0..frames {
            f.extend(
                padded[i * HOP..i * HOP + N_FFT]
                    .iter()
                    .zip(&self.window)
                    .map(|(a, b)| a * b),
            );
        }
        let bins = N_FFT / 2 + 1;
        let spec = Tensor::from_vec(f, (frames, N_FFT), &Device::Cpu)?.matmul(&self.dft)?;
        let power = (spec.narrow(1, 0, bins)?.sqr()? + spec.narrow(1, bins, bins)?.sqr()?)?;
        let log = (power
            .matmul(&self.filters)?
            .clamp(1e-10f32, f32::MAX)?
            .log()?
            / std::f64::consts::LN_10)?;
        let floor = log.max_all()?.to_scalar::<f32>()? - 8.0;
        (log.maximum(floor)? + 4.0)? / 4.0
    }
}
