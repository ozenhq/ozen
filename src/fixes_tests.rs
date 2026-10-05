use super::*;

#[test]
fn learns_added_words_and_repeated_corrections() {
    let r = rules(
        &[
            ("נפתח קב על זה", "נפתח ג'ירה על זה"),
            ("תשאל את קב, בסדר", "תשאל את Kev, בסדר"),
            ("ה-פי אר מוכן", "ה-PR מוכן"),
            ("פי אר חדש", "PR חדש"),
        ],
        LEARN,
    );
    assert_eq!(r["replace"], json!({"פי אר": "PR"})); // קב was fixed once each way: no rule
    assert_eq!(r["vocab"], json!(["PR", "ג'ירה", "Kev"]));
}

#[test]
fn no_replacement_where_a_fix_kept_the_phrase() {
    let r = rules(
        &[
            ("קב אמר", "Kev אמר"),
            ("שאלתי את קב", "שאלתי את Kev"),
            ("קב הזמן", "קב הזמן, בדיוק"),
        ],
        LEARN,
    );
    assert_eq!(r["replace"], json!({}));
    let r = rules(
        &[("קב אמר", "Kev אמר"), ("שאלתי את קב", "שאלתי את Kev")],
        LEARN,
    );
    assert_eq!(r["replace"], json!({"קב": "Kev"}));
}
