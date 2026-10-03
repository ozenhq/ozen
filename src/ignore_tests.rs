use super::*;

fn line(id: &str, e: &[f32]) -> (String, Vec<f32>) {
    (id.into(), unit(&json!(e)).unwrap())
}

#[test]
fn ignores_only_clear_matches_closer_than_any_person() {
    let video = [1.0, 0.0, 0.0];
    let me = [0.0, 1.0, 0.0];
    let lines = [
        line("video-again", &[0.95, 0.1, 0.0]),     // same voice
        line("near-threshold", &[0.45, 0.0, 0.89]), // 0.45 >= 0.4 but under 0.4 + margin
        line("me", &[0.6, 0.8, 0.0]),               // 0.6 to the video, but 0.8 to me
        line("tagged", &[1.0, 0.0, 0.0]),           // tagged lines keep their tag
    ];
    let tags: Map<String, Value> = [("tagged".to_string(), json!("Dana Levi"))]
        .into_iter()
        .collect();
    let ids: Vec<String> = matches(
        &lines,
        &tags,
        &[(IGNORE, video.to_vec())],
        &[me.to_vec()],
        0.4,
    )
    .into_iter()
    .map(|(id, _, _)| id)
    .collect();
    assert_eq!(ids, ["video-again"]);
    assert!(matches(&lines, &tags, &[], &[me.to_vec()], 0.4).is_empty());
}

#[test]
fn each_ignored_voice_keeps_its_own_name() {
    let lines = [
        line("tv", &[0.95, 0.1, 0.0]),
        line("radio", &[0.0, 0.1, 0.95]),
    ];
    let ignored = [
        ("Ignored", vec![1.0, 0.0, 0.0]),
        ("Ignored 2", vec![0.0, 0.0, 1.0]),
    ];
    let voices: Vec<&str> = matches(&lines, &Map::new(), &ignored, &[], 0.4)
        .into_iter()
        .map(|(_, v, _)| v)
        .collect();
    assert_eq!(voices, ["Ignored", "Ignored 2"]);
}

#[test]
fn fresh_names_the_next_free_ignored_voice() {
    let tags = |v: Value| v.as_object().unwrap().clone();
    assert_eq!(fresh(&tags(json!({"a": "Dana"}))), "Ignored");
    assert_eq!(fresh(&tags(json!({"a": "Ignored"}))), "Ignored 2");
    assert_eq!(
        fresh(&tags(json!({"a": "Ignored", "b": "Ignored 2"}))),
        "Ignored 3"
    );
    assert!(is_ignored("Ignored") && is_ignored("Ignored 12"));
    assert!(!is_ignored("Ignored TV") && !is_ignored("Ignoredx") && !is_ignored("Dana"));
}

proptest::proptest! {
    /// A new ignored voice never lands on a name already in use, and is always an ignored name.
    #[test]
    fn fresh_never_reuses_a_name(used in proptest::collection::vec(0u32..12, 0..10)) {
        let tags: Map<String, Value> = used
            .iter()
            .enumerate()
            .map(|(i, n)| (i.to_string(), json!(if *n < 2 { IGNORE.to_string() } else { format!("{IGNORE} {n}") })))
            .collect();
        let name = fresh(&tags);
        proptest::prop_assert!(is_ignored(&name));
        proptest::prop_assert!(!tags.values().any(|v| v.as_str() == Some(name.as_str())));
    }
}
