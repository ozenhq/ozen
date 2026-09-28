//! Voice separation: MossFormer2 (ClearVoice's MossFormer2_SS_16K, 2 voices) in Rust, on ClearVoice's weights.
//!
//! Reproduces `CLS_MossFormer2_SS_16K.decode_data`: the clip is decoded in 2s windows (1.5s stride, 0.25s given up
//! at each inner edge), and each track is scaled to the clip's RMS. overlap.py explains why windows, not whole clips.
//! Sequence tensors are [time, channels] (batch 1), so 1x1 convolutions are matmuls.
//!
//! `ozen separate` serves it to the Python transcriber: after a READY line, each request on stdin is a little-endian u32
//! sample count and that many f32 samples (16 kHz mono); each reply is two tracks of that many f32 samples.
use candle_core::{Device, Result, Tensor};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::Path;
use std::process::Command;

const DIR: &str = "models/checkpoints/MossFormer2_SS_16K"; // where ClearVoice kept it, so no second download
const WEIGHTS: &str = "last_best_checkpoint.pt";
const URL: &str = "https://huggingface.co/alibabasglab/MossFormer2_SS_16K/resolve/407cb030cd66340918ebb6c8cc63b18f8592cdbe/last_best_checkpoint.pt";
const READY: &[u8] = b"ozen separate 1\n";
const SR: usize = 16000;
const WINDOW: usize = 2 * SR; // decode_window: 2
const STRIDE: usize = WINDOW * 3 / 4;
const LAYERS: usize = 24;
const GROUP: usize = 256; // attention group size
const KERNEL: usize = 16; // encoder/decoder, stride KERNEL / 2
const BLOCKS: &str = "mask_net.mdl.intra_mdl.mossformerM";

pub struct Separator {
    w: HashMap<String, Tensor>,
    dev: Device,
}

use candle_nn::ops::{sigmoid, silu};

/// Normalizes each row over its channels (nn.LayerNorm on the last dim).
fn layer_norm(x: &Tensor, w: &Tensor, b: &Tensor, eps: f64) -> Result<Tensor> {
    candle_nn::ops::layer_norm(&x.contiguous()?, w, b, eps as f32)
}

/// nn.GroupNorm(1, C): one mean and variance over the whole [T, C], then per-channel affine.
fn group_norm(x: &Tensor, w: &Tensor, b: &Tensor) -> Result<Tensor> {
    let x = x.broadcast_sub(&x.mean_all()?)?;
    let var = x.sqr()?.mean_all()?;
    x.broadcast_div(&(var + 1e-8)?.sqrt()?)?
        .broadcast_mul(w)?
        .broadcast_add(b)
}

/// nn.PReLU with one or per-channel slopes.
fn prelu(x: &Tensor, a: &Tensor) -> Result<Tensor> {
    let neg = x.minimum(0f32)?;
    x.relu()? + neg.broadcast_mul(a)?
}

/// Shifts [T, C] down one row, zero-filled: x[t - 1].
fn shift(x: &Tensor) -> Result<Tensor> {
    let (t, c) = x.dims2()?;
    Tensor::cat(
        &[
            &Tensor::zeros((1, c), x.dtype(), x.device())?,
            &x.narrow(0, 0, t - 1)?,
        ],
        0,
    )
}

/// Grouped conv along time on [T, C * m] with `m` input channels per group (channels 2c, 2c+1 for m = 2),
/// C outputs, 'same' length: w is [C, m, K]. A custom op: candle would split a grouped conv into one conv per
/// group (2048 of them), and composing it from shifted multiply-adds was 10x slower on Metal.
fn grouped_conv(x: &Tensor, w: &Tensor, dilation: usize, pad: usize) -> Result<Tensor> {
    x.contiguous()?
        .apply_op2_no_bwd(&w.contiguous()?, &GroupedConv { dilation, pad })
}

struct GroupedConv {
    dilation: usize,
    pad: usize,
}

const GROUPED_CONV_METAL: &str = r"
#include <metal_stdlib>
struct P { uint t; uint c; uint m; uint k; uint dil; uint pad; };
kernel void grouped_conv(device const float* x [[buffer(0)]], device const float* w [[buffer(1)]],
                         device float* out [[buffer(2)]], constant P& p [[buffer(3)]],
                         uint2 gid [[thread_position_in_grid]]) {
    uint c = gid.x, t = gid.y;
    if (c >= p.c || t >= p.t) return;
    float acc = 0.0;
    for (uint i = 0; i < p.k; i++) {
        int s = int(t + i * p.dil) - int(p.pad);
        if (s < 0 || s >= int(p.t)) continue;
        for (uint j = 0; j < p.m; j++) acc += x[(uint(s) * p.c + c) * p.m + j] * w[(c * p.m + j) * p.k + i];
    }
    out[t * p.c + c] = acc;
}";

impl GroupedConv {
    /// (T, C, m, K) from x [T, C*m] and w [C, m, K].
    fn dims(
        &self,
        l1: &candle_core::Layout,
        l2: &candle_core::Layout,
    ) -> Result<(usize, usize, usize, usize)> {
        let (t, _) = l1.shape().dims2()?;
        let (c, m, k) = l2.shape().dims3()?;
        Ok((t, c, m, k))
    }
}

impl candle_core::CustomOp2 for GroupedConv {
    fn name(&self) -> &'static str {
        "grouped_conv"
    }

    fn cpu_fwd(
        &self,
        s1: &candle_core::CpuStorage,
        l1: &candle_core::Layout,
        s2: &candle_core::CpuStorage,
        l2: &candle_core::Layout,
    ) -> Result<(candle_core::CpuStorage, candle_core::Shape)> {
        let (t, c, m, k) = self.dims(l1, l2)?;
        let xs = &s1.as_slice::<f32>()?[l1.start_offset()..];
        let ws = &s2.as_slice::<f32>()?[l2.start_offset()..];
        let mut out = vec![0f32; t * c];
        for (ti, row) in out.chunks_exact_mut(c).enumerate() {
            for i in 0..k {
                let Some(src) = (ti + i * self.dilation)
                    .checked_sub(self.pad)
                    .filter(|s| *s < t)
                else {
                    continue;
                };
                let xr = &xs[src * c * m..(src + 1) * c * m];
                for (ch, o) in row.iter_mut().enumerate() {
                    for j in 0..m {
                        *o += xr[ch * m + j] * ws[(ch * m + j) * k + i];
                    }
                }
            }
        }
        Ok((candle_core::CpuStorage::F32(out), (t, c).into()))
    }

    fn metal_fwd(
        &self,
        s1: &candle_core::MetalStorage,
        l1: &candle_core::Layout,
        s2: &candle_core::MetalStorage,
        l2: &candle_core::Layout,
    ) -> Result<(candle_core::MetalStorage, candle_core::Shape)> {
        use candle_core::backend::BackendStorage;
        use std::sync::OnceLock;
        static PIPELINE: OnceLock<
            std::result::Result<candle_metal_kernels::metal::ComputePipeline, String>,
        > = OnceLock::new();
        let (t, c, m, k) = self.dims(l1, l2)?;
        let dev = s1.device();
        let pipeline = PIPELINE
            .get_or_init(|| {
                let md = dev.metal_device();
                let lib = md
                    .new_library_with_source(GROUPED_CONV_METAL, None)
                    .map_err(|e| e.to_string())?;
                let f = lib
                    .get_function("grouped_conv", None)
                    .map_err(|e| e.to_string())?;
                md.new_compute_pipeline_state_with_function(&f)
                    .map_err(|e| e.to_string())
            })
            .as_ref()
            .map_err(|e| candle_core::Error::Msg(e.clone()))?;
        let out = dev
            .new_buffer_builder()
            .with_size_for(t * c, candle_core::DType::F32)
            .with_label("grouped_conv")
            .build()?;
        let guard = dev.command_encoder()?;
        let enc: &candle_metal_kernels::metal::ComputeCommandEncoder = guard.as_ref();
        enc.set_label("grouped_conv");
        enc.set_compute_pipeline_state(pipeline);
        enc.set_input_buffer(0, Some(s1.buffer()), l1.start_offset() * 4);
        enc.set_input_buffer(1, Some(s2.buffer()), l2.start_offset() * 4);
        enc.set_output_buffer(2, Some(&out), 0);
        let p: [u32; 6] = [t, c, m, k, self.dilation, self.pad].map(|v| v as u32);
        enc.set_bytes(3, &p);
        let size = |w, h| objc2_metal::MTLSize {
            width: w,
            height: h,
            depth: 1,
        };
        enc.dispatch_threads(size(c, t), size(64, 4));
        Ok((
            candle_core::MetalStorage::new(out, dev.clone(), t * c, candle_core::DType::F32),
            (t, c).into(),
        ))
    }
}

impl Separator {
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
        let dev = Device::new_metal(0).unwrap_or(Device::Cpu);
        let w = candle_core::pickle::read_all_with_key(&path, Some("model"))
            .map_err(|e| format!("{}: {e}", path.display()))?
            .into_iter()
            .map(|(k, v)| {
                // Linear and 1x1 conv weights [out, in(, 1)] are used as [in, out] right-hand matmul operands.
                let v = match v.dims() {
                    [_, _] if k.ends_with(".weight") => v.t()?.contiguous()?,
                    [_, _, 1] if !k.ends_with("conv.weight") => v.squeeze(2)?.t()?.contiguous()?,
                    _ => v,
                };
                Ok((k, v.to_device(&dev)?))
            })
            .collect::<Result<_>>()
            .map_err(|e| e.to_string())?;
        Ok(Separator { w, dev })
    }

    fn get(&self, k: &str) -> Result<&Tensor> {
        self.w
            .get(k)
            .ok_or_else(|| candle_core::Error::Msg(format!("missing weight {k}")))
    }

    fn linear(&self, x: &Tensor, p: &str) -> Result<Tensor> {
        let y = x.contiguous()?.matmul(self.get(&format!("{p}.weight"))?)?;
        match self.w.get(&format!("{p}.bias")) {
            Some(b) => y.broadcast_add(b),
            None => Ok(y),
        }
    }

    /// FFConvM: norm (ScaleNorm or LayerNorm), linear, SiLU, then a residual depthwise conv over time (kernel 17).
    fn ffconvm(&self, x: &Tensor, p: &str) -> Result<Tensor> {
        let x = match self.w.get(&format!("{p}.mdl.0.g")) {
            Some(g) => {
                let d = x.dim(1)? as f64;
                let norm = (x.sqr()?.sum_keepdim(1)?.sqrt()? * d.powf(-0.5))?.maximum(1e-5f32)?;
                x.broadcast_div(&norm)?.broadcast_mul(g)?
            }
            None => layer_norm(
                x,
                self.get(&format!("{p}.mdl.0.weight"))?,
                self.get(&format!("{p}.mdl.0.bias"))?,
                1e-5,
            )?,
        };
        let h = silu(&self.linear(&x, &format!("{p}.mdl.1"))?)?;
        let w = self.get(&format!("{p}.mdl.3.sequential.1.conv.weight"))?;
        &h + grouped_conv(&h, w, 1, 8)?
    }

    /// Rotary position embedding on the first 32 channels (rotary_embedding_torch, interleaved pairs).
    fn rotary(&self, x: &Tensor, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
        let (t, d) = x.dims2()?;
        let r = x.narrow(1, 0, 32)?.reshape((t, 16, 2))?;
        let (a, b) = (
            r.narrow(2, 0, 1)?.squeeze(2)?,
            r.narrow(2, 1, 1)?.squeeze(2)?,
        );
        let ra = ((&a * cos)? - (&b * sin)?)?;
        let rb = ((&b * cos)? + (&a * sin)?)?;
        let rot = Tensor::stack(&[ra, rb], 2)?.reshape((t, 32))?;
        Tensor::cat(&[&rot, &x.narrow(1, 32, d - 32)?], 1)
    }

    /// FLASH_ShareA_FFConvM: gated attention, quadratic within groups of 256 frames plus linear across them.
    fn flash(&self, x: &Tensor, p: &str, cos: &Tensor, sin: &Tensor) -> Result<Tensor> {
        let (n, d) = x.dims2()?;
        let xs = Tensor::cat(
            &[
                &shift(&x.narrow(1, 0, d / 2)?)?,
                &x.narrow(1, d / 2, d / 2)?,
            ],
            1,
        )?;
        let hidden = self.ffconvm(&xs, &format!("{p}.to_hidden"))?;
        let e = hidden.dim(1)? / 2;
        let (v, u) = (
            hidden.narrow(1, 0, e)?.contiguous()?,
            hidden.narrow(1, e, e)?.contiguous()?,
        );
        let qk = self.ffconvm(&xs, &format!("{p}.to_qk"))?;
        let (gamma, beta) = (
            self.get(&format!("{p}.qk_offset_scale.gamma"))?,
            self.get(&format!("{p}.qk_offset_scale.beta"))?,
        );
        let head = |h: usize| -> Result<Tensor> {
            let y = qk
                .broadcast_mul(&gamma.narrow(0, h, 1)?)?
                .broadcast_add(&beta.narrow(0, h, 1)?)?;
            self.rotary(&y, cos, sin)
        };
        let (quad_q, lin_q, quad_k, lin_k) = (head(0)?, head(1)?, head(2)?, head(3)?);

        let pad = (GROUP - n % GROUP) % GROUP;
        let groups = (n + pad) / GROUP;
        let grouped = |t: &Tensor| -> Result<Tensor> {
            let c = t.dim(1)?;
            let t = if pad > 0 {
                Tensor::cat(&[t, &Tensor::zeros((pad, c), t.dtype(), t.device())?], 0)?
            } else {
                t.clone()
            };
            t.reshape((groups, GROUP, c))
        };
        let (gq, gk, gv, gu) = (
            grouped(&quad_q)?,
            grouped(&quad_k)?,
            grouped(&v)?,
            grouped(&u)?,
        );
        // padded keys are zero, so they get no attention (the mask in the original is redundant)
        let attn = (gq.matmul(&gk.transpose(1, 2)?.contiguous()?)? / GROUP as f64)?
            .relu()?
            .sqr()?;
        let quad_v = attn
            .matmul(&gv)?
            .reshape((groups * GROUP, e))?
            .narrow(0, 0, n)?;
        let quad_u = attn
            .matmul(&gu)?
            .reshape((groups * GROUP, e))?
            .narrow(0, 0, n)?;
        let lin_kt = lin_k.t()?.contiguous()?;
        let lin_v = lin_q.matmul(&(lin_kt.matmul(&v)? / n as f64)?)?;
        let lin_u = lin_q.matmul(&(lin_kt.matmul(&u)? / n as f64)?)?;
        let (att_v, att_u) = ((quad_v + lin_v)?, (quad_u + lin_u)?);
        let out = ((att_u * &v)? * sigmoid(&(att_v * &u)?)?)?;
        x + self.ffconvm(&out, &format!("{p}.to_out"))?
    }

    /// Gated_FSMN_Block_Dilated: 1x1 conv down to 256, gated FSMN memory over a dilated dense conv net, back up.
    fn fsmn(&self, x: &Tensor, p: &str) -> Result<Tensor> {
        let ln = |x: &Tensor, k: &str| -> Result<Tensor> {
            layer_norm(
                x,
                self.get(&format!("{p}.{k}.weight"))?,
                self.get(&format!("{p}.{k}.bias"))?,
                1e-5,
            )
        };
        let h = prelu(
            &self.linear(x, &format!("{p}.conv1.0"))?,
            self.get(&format!("{p}.conv1.1.weight"))?,
        )?;
        let h = ln(&h, "norm1")?;
        let g = format!("{p}.gated_fsmn");
        let xu = self.ffconvm(&h, &format!("{g}.to_u"))?;
        let xv = self.ffconvm(&h, &format!("{g}.to_v"))?;
        // UniDeepFsmn_dilated
        let f = format!("{g}.fsmn");
        let p1 = self.linear(
            &self.linear(&xu, &format!("{f}.linear"))?.relu()?,
            &format!("{f}.project"),
        )?;
        let mut skip = p1.clone();
        let mut out = p1.clone();
        for i in 0..2 {
            let dil = 1 << i;
            let pad = 20 + (dil - 1) * 19 - 1;
            let c = format!("{f}.conv");
            let w = self.get(&format!("{c}.conv{}.weight", i + 1))?.squeeze(3)?;
            out = grouped_conv(&skip, &w, dil, pad)?;
            // InstanceNorm2d(affine): per channel over time
            let o = out.broadcast_sub(&out.mean_keepdim(0)?)?;
            let var = o.sqr()?.mean_keepdim(0)?;
            out = o
                .broadcast_div(&(var + 1e-5)?.sqrt()?)?
                .broadcast_mul(self.get(&format!("{c}.norm{}.weight", i + 1))?)?
                .broadcast_add(self.get(&format!("{c}.norm{}.bias", i + 1))?)?;
            out = prelu(&out, self.get(&format!("{c}.prelu{}.weight", i + 1))?)?;
            skip = Tensor::cat(&[&out, &skip], 1)?;
        }
        let xu = (xu + out)?;
        let h = ((xv * xu)? + h)?;
        let h = ln(&h, "norm2")?;
        x + self.linear(&h, &format!("{p}.conv2"))?
    }

    /// One window through MossFormer: [samples] -> one track per voice, as long as the input.
    fn forward(&self, wav: &[f32]) -> Result<[Vec<f32>; 2]> {
        let dev = &self.dev;
        let hop = KERNEL / 2;
        let frames = (wav.len() - KERNEL) / hop + 1;
        let patches: Vec<f32> = (0..frames)
            .flat_map(|f| wav[f * hop..f * hop + KERNEL].to_vec())
            .collect();
        let enc_w = self
            .get("enc.conv1d.weight")?
            .squeeze(1)?
            .t()?
            .contiguous()?; // [16, 512]
        let enc = Tensor::from_vec(patches, (frames, KERNEL), dev)?
            .matmul(&enc_w)?
            .relu()?;

        let m = "mask_net";
        let x = group_norm(
            &enc,
            self.get(&format!("{m}.norm.weight"))?,
            self.get(&format!("{m}.norm.bias"))?,
        )?;
        let x = self.linear(&x, &format!("{m}.conv1d_encoder"))?;
        // ScaledSinuEmbedding
        let pos = Tensor::arange(0u32, frames as u32, dev)?
            .to_dtype(candle_core::DType::F32)?
            .unsqueeze(1)?;
        let sinu = pos.broadcast_mul(&self.get(&format!("{m}.pos_enc.inv_freq"))?.unsqueeze(0)?)?;
        let emb = Tensor::cat(&[sinu.sin()?, sinu.cos()?], 1)?
            .broadcast_mul(self.get(&format!("{m}.pos_enc.scale"))?)?;
        let x = (x + emb)?;

        let freqs = self.get(&format!("{BLOCKS}.layers.0.rotary_pos_emb.freqs"))?;
        let ang = pos.broadcast_mul(&freqs.unsqueeze(0)?)?;
        let (cos, sin) = (ang.cos()?, ang.sin()?);
        let mut h = x.clone();
        for i in 0..LAYERS {
            h = self.flash(&h, &format!("{BLOCKS}.layers.{i}"), &cos, &sin)?;
            h = self.fsmn(&h, &format!("{BLOCKS}.fsmn.{i}"))?;
        }
        let n = "mask_net.mdl.intra_mdl.norm";
        let h = layer_norm(
            &h,
            self.get(&format!("{n}.weight"))?,
            self.get(&format!("{n}.bias"))?,
            1e-6,
        )?;
        let h = group_norm(
            &h,
            self.get("mask_net.mdl.intra_norm.weight")?,
            self.get("mask_net.mdl.intra_norm.bias")?,
        )?;
        let h = (h + x)?;
        let h = prelu(&h, self.get(&format!("{m}.prelu.weight"))?)?;
        let h = self.linear(&h, &format!("{m}.conv1d_out"))?;
        let c = h.dim(1)? / 2;
        let dec_w = self.get("dec.weight")?.squeeze(1)?; // [512, 16]
        let track = |s: usize| -> Result<Vec<f32>> {
            let hs = h.narrow(1, s * c, c)?;
            let gate = (self.linear(&hs, &format!("{m}.output.0"))?.tanh()?
                * sigmoid(&self.linear(&hs, &format!("{m}.output_gate.0"))?)?)?;
            let mask = self.linear(&gate, &format!("{m}.conv1_decoder"))?.relu()?;
            // ConvTranspose1d: each frame adds KERNEL samples at frame * hop
            let pieces = (&enc * mask)?.matmul(&dec_w)?.to_vec2::<f32>()?;
            let mut out = vec![0f32; wav.len().max((frames - 1) * hop + KERNEL)];
            for (f, p) in pieces.iter().enumerate() {
                for (k, v) in p.iter().enumerate() {
                    out[f * hop + k] += v;
                }
            }
            out.truncate(wav.len());
            Ok(out)
        };
        Ok([track(0)?, track(1)?])
    }

    /// decode_one_audio_mossformer2_ss_16k: the clip's two voices, each scaled to the clip's RMS.
    pub fn separate(&self, clip: &[f32]) -> Result<[Vec<f32>; 2]> {
        let t = clip.len();
        let padded = if t < WINDOW {
            WINDOW
        } else if t < WINDOW + STRIDE {
            WINDOW + STRIDE
        } else if !(t - WINDOW).is_multiple_of(STRIDE) {
            t + t - (t - WINDOW) / STRIDE * STRIDE // ClearVoice's padding, kept as is
        } else {
            t
        };
        let mut x = clip.to_vec();
        x.resize(padded, 0.0);
        let mut out = if t > WINDOW {
            let give_up = (WINDOW - STRIDE) / 2;
            let mut out = [vec![0f32; padded], vec![0f32; padded]];
            let mut i = 0;
            while i + WINDOW <= padded {
                let tracks = self.forward(&x[i..i + WINDOW])?;
                let (from, to) = (if i == 0 { 0 } else { give_up }, WINDOW - give_up);
                for (o, tr) in out.iter_mut().zip(&tracks) {
                    o[i + from..i + to].copy_from_slice(&tr[from..to]);
                }
                i += STRIDE;
            }
            out
        } else {
            self.forward(&x)?
        };
        let rms = |v: &[f32]| {
            (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
        };
        let target = rms(clip);
        for o in &mut out {
            o.truncate(t);
            let r = rms(o);
            let k = if r > 0.0 { (target / r) as f32 } else { 0.0 };
            o.iter_mut().for_each(|v| *v *= k);
        }
        Ok(out)
    }
}

/// `ozen separate`: answer separation requests on stdin until it closes.
pub fn serve() -> std::result::Result<(), String> {
    let sep = Separator::load()?;
    let (mut inp, mut out) = (std::io::stdin().lock(), std::io::stdout().lock());
    out.write_all(READY)
        .and_then(|_| out.flush())
        .map_err(|e| e.to_string())?;
    let mut n = [0u8; 4];
    while inp.read_exact(&mut n).is_ok() {
        let mut buf = vec![0u8; u32::from_le_bytes(n) as usize * 4];
        inp.read_exact(&mut buf).map_err(|e| e.to_string())?;
        let clip: Vec<f32> = buf
            .as_chunks::<4>()
            .0
            .iter()
            .map(|b| f32::from_le_bytes(*b))
            .collect();
        let tracks = sep.separate(&clip).map_err(|e| e.to_string())?;
        let bytes: Vec<u8> = tracks
            .iter()
            .flatten()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        out.write_all(&bytes)
            .and_then(|_| out.flush())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The custom op equals candle's own (slow, per-group) conv1d, for depthwise and 2-in-per-group dilated kernels.
    #[test]
    fn grouped_conv_matches_conv1d() {
        let cpu = &Device::Cpu;
        for (c, m, k, dil) in [(6, 1, 17, 1), (4, 2, 39, 2)] {
            let pad = (k - 1) / 2 * dil;
            let x = Tensor::randn(0f32, 1.0, (50, c * m), cpu).unwrap();
            let w = Tensor::randn(0f32, 1.0, (c, m, k), cpu).unwrap();
            let got = grouped_conv(&x, &w, dil, pad).unwrap();
            let want = x.t().unwrap().unsqueeze(0).unwrap().contiguous().unwrap();
            let want = want
                .conv1d(&w, pad, 1, dil, c)
                .unwrap()
                .squeeze(0)
                .unwrap()
                .t()
                .unwrap();
            let diff = (got - want).unwrap().abs().unwrap().max_all().unwrap();
            assert!(diff.to_scalar::<f32>().unwrap() < 1e-4, "c={c} m={m}");
        }
    }
}
