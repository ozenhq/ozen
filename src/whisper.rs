//! Speech to text: Whisper large-v3-turbo (stock, and ivrit.ai's Hebrew fine-tune) in Rust, on the GPU.
//!
//! A port of what asr.py ran through mlx_whisper: the same MLX weights (mlx-community's fp16 safetensors), the
//! same log-mel front end, and `mlx_whisper.transcribe`'s decoding with the options asr.py used: greedy with
//! temperature fallback (0.0, 0.2 .. 1.0) on compression ratio > 2.4 or average log-prob < -1, timestamp segments,
//! the non-speech token suppression, an initial prompt, no conditioning on previous windows. mlx_whisper's own
//! quirks are kept where they change output (its "timestamps must not decrease" rule compares positions, not
//! timestamps, so it never fires).
use base64::Engine;
use candle_core::{DType, Device, IndexOp, Result, Tensor};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

const DIR: &str = "models/whisper";
const STOCK: &str = "mlx-community/whisper-large-v3-turbo"; // English + language detection
const HEBREW: &str = "mlx-community/ivrit-ai-whisper-large-v3-turbo-mlx"; // conversational Hebrew
// openai/whisper's assets, byte-identical to the copies mlx_whisper ships.
const ASSETS: &str = "https://raw.githubusercontent.com/openai/whisper/86098128c0b4f24f0e2aa2994de830614b474227/whisper/assets";
pub const SR: usize = 16000;
const N_FFT: usize = 400;
const HOP: usize = 160;
const N_SAMPLES: usize = 30 * SR; // one 30s window
const N_FRAMES: usize = N_SAMPLES / HOP; // 3000 mel frames
const LANGS: [(&str, u32); 2] = [("he", 50279), ("en", 50259)]; // the languages actually spoken, and their tokens

/// Fetch `url` to `path` once.
fn fetch(path: &Path, url: &str) -> std::result::Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let part = path.with_extension("part");
    let ok = Command::new("curl")
        .args(["-sfL", "--retry", "3", "-o"])
        .arg(&part)
        .arg(url)
        .status()
        .is_ok_and(|s| s.success());
    if !ok || std::fs::rename(&part, path).is_err() {
        let _ = std::fs::remove_file(&part);
        return Err(format!("download {url} failed"));
    }
    Ok(())
}

/// A model's local files: the copy mlx_whisper already downloaded, else ours under models/whisper.
fn model_dir(repo: &str) -> std::result::Result<PathBuf, String> {
    let home = std::env::var("HOME").unwrap_or_default();
    let hub = Path::new(&home).join(format!(
        ".cache/huggingface/hub/models--{}/snapshots",
        repo.replace('/', "--")
    ));
    for snap in std::fs::read_dir(hub).into_iter().flatten().flatten() {
        if snap.path().join("weights.safetensors").exists()
            && snap.path().join("config.json").exists()
        {
            return Ok(snap.path());
        }
    }
    let dir = Path::new(DIR).join(repo.replace('/', "--"));
    for f in ["config.json", "weights.safetensors"] {
        fetch(
            &dir.join(f),
            &format!("https://huggingface.co/{repo}/resolve/main/{f}"),
        )?;
    }
    Ok(dir)
}

struct Tokenizer {
    bpe: tiktoken_rs::CoreBPE,
    eot: u32,
    sot: u32,
    transcribe: u32,
    sot_prev: u32,
    no_speech: u32,
    no_timestamps: u32,
    ts_begin: u32,
    suppress: Vec<u32>, // "-1": non-speech symbols, plus the task tokens
}

impl Tokenizer {
    fn load() -> std::result::Result<Self, String> {
        let path = Path::new(DIR).join("multilingual.tiktoken");
        fetch(&path, &format!("{ASSETS}/multilingual.tiktoken"))?;
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let b64 = base64::engine::general_purpose::STANDARD;
        let mut ranks = HashMap::new();
        for line in text.lines().filter(|l| !l.is_empty()) {
            let (tok, rank) = line.split_once(' ').ok_or("bad tiktoken line")?;
            let bytes = if tok == "=" {
                vec![]
            } else {
                b64.decode(tok).map_err(|e| e.to_string())?
            }; // Python: b""
            ranks.insert(bytes, rank.parse().map_err(|_| "bad rank")?);
        }
        let n = ranks.len() as u32; // specials follow: eot, sot, 100 languages, then the task tokens
        let pattern = r"'s|'t|'re|'ve|'m|'ll|'d| ?\p{L}+| ?\p{N}+| ?[^\s\p{L}\p{N}]+|\s+(?!\S)|\s+";
        let bpe =
            tiktoken_rs::CoreBPE::new(ranks.into_iter().collect(), Default::default(), pattern)
                .map_err(|e| e.to_string())?;
        let (eot, sot) = (n, n + 1);
        let (translate, transcribe, sot_lm, sot_prev, no_speech, no_timestamps) =
            (n + 102, n + 103, n + 104, n + 105, n + 106, n + 107);
        // Tokenizer.non_speech_tokens: symbols that are single tokens (or the first token of ♩♪♫♬♭♮♯).
        let mut symbols: Vec<String> = "\"#()*+/:;<=>@[\\]^_`{|}~「」『』"
            .chars()
            .map(String::from)
            .collect();
        symbols.extend(
            "<< >> <<< >>> -- --- -( -[ (' (\" (( )) ((( ))) [[ ]] {{ }} ♪♪ ♪♪♪"
                .split(' ')
                .map(String::from),
        );
        let misc: Vec<String> = "♩♪♫♬♭♮♯".chars().map(String::from).collect();
        let mut suppress = vec![bpe.encode_ordinary(" -")[0], bpe.encode_ordinary(" '")[0]];
        for s in symbols.iter().chain(&misc) {
            for t in [
                bpe.encode_ordinary(s),
                bpe.encode_ordinary(&format!(" {s}")),
            ] {
                if t.len() == 1 || misc.contains(s) {
                    suppress.push(t[0]);
                }
            }
        }
        suppress.extend([transcribe, translate, sot, sot_prev, sot_lm, no_speech]);
        suppress.sort();
        suppress.dedup();
        Ok(Tokenizer {
            bpe,
            eot,
            sot,
            transcribe,
            sot_prev,
            no_speech,
            no_timestamps,
            ts_begin: n + 108,
            suppress,
        })
    }

    fn encode(&self, text: &str) -> Vec<u32> {
        self.bpe.encode_ordinary(text)
    }

    /// Text tokens only (timestamps and specials dropped), invalid UTF-8 replaced like Python's errors="replace".
    fn decode(&self, tokens: &[u32]) -> String {
        let text: Vec<u32> = tokens.iter().copied().filter(|&t| t < self.eot).collect();
        String::from_utf8_lossy(&self.bpe.decode_bytes(&text).unwrap_or_default()).into_owned()
    }
}

/// log-mel spectrogram [frames, 128], exactly as mlx_whisper.audio.log_mel_spectrogram.
struct Mel {
    dft: Tensor,     // [N_FFT, 2 * bins]: cos then -sin
    filters: Tensor, // [bins, 128]
    window: Vec<f32>,
}

/// librosa.filters.mel(sr=16000, n_fft=400, n_mels) (Slaney scale and norm): what mel_filters.npz holds.
fn mel_filters(n_mels: usize) -> Vec<f32> {
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
    fn load() -> Result<Self> {
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
    fn compute(&self, audio: &[f32], padding: usize) -> Result<Tensor> {
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

struct Model {
    w: HashMap<String, Tensor>,
    heads: usize,
    positions: Tensor, // encoder sinusoids [1500, 1280]
}

fn gelu(x: &Tensor) -> Result<Tensor> {
    x.gelu_erf()
}

impl Model {
    fn load(repo: &str, dev: &Device) -> std::result::Result<Self, String> {
        let dir = model_dir(repo)?;
        let cfg: Value = serde_json::from_slice(
            &std::fs::read(dir.join("config.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut w = candle_core::safetensors::load(dir.join("weights.safetensors"), dev)
            .map_err(|e| e.to_string())?;
        // Linear weights kept transposed, so x @ W^T is a plain matmul.
        let err = |e: candle_core::Error| e.to_string();
        let keys: Vec<String> = w.keys().cloned().collect();
        for k in keys {
            let t = &w[&k];
            if t.rank() == 2 && k.ends_with(".weight") && !k.contains("positional") {
                let tt = t.t().and_then(|x| x.contiguous()).map_err(err)?;
                if !k.contains("token_embedding") {
                    w.remove(&k);
                }
                w.insert(format!("{k}.t"), tt);
            }
        }
        let (ctx, state) = (
            cfg["n_audio_ctx"].as_u64().unwrap_or(1500) as usize,
            cfg["n_audio_state"].as_u64().unwrap_or(1280) as usize,
        );
        let half = state / 2;
        let inc = (10000f32).ln() / (half - 1) as f32;
        let mut pos = vec![0f32; ctx * state];
        for t in 0..ctx {
            for c in 0..half {
                let a = t as f32 * (-inc * c as f32).exp();
                pos[t * state + c] = a.sin();
                pos[t * state + half + c] = a.cos();
            }
        }
        let err = |e: candle_core::Error| e.to_string();
        let positions = Tensor::from_vec(pos, (ctx, state), dev)
            .map_err(err)?
            .to_dtype(DType::F16)
            .map_err(err)?;
        Ok(Model {
            w,
            heads: cfg["n_text_head"].as_u64().unwrap_or(20) as usize,
            positions,
        })
    }

    fn get(&self, k: &str) -> Result<&Tensor> {
        self.w
            .get(k)
            .ok_or_else(|| candle_core::Error::Msg(format!("missing weight {k}")))
    }

    fn linear(&self, x: &Tensor, p: &str) -> Result<Tensor> {
        let y = x.matmul(self.get(&format!("{p}.weight.t"))?)?;
        match self.w.get(&format!("{p}.bias")) {
            Some(b) => y.broadcast_add(b),
            None => Ok(y),
        }
    }

    /// LayerNorm (eps 1e-5), one fused kernel accumulating in f32.
    fn ln(&self, x: &Tensor, p: &str) -> Result<Tensor> {
        let (w, b) = (
            self.get(&format!("{p}.weight"))?,
            self.get(&format!("{p}.bias"))?,
        );
        candle_nn::ops::layer_norm(x, w, b, 1e-5)
    }

    /// MLX Conv1d (weight [out, k, in]) on [L, in] with padding 1, as one matmul.
    fn conv(&self, x: &Tensor, p: &str, stride: usize) -> Result<Tensor> {
        let w = self.get(&format!("{p}.weight"))?;
        let (out, k, inp) = w.dims3()?;
        let (l, c) = x.dims2()?;
        let x = Tensor::cat(
            &[
                &Tensor::zeros((1, c), x.dtype(), x.device())?,
                x,
                &Tensor::zeros((1, c), x.dtype(), x.device())?,
            ],
            0,
        )?;
        let lo = (l + 2 - k) / stride + 1;
        let taps: Vec<Tensor> = (0..k)
            .map(|j| {
                let idx: Vec<u32> = (0..lo).map(|t| (j + t * stride) as u32).collect();
                x.index_select(&Tensor::new(idx, x.device())?, 0)
            })
            .collect::<Result<_>>()?;
        let cols = Tensor::cat(&taps, 1)?; // [lo, k*in], taps major like the weight
        cols.matmul(&w.reshape((out, k * inp))?.t()?)?
            .broadcast_add(self.get(&format!("{p}.bias"))?)
    }

    /// Multi-head attention of q [Lq, S] over k, v [Lk, S]; `causal` adds the decoder's mask (from position 0).
    fn attend(&self, q: &Tensor, k: &Tensor, v: &Tensor, causal: bool) -> Result<Tensor> {
        let (lq, s) = q.dims2()?;
        let lk = k.dim(0)?;
        let hd = s / self.heads;
        if q.device().is_metal() {
            // Fused on the GPU: scale 1/sqrt(hd) once, where MLX scales q and k by hd^-0.25 each.
            let heads = |t: &Tensor, l: usize| {
                t.reshape((1, l, self.heads, hd))?
                    .transpose(1, 2)?
                    .contiguous()
            };
            let out = candle_nn::ops::sdpa(
                &heads(q, lq)?,
                &heads(k, lk)?,
                &heads(v, lk)?,
                None,
                causal && lq > 1,
                1.0 / (hd as f32).sqrt(),
                1.0,
            )?;
            return out.transpose(1, 2)?.reshape((lq, s));
        }
        let scale = (hd as f64).powf(-0.25);
        let q = (q.reshape((lq, self.heads, hd))?.transpose(0, 1)? * scale)?.contiguous()?;
        let k = (k
            .reshape((lk, self.heads, hd))?
            .transpose(0, 1)?
            .transpose(1, 2)?
            * scale)?
            .contiguous()?;
        let v = v
            .reshape((lk, self.heads, hd))?
            .transpose(0, 1)?
            .contiguous()?;
        let mut qk = q.matmul(&k)?;
        if causal && lq > 1 {
            let m: Vec<f32> = (0..lq)
                .flat_map(|i| (0..lk).map(move |j| if j > i { f32::NEG_INFINITY } else { 0.0 }))
                .collect();
            qk = qk.broadcast_add(
                &Tensor::from_vec(m, (lq, lk), qk.device())?.to_dtype(qk.dtype())?,
            )?;
        }
        let w = candle_nn::ops::softmax_last_dim(&qk)?; // fused; accumulates in f32 like MLX's precise softmax
        w.matmul(&v)?.transpose(0, 1)?.reshape((lq, s))
    }

    /// Audio features [1500, 1280] for a [3000, 128] mel window.
    fn encode(&self, mel: &Tensor) -> Result<Tensor> {
        let mut x = gelu(&self.conv(&mel.to_dtype(DType::F16)?, "encoder.conv1", 1)?)?;
        x = gelu(&self.conv(&x, "encoder.conv2", 2)?)?;
        x = (x + &self.positions)?;
        for i in 0.. {
            let p = format!("encoder.blocks.{i}");
            if !self.w.contains_key(&format!("{p}.attn_ln.weight")) {
                break;
            }
            let h = self.ln(&x, &format!("{p}.attn_ln"))?;
            let (q, k, v) = (
                self.linear(&h, &format!("{p}.attn.query"))?,
                self.linear(&h, &format!("{p}.attn.key"))?,
                self.linear(&h, &format!("{p}.attn.value"))?,
            );
            x = (x + self.linear(&self.attend(&q, &k, &v, false)?, &format!("{p}.attn.out"))?)?;
            let h = self.ln(&x, &format!("{p}.mlp_ln"))?;
            x = (&x
                + self.linear(
                    &gelu(&self.linear(&h, &format!("{p}.mlp1"))?)?,
                    &format!("{p}.mlp2"),
                )?)?;
        }
        self.ln(&x, "encoder.ln_post")
    }

    fn decoder_layers(&self) -> usize {
        (0..)
            .take_while(|i| {
                self.w
                    .contains_key(&format!("decoder.blocks.{i}.attn_ln.weight"))
            })
            .count()
    }
}

/// One window's decoder state: cross-attention keys/values and the growing self-attention cache.
struct Session<'a> {
    m: &'a Model,
    cross: Vec<(Tensor, Tensor)>,
    cache: Vec<Option<(Tensor, Tensor)>>,
}

impl<'a> Session<'a> {
    fn new(m: &'a Model, audio: &Tensor) -> Result<Self> {
        let n = m.decoder_layers();
        let cross = (0..n)
            .map(|i| {
                let p = format!("decoder.blocks.{i}.cross_attn");
                Ok((
                    m.linear(audio, &format!("{p}.key"))?,
                    m.linear(audio, &format!("{p}.value"))?,
                ))
            })
            .collect::<Result<_>>()?;
        Ok(Session {
            m,
            cross,
            cache: vec![None; n],
        })
    }

    /// Logits [len(tokens), vocab] (f32) for these next tokens.
    fn logits(&mut self, tokens: &[u32]) -> Result<Tensor> {
        let m = self.m;
        let offset = self.cache[0]
            .as_ref()
            .map_or(0, |(k, _)| k.dim(0).unwrap_or(0));
        let dev = m.positions.device();
        let emb = m.get("decoder.token_embedding.weight")?;
        let ids = Tensor::new(tokens, dev)?;
        let mut x = (emb.index_select(&ids, 0)?
            + m.get("decoder.positional_embedding")?
                .narrow(0, offset, tokens.len())?)?;
        for i in 0..self.cache.len() {
            let p = format!("decoder.blocks.{i}");
            let h = m.ln(&x, &format!("{p}.attn_ln"))?;
            let q = m.linear(&h, &format!("{p}.attn.query"))?;
            let (mut k, mut v) = (
                m.linear(&h, &format!("{p}.attn.key"))?,
                m.linear(&h, &format!("{p}.attn.value"))?,
            );
            if let Some((pk, pv)) = &self.cache[i] {
                k = Tensor::cat(&[pk, &k], 0)?;
                v = Tensor::cat(&[pv, &v], 0)?;
            }
            x = (x + m.linear(&m.attend(&q, &k, &v, true)?, &format!("{p}.attn.out"))?)?;
            self.cache[i] = Some((k, v));
            let h = m.ln(&x, &format!("{p}.cross_attn_ln"))?;
            let q = m.linear(&h, &format!("{p}.cross_attn.query"))?;
            let (ck, cv) = &self.cross[i];
            x = (x + m.linear(
                &m.attend(&q, ck, cv, false)?,
                &format!("{p}.cross_attn.out"),
            )?)?;
            let h = m.ln(&x, &format!("{p}.mlp_ln"))?;
            x = (&x
                + m.linear(
                    &gelu(&m.linear(&h, &format!("{p}.mlp1"))?)?,
                    &format!("{p}.mlp2"),
                )?)?;
        }
        let x = m.ln(&x, "decoder.ln")?;
        x.matmul(m.get("decoder.token_embedding.weight.t")?)?
            .to_dtype(DType::F32)
    }
}

/// Deterministic sampler for the temperature fallback (splitmix64), reseeded per request.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn log_softmax(x: &[f32]) -> Vec<f32> {
    let m = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let lse = m + x.iter().map(|v| (v - m).exp()).sum::<f32>().ln();
    x.iter().map(|v| v - lse).collect()
}

/// A segment's text with its window's no_speech_prob, avg_logprob and compression_ratio.
type Segment = (String, f64, f64, f64);

struct Decoded {
    tokens: Vec<u32>,
    temperature: f64,
    avg_logprob: f64,
    compression_ratio: f64,
    no_speech_prob: f64,
}

fn compression_ratio(text: &str) -> f64 {
    let mut z = flate2::write::ZlibEncoder::new(vec![], flate2::Compression::default());
    z.write_all(text.as_bytes()).unwrap();
    text.len() as f64 / z.finish().unwrap().len() as f64
}

pub struct Whisper {
    tok: Tokenizer,
    mel: Mel,
    stock: Model,
    hebrew: Model,
}

impl Whisper {
    pub fn load() -> std::result::Result<Self, String> {
        let dev = Device::new_metal(0).unwrap_or(Device::Cpu);
        Ok(Whisper {
            tok: Tokenizer::load()?,
            mel: Mel::load().map_err(|e| e.to_string())?,
            stock: Model::load(STOCK, &dev)?,
            hebrew: Model::load(HEBREW, &dev)?,
        })
    }

    /// DecodingTask.run for one window at one temperature.
    fn decode_window(
        &self,
        m: &Model,
        audio: &Tensor,
        lang: u32,
        prompt: &[u32],
        t: f64,
        rng: &mut Rng,
    ) -> Result<Decoded> {
        let tok = &self.tok;
        let mut initial = vec![];
        if !prompt.is_empty() {
            initial.push(tok.sot_prev);
            initial.extend_from_slice(&prompt[prompt.len().saturating_sub(223)..]);
        }
        let sot_index = initial.len();
        initial.extend([tok.sot, lang, tok.transcribe]);
        let begin = initial.len();
        let mut s = Session::new(m, audio)?;
        let mut tokens = initial.clone();
        let (mut sum_logprob, mut no_speech_prob) = (0f64, 0f64);
        let ts = tok.ts_begin as usize;
        for step in 0..224 {
            if tokens.len() > 448 {
                break;
            }
            let logits = if step == 0 {
                let all = s.logits(&initial)?;
                let at_sot: Vec<f32> = all.i(sot_index)?.to_vec1()?;
                no_speech_prob = log_softmax(&at_sot)[tok.no_speech as usize].exp() as f64;
                all.i(all.dim(0)? - 1)?.to_vec1::<f32>()?
            } else {
                s.logits(&tokens[tokens.len() - 1..])?
                    .i(0)?
                    .to_vec1::<f32>()?
            };
            let mut l = logits;
            // SuppressBlank, SuppressTokens
            if tokens.len() == begin {
                for t in tok.encode(" ").into_iter().chain([tok.eot]) {
                    l[t as usize] = f32::NEG_INFINITY;
                }
            }
            for &t in &tok.suppress {
                l[t as usize] = f32::NEG_INFINITY;
            }
            // ApplyTimestampRules
            let mut mask = vec![0f32; l.len()];
            mask[tok.no_timestamps as usize] = f32::NEG_INFINITY;
            let seq = &tokens[begin..];
            let last_ts = seq.last().is_some_and(|&x| x >= tok.ts_begin);
            let penult_ts = seq.len() < 2 || seq[seq.len() - 2] >= tok.ts_begin;
            if last_ts {
                let r = if penult_ts {
                    ts..l.len()
                } else {
                    0..tok.eot as usize
                };
                mask[r].fill(f32::NEG_INFINITY);
            }
            // mlx_whisper masks [ts_begin, last timestamp's *position*): always empty, so nothing to do here.
            if tokens.len() == begin {
                mask[..ts].fill(f32::NEG_INFINITY);
                mask[ts + 50 + 1..].fill(f32::NEG_INFINITY); // max_initial_timestamp 1.0s
            }
            let lp = log_softmax(&l);
            let m = lp[ts..].iter().copied().fold(f32::NEG_INFINITY, f32::max);
            let ts_lp = m + lp[ts..].iter().map(|v| (v - m).exp()).sum::<f32>().ln();
            if ts_lp > lp[..ts].iter().copied().fold(f32::NEG_INFINITY, f32::max) {
                mask[..ts].fill(f32::NEG_INFINITY);
            }
            for (a, b) in l.iter_mut().zip(&mask) {
                *a += b;
            }
            // GreedyDecoder
            let next = if t == 0.0 {
                l.iter()
                    .enumerate()
                    .fold(
                        (0, f32::NEG_INFINITY),
                        |b, (i, &v)| if v > b.1 { (i, v) } else { b },
                    )
                    .0
            } else {
                let scaled: Vec<f32> = l.iter().map(|v| v / t as f32).collect();
                let p: Vec<f64> = log_softmax(&scaled)
                    .iter()
                    .map(|v| (*v as f64).exp())
                    .collect();
                let (mut r, mut pick) = (rng.next(), p.len() - 1);
                for (i, pi) in p.iter().enumerate() {
                    if r < *pi {
                        pick = i;
                        break;
                    }
                    r -= pi;
                }
                pick
            };
            sum_logprob += log_softmax(&l)[next] as f64;
            tokens.push(next as u32);
            if next as u32 == tok.eot {
                break;
            }
        }
        let mut out: Vec<u32> = tokens[begin..].to_vec();
        if let Some(e) = out.iter().position(|&x| x == tok.eot) {
            out.truncate(e);
        } // out of steps: finalize() would append EOT and cut there, which changes nothing
        let text = tok.decode(&out).trim().to_string();
        Ok(Decoded {
            avg_logprob: sum_logprob / (out.len() + 1) as f64,
            compression_ratio: compression_ratio(&text),
            no_speech_prob,
            temperature: t,
            tokens: out,
        })
    }

    /// mlx_whisper.transcribe(clip, language, initial_prompt, condition_on_previous_text=False): its segments as
    /// (text, no_speech_prob, avg_logprob, compression_ratio), and the whole text.
    fn transcribe(
        &self,
        m: &Model,
        clip: &[f32],
        lang: u32,
        prompt: &str,
    ) -> Result<(Vec<Segment>, String)> {
        let tok = &self.tok;
        let mel = self.mel.compute(clip, N_SAMPLES)?;
        let content = mel.dim(0)? - N_FRAMES;
        let initial = tok.encode(&format!(" {}", prompt.trim())); // asr.py always passes one, even if empty
        let (mut all, mut segments, mut reset) = (initial.clone(), vec![], 0);
        let mut seek = 0;
        let mut rng = Rng(0);
        while seek < content {
            let size = N_FRAMES.min(content - seek);
            let seg = mel.narrow(0, seek, size)?;
            let seg = Tensor::cat(
                &[
                    &seg,
                    &Tensor::zeros((N_FRAMES - size, seg.dim(1)?), DType::F32, &Device::Cpu)?,
                ],
                0,
            )?;
            let audio = m.encode(&seg.to_device(m.positions.device())?)?;
            let prompt_tokens = all[reset..].to_vec();
            let mut r = None;
            for t in [0.0, 0.2, 0.4, 0.6, 0.8, 1.0] {
                let d = self.decode_window(m, &audio, lang, &prompt_tokens, t, &mut rng)?;
                let fallback =
                    (d.compression_ratio > 2.4 || d.avg_logprob < -1.0) && d.no_speech_prob <= 0.6;
                r = Some(d);
                if !fallback {
                    break;
                }
            }
            let r = r.unwrap();
            if r.no_speech_prob > 0.6 && r.avg_logprob <= -1.0 {
                seek += size; // silence
                continue;
            }
            let toks = &r.tokens;
            let is_ts: Vec<bool> = toks.iter().map(|&x| x >= tok.ts_begin).collect();
            let single_end = is_ts.len() >= 2 && !is_ts[is_ts.len() - 2] && is_ts[is_ts.len() - 1];
            let mut cuts: Vec<usize> = (1..is_ts.len())
                .filter(|&i| is_ts[i - 1] && is_ts[i])
                .collect();
            let mut current: Vec<(i64, i64, Vec<u32>)> = vec![]; // (start pos, end pos, tokens)
            if !cuts.is_empty() {
                if single_end {
                    cuts.push(toks.len());
                }
                let mut last = 0;
                for &c in &cuts {
                    let sl = &toks[last..c];
                    let pos = |x: u32| x as i64 - tok.ts_begin as i64;
                    current.push((pos(sl[0]), pos(*sl.last().unwrap()), sl.to_vec()));
                    last = c;
                }
                seek += if single_end {
                    size
                } else {
                    (toks[last - 1] - tok.ts_begin) as usize * 2
                };
            } else {
                let dur = toks
                    .iter()
                    .rfind(|&&x| x >= tok.ts_begin)
                    .filter(|&&x| x != tok.ts_begin)
                    .map_or(size as i64, |&x| (x - tok.ts_begin) as i64 * 2);
                current.push((0, dur, toks.clone()));
                seek += size;
            }
            for (start, end, mut t) in current {
                let mut text = tok.decode(&t);
                if start == end || text.trim().is_empty() {
                    (text, t) = (String::new(), vec![]);
                }
                all.extend_from_slice(&t);
                segments.push((text, r.no_speech_prob, r.avg_logprob, r.compression_ratio));
            }
            reset = all.len(); // condition_on_previous_text=False
            let _ = r.temperature;
        }
        Ok((
            segments,
            tok.decode(&all[initial.len()..]).trim().to_string(),
        ))
    }

    /// asr.py's decode: (text Whisper is confident in, language, raw text before any filter). `model` pins
    /// "stock" or "hebrew" in `lang`; otherwise clips of 1.5s+ pick between he and en with the stock model, and
    /// the language picks the model.
    pub fn decode(
        &self,
        clip: &[f32],
        prompt: &str,
        lang: &str,
        model: Option<&str>,
    ) -> Result<(String, String, String)> {
        let mut lang = lang.to_string();
        if model.is_none() && clip.len() as f64 >= 1.5 * SR as f64 {
            let mut x = clip[..clip.len().min(N_SAMPLES)].to_vec();
            x.resize(N_SAMPLES, 0.0); // pad_or_trim
            let mel = self.mel.compute(&x, 0)?;
            let audio = self
                .stock
                .encode(&mel.to_device(self.stock.positions.device())?)?;
            let logits: Vec<f32> = Session::new(&self.stock, &audio)?
                .logits(&[self.tok.sot])?
                .i(0)?
                .to_vec1()?;
            lang = LANGS
                .iter()
                .max_by(|a, b| logits[a.1 as usize].total_cmp(&logits[b.1 as usize]))
                .unwrap()
                .0
                .to_string();
        }
        let m = match model {
            Some("stock") => &self.stock,
            Some("hebrew") => &self.hebrew,
            _ if lang == "he" => &self.hebrew,
            _ => &self.stock,
        };
        let token = LANGS
            .iter()
            .find(|l| l.0 == lang)
            .map_or(LANGS[0].1, |l| l.1);
        let (segments, raw) = self.transcribe(m, clip, token, prompt)?;
        // Drop segments Whisper itself flags as noise; these are the hallucinated lines.
        let text = segments
            .iter()
            .filter(|s| s.1 < 0.5 && s.2 > -0.8 && s.3 < 2.4)
            .map(|s| s.0.trim())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string();
        Ok((text, lang, raw))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mel_filters_match_whispers_npz() {
        // mel_filters.npz ["mel_128"] (openai/whisper and mlx_whisper assets), indexed [mel][bin]
        let m = mel_filters(128);
        let at = |mel: usize, bin: usize| m[bin * 128 + mel];
        let sum: f32 = m.iter().sum();
        assert!((sum - 3.1909854).abs() < 1e-5, "{sum}");
        let max = m.iter().copied().fold(0.0, f32::max);
        assert!((max - 0.041_681_75).abs() < 1e-7, "{max}");
        assert_eq!(at(10, 5), 0.0);
        assert_eq!(at(127, 200), 0.0);
        assert_eq!(at(0, 0), 0.0);
        for (got, want) in [
            (at(0, 1), 0.012373987),
            (at(2, 2), 0.024747973),
            (at(127, 199), 0.0011142802),
        ] {
            assert!((got - want).abs() < 1e-9, "{got} vs {want}");
        }
    }
}
