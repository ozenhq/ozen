use super::*;

#[test]
fn launchers_run_the_agent_in_their_own_folder() {
    let s = command_script("hermes");
    assert!(s.starts_with("#!/bin/zsh -il\n"));
    assert!(s.contains(r#"cd "${0:A:h}" && export TERMINAL_CWD="$PWD" && exec hermes"#));
}

fn meeting(t: f64, texts: &[&str]) -> Meeting {
    let lines = texts
        .iter()
        .map(|s| Line {
            t,
            text: s.to_string(),
        })
        .collect();
    Meeting {
        id: (t as i64).to_string(),
        lines,
    }
}

#[test]
fn excerpts_keep_the_start_and_the_end() {
    let m = meeting(0.0, &["abcdef", "ghijkl"]);
    assert_eq!(m.excerpt(100), "abcdef\nghijkl");
    assert_eq!(m.excerpt(6), "abc\n[…]\njkl");
}

#[test]
fn the_folder_holds_the_transcripts_and_says_what_they_are() {
    let dir = std::env::temp_dir().join(format!("ozen-test-{}", std::process::id()));
    let d = dir.to_str().unwrap();
    let (a, b) = (
        meeting(1e9, &["[x] A (room): hi"]),
        meeting(2e9, &["[y] B (call): yo"]),
    );
    write_folder(d, &[&a], Some(&[(&b, 0.87)]), "").unwrap();
    let agents = fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(agents.contains(&format!("Picked by the user: `{}`", a.file())));
    assert!(agents.contains(&format!("`{}` (0.87)", b.file())));
    assert_eq!(
        fs::read_to_string(dir.join(a.file())).unwrap(),
        "[x] A (room): hi\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("CLAUDE.md")).unwrap(),
        "@AGENTS.md\n"
    );
    let mode = fs::metadata(dir.join("hermes.command"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111, "launchers must be executable");
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn only_a_meeting_with_a_recent_line_is_live() {
    let ms = vec![meeting(100.0, &["old"]), meeting(5000.0, &["now"])];
    assert_eq!(
        current(&ms, 5000.0 + GAP).map(|m| m.id.as_str()),
        Some("5000")
    );
    assert!(current(&ms, 5001.0 + GAP).is_none());
    assert!(current(&[], 0.0).is_none());
}

#[test]
fn refreshing_a_folder_replaces_its_transcripts_but_keeps_the_folder() {
    let dir = std::env::temp_dir().join(format!("ozen-live-test-{}", std::process::id()));
    let d = dir.to_str().unwrap();
    let (old, new) = (meeting(1e9, &["old"]), meeting(2e9, &["new"]));
    write_folder(d, &[&old], None, "").unwrap();
    fs::write(dir.join("notes.txt"), "the agent's own file").unwrap();
    write_folder(d, &[&new], None, "\n\nStill going.").unwrap();
    assert!(!dir.join(old.file()).exists());
    assert_eq!(fs::read_to_string(dir.join(new.file())).unwrap(), "new\n");
    assert!(dir.join("notes.txt").exists());
    assert!(
        fs::read_to_string(dir.join("AGENTS.md"))
            .unwrap()
            .contains("Still going.")
    );
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn open_refuses_unknown_targets_and_missing_launchers() {
    assert!(
        open("/nonexistent", "vim")
            .unwrap_err()
            .contains("use claude, hermes or finder")
    );
    assert!(
        open("/nonexistent", "claude")
            .unwrap_err()
            .contains("doesn't exist")
    );
}

#[test]
fn an_unreachable_kev_is_an_error_that_says_how_to_start_it() {
    let m = meeting(0.0, &["x"]);
    let err = kev_related("http://127.0.0.1:9/v1/systemone", &[&m], &[&m]).unwrap_err();
    assert!(
        err.contains("Kev isn't answering") && err.contains("kev.serve"),
        "{err}"
    );
}

#[test]
fn junk_lines_stay_out_of_the_export() {
    let raw = concat!(
        r#"{"id": "a", "t": 2, "spk": "S1", "src": "room", "text": "real"}"#,
        "\n",
        r#"{"id": "b", "t": 1, "spk": "S1", "src": "room", "text": "אורן דן, תודה רבה."}"#,
        "\n",
    );
    let out = render(raw, &Value::Null, &Value::Null, &serde_json::json!(["b"]));
    assert_eq!(out.len(), 1);
    assert!(out[0].text.ends_with("S1 (room): real"));
    assert_eq!(
        render(raw, &Value::Null, &Value::Null, &Value::Null).len(),
        2
    ); // no junk.json: all lines
}

#[test]
fn a_long_silence_starts_a_new_meeting() {
    let l = |t: f64| Line {
        t,
        text: String::new(),
    };
    let ms = split(vec![
        l(100.0),
        l(100.0 + GAP),
        l(101.0 + 2.0 * GAP),
        l(102.0 + 2.0 * GAP),
    ]);
    assert_eq!(
        ms.iter()
            .map(|m| (m.id.as_str(), m.lines.len()))
            .collect::<Vec<_>>(),
        [("100", 2), ("1301", 2)]
    );
}
