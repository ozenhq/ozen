use super::*;

fn case(text: &str) -> Case {
    Case {
        id: String::new(),
        teach: false,
        audio: PathBuf::new(),
        start: 0.0,
        duration: None,
        text: text.into(),
        kind: kind(text),
    }
}

#[test]
fn scores_words_terms_and_invented_words() {
    let (term, plain, noise) = (case("תפתח PR ל-Kev"), case("בוא נקבע פגישה"), case(""));
    let s = score(
        &[&term, &plain, &noise],
        &[
            "תפתח פי אר ל-Kev".into(),
            "בוא נקבע פגישה".into(),
            "PR תודה".into(),
        ],
    );
    assert_eq!((s.terms_hit, s.terms), (1, 2)); // Kev right, PR heard as פי אר
    assert_eq!((s.errors, s.words), (2, 7)); // PR -> פי אר: one substitution + one insertion
    assert_eq!((s.control_errors, s.control_words), (0, 3));
    assert_eq!(s.invented, 2); // noise clips never count toward WER
}
