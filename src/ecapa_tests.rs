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
