#[test]
fn rms_of_float_and_int_chunks() {
    let dir = std::env::temp_dir().join(format!("ozen-compare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let write = |name: &str, spec: hound::WavSpec, f: &dyn Fn(&mut hound::WavWriter<_>)| {
        let p = dir.join(name);
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        f(&mut w);
        w.finalize().unwrap();
        p
    };
    let spec = |bits, sample_format| hound::WavSpec {
        channels: 1,
        sample_rate: 48000,
        bits_per_sample: bits,
        sample_format,
    };
    let float = write("f.wav", spec(32, hound::SampleFormat::Float), &|w| {
        for s in [0.5f32, -0.5, 0.5, -0.5] {
            w.write_sample(s).unwrap();
        }
    });
    let int = write("i.wav", spec(16, hound::SampleFormat::Int), &|w| {
        for s in [16384i16, -16384] {
            w.write_sample(s).unwrap();
        }
    });
    assert!((super::rms(&float).unwrap() - 0.5).abs() < 1e-9);
    assert!((super::rms(&int).unwrap() - 0.5).abs() < 1e-9);
    assert_eq!(super::rms(&dir.join("missing.wav")), None);
    use serde_json::json;
    assert_eq!(
        super::line(&json!({"heard": "שלום", "raw": "שלום."})),
        "שלום"
    );
    assert_eq!(
        super::line(&json!({"heard": "", "raw": "תודה רבה."})),
        "(dropped: תודה רבה.)"
    );
    assert_eq!(super::line(&json!({"heard": ""})), "");
    let _ = std::fs::remove_dir_all(&dir);
}
