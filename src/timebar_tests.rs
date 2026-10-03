use super::*;

#[test]
fn done_beats_waiting_beats_old() {
    let pace = concat!(
        r#"{"ms": 2000, "tag": "mic", "sec": 15.0, "done": 30.5, "took": 4.2, "lines": 2}"#,
        "\n",
        r#"{"ms": 3000, "tag": "call", "sec": 0.0, "done": 31.0, "took": 0.1, "lines": 0, "error": "bad wav"}"#,
        "\nnot json\n"
    );
    let ids = ["1000-mic-0", "1000-mic-1", "2000-mic-0", "note-1"];
    let waiting = ["2000-mic.wav".to_string(), "4000-local.wav".to_string()];
    let got = build(pace, &ids, &waiting);
    let states: Vec<(f64, &str, &str)> = got
        .iter()
        .map(|c| {
            (
                c["t"].as_f64().unwrap(),
                c["tag"].as_str().unwrap(),
                c["state"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        states,
        [
            (1.0, "mic", "old"),
            (2.0, "mic", "done"),
            (3.0, "call", "error"),
            (4.0, "local", "waiting")
        ]
    );
    assert_eq!(got[1]["took"], 4.2);
    assert_eq!(got[1]["lines"], 2);
    assert_eq!(got[2]["error"], "bad wav");
}
