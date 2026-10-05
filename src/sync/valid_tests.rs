use super::*;
use serde_json::json;

fn print(n: usize) -> Value {
    json!(vec![0.5; n])
}

#[test]
fn real_records_pass() {
    let line = json!({"id": "1@a", "v": 3, "t": 1_760_000_000.5, "d": 2.1, "src": "mic", "run": 7,
        "spk": "Dana", "text": "hi", "heard": "hy", "doubt": 0.4, "e": print(192), "extra": [1]});
    let note = json!({"id": "n@a", "t": 5.0, "src": "note", "spk": "Note", "text": "x"});
    let pass = [
        ("lines", "1@a", line),
        ("lines", "n@a", note),
        ("lines", "2@b", json!({"id": "2@b", "v": 9, "del": true})),
        (
            "lines",
            "3",
            json!({"id": "3", "t": 1.0, "text": "", "e": null}),
        ),
        (
            "places",
            "p@a",
            json!({"id": "p@a", "v": 1, "label": "Home", "action": "off", "lat": 32.1, "lon": 34.8, "radius": 150.0}),
        ),
        ("places", "q@a", json!({"id": "q@a", "v": 2, "del": true})),
        ("tags", "1@a", json!({"v": 1, "val": "Dana"})),
        ("tags", "1@a", json!({"v": 2})),
        ("tags", "old", json!("Dana")),
        ("fixes", "1@a", json!({"v": 1, "val": "hi there"})),
        ("vocab", "Kev", json!({"v": 1, "val": true})),
        ("vocab", "Kev", json!({"v": 2})),
        ("kinds-from-a-newer-ozen", "k", json!(42)),
    ];
    for (k, key, r) in pass {
        assert_eq!(record(k, key, &r), Ok(()), "{k} {key} {r}");
    }
}

#[test]
fn malformed_records_fail() {
    let fail = [
        ("lines", "1", json!({"id": "1", "t": "noon", "text": "x"})),
        (
            "lines",
            "1",
            json!({"id": "1", "t": 1.0, "text": "x", "e": [0.1, 0.2, 0.3]}),
        ),
        (
            "lines",
            "1",
            json!({"id": "1", "t": 1.0, "text": "x", "e": vec!["a"; 192]}),
        ),
        ("lines", "1", json!({"t": 1.0, "text": "x"})),
        ("lines", "1", json!({"id": "2", "t": 1.0, "text": "x"})),
        ("lines", "1", json!({"id": "1", "t": -5.0, "text": "x"})),
        ("lines", "1", json!({"id": "1", "t": 9e12, "text": "x"})),
        ("lines", "1", json!({"id": "1", "t": 1.0, "text": 7})),
        (
            "lines",
            "1",
            json!({"id": "1", "t": 1.0, "text": "x", "spk": ["Dana"]}),
        ),
        (
            "lines",
            "1",
            json!({"id": "1", "t": 1.0, "text": "x", "d": -1}),
        ),
        (
            "lines",
            "1",
            json!({"id": "1", "v": -3, "t": 1.0, "text": "x"}),
        ),
        (
            "lines",
            "1",
            json!({"id": "1", "v": "9", "t": 1.0, "text": "x"}),
        ),
        ("lines", "1", json!({"id": "1", "del": "yes"})),
        ("lines", "1", json!("a line")),
        (
            "places",
            "p",
            json!({"id": "p", "label": "Home", "action": "explode"}),
        ),
        (
            "places",
            "p",
            json!({"id": "p", "label": "Home", "action": "off", "lat": 200.0}),
        ),
        (
            "places",
            "p",
            json!({"id": "p", "label": 3, "action": "off"}),
        ),
        ("tags", "1", json!({"v": 1, "val": 5})),
        ("tags", "1", json!(5)),
        ("fixes", "1", json!({"v": 1, "val": {"text": "x"}})),
        ("vocab", "Kev", json!({"v": 1, "val": "yes"})),
        ("tags", "", json!({"v": 1, "val": "Dana"})),
        ("tags", &"k".repeat(300), json!({"v": 1, "val": "Dana"})),
        (
            "lines",
            "1",
            json!({"id": "1", "t": 1.0, "text": "x".repeat(70_000)}),
        ),
    ];
    for (k, key, r) in fail {
        assert!(record(k, key, &r).is_err(), "{k} {key} {r} should fail");
    }
}
