use super::*;

const KEY: Key = [7; 32];
const VAULT: &str = "ab12";

#[test]
fn round_trips() {
    for n in [0, 1, 500, 5000, MAX] {
        let p: Vec<u8> = (0..n).map(|i| i as u8).collect();
        assert_eq!(
            open(&KEY, VAULT, &seal(&KEY, VAULT, &p).unwrap()).unwrap(),
            p
        );
    }
}

#[test]
fn sealed_sizes_are_exactly_the_buckets() {
    let edge = |b: usize| b - NONCE - TAG - LEN;
    for (n, want) in [
        (0, BUCKETS[0]),
        (edge(BUCKETS[0]), BUCKETS[0]),
        (edge(BUCKETS[0]) + 1, BUCKETS[1]),
        (edge(BUCKETS[1]) + 1, BUCKETS[2]),
        (edge(BUCKETS[2]) + 1, BUCKETS[3]),
        (MAX, BUCKETS[3]),
    ] {
        assert_eq!(
            seal(&KEY, VAULT, &vec![1; n]).unwrap().len(),
            want,
            "{n} bytes"
        );
    }
    assert!(BUCKETS[3] < 64 << 10);
}

#[test]
fn too_big_is_an_error() {
    assert!(seal(&KEY, VAULT, &vec![0; MAX + 1]).is_err());
}

#[test]
fn nonces_differ() {
    assert_ne!(
        seal(&KEY, VAULT, b"x").unwrap(),
        seal(&KEY, VAULT, b"x").unwrap()
    );
}

#[test]
fn tampered_or_mismatched_frames_do_not_open() {
    let f = seal(&KEY, VAULT, b"tag edit").unwrap();
    for i in [0, 30, f.len() - 1] {
        let mut bad = f.clone();
        bad[i] ^= 1;
        assert!(open(&KEY, VAULT, &bad).is_err(), "flipped byte {i}");
    }
    assert!(open(&[8; 32], VAULT, &f).is_err(), "wrong key");
    assert!(open(&KEY, "ab13", &f).is_err(), "wrong vault id");
    assert!(open(&KEY, VAULT, &f[..f.len() - 1]).is_err(), "truncated");
    assert!(
        open(&KEY, VAULT, &[&f[..], &[0]].concat()).is_err(),
        "extended"
    );
    assert!(
        open(&KEY, VAULT, &f[..10]).is_err(),
        "truncated below the nonce"
    );
}
