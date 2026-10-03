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
