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
