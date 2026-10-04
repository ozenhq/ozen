use super::*;

#[test]
fn same_records_same_hashes_and_one_change_moves_one_bucket() {
    let a = [("tags", "x", 1, "aa"), ("lines", "1@a", 2, "bb")];
    let ha = hashes(a);
    assert_eq!(ha.len(), N * 16);
    assert_eq!(ha, hashes(a));
    assert!(differing(&ha, &ha).is_empty());
    let b = [("tags", "x", 2, "cc"), ("lines", "1@a", 2, "bb")];
    assert_eq!(differing(&ha, &hashes(b)), vec![of("tags", "x")]);
}

#[test]
fn malformed_hashes_mean_every_bucket() {
    let ha = hashes([]);
    assert_eq!(differing(&ha, &[1, 2, 3]).len(), N);
}
