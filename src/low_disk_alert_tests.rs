#[test]
fn reads_free_space_from_df() {
    let df = "Filesystem 1024-blocks Used Available Capacity iused ifree %iused Mounted on\n\
              /dev/disk3s1s1 482797652 20046436 5347328 79% 459k 54M 1% /\n";
    let gb = super::free_gb(df).unwrap();
    assert!((gb - 5.1).abs() < 0.01, "{gb}");
    assert_eq!(super::free_gb(""), None);
}
