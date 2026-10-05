use super::*;

#[test]
fn a_mac_speaks_the_lowest_version_it_heard_while_it_still_can() {
    let mut v = Versions::new(2);
    assert_eq!((v.oldest(), v.speak()), (1, 2), "alone: its own");
    v.heard(2);
    assert_eq!(v.speak(), 2);
    v.heard(1);
    assert_eq!(v.speak(), 1, "a v1 Mac joined");
    v.heard(3);
    assert_eq!(v.speak(), 1, "the lowest wins");
    let mut old = Versions::new(3);
    old.heard(1);
    assert_eq!(
        old.speak(),
        3,
        "v1 is past what a v3 build writes: it tells the v1 Mac to update"
    );
}

#[test]
fn a_mac_reads_its_own_version_and_the_one_before() {
    let v = Versions::new(3);
    assert_eq!(
        [0, 1, 2, 3, 4].map(|x| v.read(x)),
        [Read::Older, Read::Older, Read::Yes, Read::Yes, Read::Newer]
    );
    assert_eq!(Versions::new(1).read(1), Read::Yes);
    assert_eq!(Versions::default().own(), VERSION);
}
