use super::*;

#[test]
fn percentile_interpolates_like_numpy() {
    let v = [4.0, 1.0, 3.0, 2.0];
    assert_eq!(percentile(&v, 10.0), 1.3); // np.percentile([4,1,3,2], 10)
    assert_eq!(percentile(&v, 95.0), 3.85);
    assert_eq!(percentile(&[5.0], 50.0), 5.0);
}

#[test]
fn utterances_split_on_pauses_over_room_noise() {
    // overlap.py's self-check, with a tone for the voice: steady noise at SPEECH_RMS is no speech; a voice
    // 6 dB over it is, and a 0.5s pause splits it
    let mut seed = 1u64;
    let mut noise = || {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((seed >> 33) as f32 / (1u64 << 31) as f32 - 0.5) * 2.0 * SPEECH_RMS * 3f32.sqrt()
    };
    let room: Vec<f32> = (0..15 * SR).map(|_| noise()).collect();
    let floor = percentile(&frame_rms(&room), 10.0);
    assert!(
        utterances(&room, floor, 0.3).is_empty(),
        "room noise passed as speech"
    );
    let mut talk = room.clone();
    let tone = |i: usize| (i as f32 * 0.1).sin() * 4.0 * SPEECH_RMS;
    for (i, v) in talk.iter_mut().enumerate() {
        if (3 * SR..5 * SR).contains(&i) || (5 * SR + SR / 2..7 * SR).contains(&i) {
            *v += tone(i);
        }
    }
    let got = utterances(&talk, floor, 0.3);
    assert_eq!(
        got.len(),
        2,
        "{:?}",
        got.iter().map(|g| g.0).collect::<Vec<_>>()
    );
    assert!((got[0].0 - 2.91).abs() < 0.02 && (got[1].0 - 5.41).abs() < 0.02);
}

#[test]
fn unleak_silences_the_quieter_track() {
    let a = vec![1.0f32; FRAME * 2];
    let mut b = vec![0.05f32; FRAME * 2]; // under LEAK x the other track: leak
    b[FRAME..].fill(1.0);
    let out = unleak(&[a, b]);
    assert!(out[0].iter().all(|v| *v == 1.0));
    assert!(out[1][..FRAME].iter().all(|v| *v == 0.0) && out[1][FRAME..].iter().all(|v| *v == 1.0));
}
