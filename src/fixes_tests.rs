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

mod prints_equal_the_map_parse {
    use super::*;
    use proptest::prelude::*;

    /// A lines.jsonl row as ozen or an older/odd writer might leave it.
    fn row() -> impl Strategy<Value = String> {
        let id = prop_oneof![
            Just(None),
            (0u8..6).prop_map(|i| Some(json!(format!("{i}@m")))), // repeats: duplicate ids
            Just(Some(json!(7))),
        ];
        let d = prop_oneof![
            Just(None),
            (0.0f64..3.0).prop_map(|d| Some(json!(d))),
            Just(Some(json!("2.5")))
        ];
        let e = prop_oneof![
            Just(None),
            Just(Some(Value::Null)),
            Just(Some(json!([]))),
            proptest::collection::vec(-1.0f64..1.0, 1..6).prop_map(|v| Some(json!(v))),
        ];
        (id, d, e).prop_map(|(id, d, e)| {
            let mut r = json!({"t": 1.0, "text": "said"});
            for (k, v) in [("id", id), ("d", d), ("e", e)] {
                if let Some(v) = v {
                    r[k] = v;
                }
            }
            r.to_string()
        })
    }

    proptest! {
        #[test]
        fn on_any_rows(rows in proptest::collection::vec(row(), 0..40)) {
            let _cwd = crate::CWD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let back = std::env::current_dir().unwrap();
            let dir = tempfile::tempdir().unwrap();
            std::env::set_current_dir(dir.path()).unwrap();
            fs::write(LINES, rows.join("\n")).unwrap();
            // what src/train.rs and src/ignore.rs read before: JSON maps, then id, d and e picked out
            let before: Vec<(String, Option<f64>, Vec<f64>)> = lines()
                .into_iter()
                .filter_map(|r| {
                    let e = r.get("e")?.as_array()?.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect();
                    Some((r.get("id")?.as_str()?.to_string(), r.get("d").and_then(Value::as_f64), e))
                })
                .collect();
            let now: Vec<_> = prints().iter().map(|p| (p.id.clone(), p.d, p.e.clone())).collect();
            std::env::set_current_dir(back).unwrap();
            prop_assert_eq!(now, before);
        }
    }
}
