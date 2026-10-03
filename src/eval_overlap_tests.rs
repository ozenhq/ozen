use super::*;

#[test]
fn mix_starts_b_a_second_in_at_a_level() {
    let m = mix(&[0.5; 4], &[0.1; SR]);
    assert_eq!(m.len(), 2 * SR);
    assert_eq!(&m[..4], &[0.5; 4]);
    assert!((m[SR] - 0.5).abs() < 1e-6); // b scaled to a's peak
}

#[test]
fn sample_is_fixed_and_without_repeats() {
    let v: Vec<usize> = (0..50).collect();
    let a = Rng(0).sample(&v, 10);
    assert_eq!(a, Rng(0).sample(&v, 10));
    let mut s = a.clone();
    s.sort();
    s.dedup();
    assert_eq!(s.len(), 10);
}

#[test]
fn words_counts_like_the_python_eval() {
    let w = words("Yeah, I mean -- that's THAT'S it.");
    assert_eq!(w["that's"], 2);
    assert_eq!(w["yeah"], 1);
    assert!(!w.contains_key("--"));
}
