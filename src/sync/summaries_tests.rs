use super::*;

fn entries(n: usize) -> Vec<Entry> {
    (0..n)
        .map(|i| ("lines".into(), format!("{i}"), 1, "0123456789abcdef".into()))
        .collect()
}

#[test]
fn a_summary_completes_once_every_part_is_in_whatever_the_order() {
    let mut p = Partial::default();
    assert_eq!(p.add(7, 2, 3, entries(2)).unwrap(), None);
    assert_eq!(p.add(7, 0, 3, entries(1)).unwrap(), None);
    assert_eq!(
        p.add(7, 0, 3, entries(1)).unwrap(),
        None,
        "a repeated part replaces itself"
    );
    assert_eq!(
        p.add(7, 1, 3, entries(3)).unwrap().map(|e| e.len()),
        Some(6)
    );
    assert_eq!((p.parts, p.entries), (0, 0), "nothing held once answered");
}

#[test]
fn a_hostile_stream_of_a_million_parts_stays_within_the_caps() {
    // OFE-65: a peer claiming the most parts allowed, over and over, with fresh ids
    let mut p = Partial::default();
    let mut refused = 0;
    for i in 0..1_000_000u32 {
        let id = u64::from(i / 4000); // a new summary every 4000 parts, none ever complete
        if p.add(id, i % 4000, PARTS as u32, entries(1)).is_err() {
            refused += 1;
        }
        assert!(p.parts <= PARTS && p.entries <= ENTRIES && p.by_id.len() <= SUMMARIES);
    }
    assert!(refused > 0, "the part cap was hit");
    assert!(
        p.add(1, 0, PARTS as u32 + 1, vec![]).is_err(),
        "more parts than allowed"
    );
    assert!(p.add(1, 5, 5, vec![]).is_err(), "a part past the end");
}

#[test]
fn too_many_entries_drop_the_summary() {
    let mut p = Partial::default();
    let per = ENTRIES / 2 + 1;
    assert!(p.add(1, 0, 3, entries(per)).unwrap().is_none());
    assert!(p.add(1, 1, 3, entries(per)).is_err());
    assert_eq!((p.parts, p.entries), (0, 0), "dropped, nothing held");
}

#[test]
fn a_real_summary_of_a_million_records_still_fits() {
    // about 700 entries per frame: a million records is ~1,430 parts
    let mut p = Partial::default();
    let n = 1_430u32;
    for i in 0..n - 1 {
        assert!(p.add(9, i, n, entries(700)).unwrap().is_none());
    }
    assert_eq!(
        p.add(9, n - 1, n, entries(700)).unwrap().map(|e| e.len()),
        Some(1_001_000)
    );
}
