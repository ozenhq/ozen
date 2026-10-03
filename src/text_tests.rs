use super::*;
use serde_json::json;

#[test]
fn filters_match_asr_py() {
    assert_eq!(words_of("Hello, World!  x_y"), ["hello", "world", "x_y"]);
    assert!(noise("תודה. תודה רבה.") && noise("Thank you.") && noise(""));
    assert!(!noise("Thank you, Kev."));
    // asr.py's doctest
    assert!(looped("Amen. Amen. Amen. Amen. Yeah."));
    assert!(!looped("Yeah. Yeah.") && !looped("זה קורה קורה קורה."));
}

#[test]
fn hint_echo_and_prompt_match_asr_py() {
    let hint = ["Kev".to_string(), "PR".to_string(), "Kev".to_string()];
    assert!(hint_echo("Kev. Kev. Kev.", &hint) && hint_echo("Kev, thanks. Kev.", &hint));
    assert!(!hint_echo("Kev.", &hint) && !hint_echo("Kev said Kev", &hint));
    assert_eq!(kept("Kev. Kev.", &hint), "");
    assert_eq!(kept("Open the PR, Kev.", &hint), "Open the PR, Kev.");
    assert_eq!(prompt(&hint, "hello"), "Kev, PR. hello");
    assert_eq!(prompt(&[], ""), ".");
    assert_eq!(prompt(&[], &"א".repeat(250)).chars().count(), 202);
}

#[test]
fn corrections_replace_whole_words_only() {
    let r = json!({"cave": "Kev", "p r": "PR"});
    let r = r.as_object().unwrap();
    assert_eq!(corrected("cave caves cave, p r.", r), "Kev caves Kev, PR.");
    assert_eq!(corrected("caveman cave", r), "caveman Kev"); // retries past a rejected match
    assert_eq!(corrected("שלום cave", r), "שלום Kev");
}

#[test]
fn junk_is_filler_loops_and_names_read_back() {
    let names: HashSet<String> = ["אורן", "דן"].map(String::from).into();
    assert!(junk("אורן דן, תודה רבה.", &names));
    assert!(!junk("דן, תביא את הקובץ", &names));
    assert!(junk("Okay. Okay. Okay.", &names));
}
