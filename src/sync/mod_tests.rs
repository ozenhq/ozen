use super::*;

const VAULT: &str = "96e30ee41d949acadae54cca5765bad30e12e1539cf36207ddfd359431628c69";

#[test]
fn init_shows_only_the_short_vault_id() {
    for out in [
        report(VAULT, Some("wss://relay.example")),
        report(VAULT, None),
    ] {
        assert!(out.contains("vault 96e30ee4"), "{out}");
        assert!(
            !out.contains(&VAULT[..9]),
            "more than 8 chars of the id: {out}"
        );
    }
}

/// No format string in src/sync interpolates a vault id by name: anything shown goes through
/// key::short. (A heuristic: it catches `{vault}`, `{vault_id}` and `{self.vault}` captures.)
#[test]
fn no_source_in_sync_formats_a_full_vault_id() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sync");
    let mut found = vec![];
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        for (i, l) in std::fs::read_to_string(&p).unwrap().lines().enumerate() {
            if ["{vault}", "{vault_id}", "{self.vault}"]
                .iter()
                .any(|c| l.contains(c))
            {
                found.push(format!("{name}:{}: {}", i + 1, l.trim()));
            }
        }
    }
    assert!(found.is_empty(), "{found:#?}");
}
