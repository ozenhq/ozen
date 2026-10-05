//! A record too big for one frame syncs in parts (OFE-76).
use super::tests::{Mac, exchange, folder};
use super::*;
use serde_json::json;

fn long_line() -> Value {
    let text: String = (0..300 * 1024)
        .map(|i| (b'a' + (i % 26) as u8) as char)
        .collect();
    json!({"id": "1@a", "v": 1, "t": 1.0, "text": text})
}

#[test]
fn a_300_kib_line_reaches_the_other_mac_byte_identical() {
    let da = folder(json!([long_line()]), json!({}));
    let db = folder(json!([]), json!({}));
    let (mut a, mut b) = (Mac::new(da.path()), Mac::new(db.path()));
    exchange(&mut a, &mut b);
    let id = ("lines".to_string(), "1@a".to_string());
    assert_eq!(b.synced().get(&id), Some(&long_line()));
    assert_eq!(b.s.dropped, Dropped::default());
    // and once in sync, the next exchange sends nothing more
    assert_eq!(exchange(&mut a, &mut b), 0);
}

#[test]
fn a_garbled_part_is_dropped_and_counted() {
    let db = folder(json!([]), json!({}));
    let mut b = Mac::new(db.path());
    let mut p = parts::split("lines", "1@a", &long_line(), BUDGET)
        .unwrap()
        .remove(0);
    p.d = "not base64!".into();
    let frame = b.s.frame(&Msg::Part(p)).unwrap();
    assert!(b.receive(&frame).is_empty());
    assert_eq!(b.s.dropped.refused, 1);
    assert!(b.synced().is_empty());
}
