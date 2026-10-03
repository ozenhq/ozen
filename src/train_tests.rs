use super::*;

#[test]
fn writes_json_like_python() {
    let v: IndexMap<String, Value> = serde_json::from_str(
        r#"{"b": [1, 0.5, 1e-05, 1.5e+16, -0.0001], "a": {}, "c": [], "d": "אורן"}"#,
    )
    .unwrap();
    assert_eq!(
        dump(&v, false),
        r#"{"b": [1, 0.5, 1e-05, 1.5e+16, -0.0001], "a": {}, "c": [], "d": "אורן"}"#
    );
    assert_eq!(
        dump(&v, true),
        "{\n \"b\": [\n  1,\n  0.5,\n  1e-05,\n  1.5e+16,\n  -0.0001\n ],\n \"a\": {},\n \"c\": [],\n \"d\": \"אורן\"\n}"
    );
}

#[test]
fn floats_like_python() {
    // Python's repr of each; the first is a tie between ...062 and ...063 that Python breaks to even.
    let cases = "-0.07583999633789062 0.1 100.0 1e+16 1.5e+16 1e-05 0.0001 123456.789 -2.5e-07 0.3 1e+100 1e+23";
    for want in cases.split(' ') {
        assert_eq!(py_float(want.parse().unwrap()), want);
    }
}

#[test]
fn rounds_and_calibrates_like_python() {
    assert_eq!(round(0.0005, 3), 0.001); // 0.0005 is just above half in binary
    assert_eq!(round(2.675, 2), 2.67); // just below
    assert_eq!(calibrate(&[0.9, 0.8], &[0.1, 0.2, 0.3]), DEFAULT_THRESHOLD); // too few to calibrate
    assert_eq!(calibrate(&[0.9, 0.8, 0.7], &[0.1, 0.2, 0.3]), 0.3); // first grid step above 0.3 (0.30000000000000004) separates them
    assert_eq!(slug("  Dana Levi! "), "dana-levi");
    assert_eq!(slug("אורן דן"), "אורן-דן");
}
