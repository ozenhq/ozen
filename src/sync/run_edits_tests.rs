//! Local edits of every synced kind follow to the other Mac on the connection's tick, without a new
//! hello (OFE-40). The tick looks at every synced file's stamp (protocol.rs `tick`), so it doesn't
//! matter which writer made the edit: the panel, `ozen tag`, `ozen fix`, the MCP tools or a merge.
use super::*;

/// The synced records in `dir`, as (kind, key) ids.
fn synced(dir: &Path) -> Vec<(String, String)> {
    let s = crate::merge::read_synced(&format!("{}/", dir.display()));
    let mut ids: Vec<(String, String)> =
        s.tags.keys().map(|k| ("tags".into(), k.clone())).collect();
    ids.extend(s.fixes.keys().map(|k| ("fixes".into(), k.clone())));
    ids.extend(s.vocab.keys().map(|k| ("vocab".into(), k.clone())));
    ids.extend(
        s.places
            .iter()
            .map(|p| ("places".into(), p["id"].as_str().unwrap_or("").into())),
    );
    ids.extend(
        s.lines
            .iter()
            .map(|r| ("lines".into(), r["id"].as_str().unwrap_or("").into())),
    );
    ids.sort();
    ids
}

#[test]
fn a_tag_fix_note_place_and_vocab_word_each_reach_the_other_mac_on_the_next_tick() {
    let (a, b) = (folder(&[line("1@a", "hi")]), folder(&[line("2@b", "yo")]));
    let key = [7; 32];
    let (_ma, _mb) = (mac(a.path(), &key), mac(b.path(), &key));
    wait_for("first exchange", || ids(b.path()).len() == 2);
    at(a.path(), || {
        fs::write(
            "tags.json",
            json!({"1@a": {"v": 5, "val": "Dana"}}).to_string(),
        )
        .unwrap();
        fs::write(
            "fixes.json",
            json!({"1@a": {"v": 5, "val": "hi there"}}).to_string(),
        )
        .unwrap();
        fs::write(
            "vocab.json",
            json!({"ozen": {"v": 5, "val": true}}).to_string(),
        )
        .unwrap();
        let gym = json!([{"id": "p1@a", "v": 5, "action": "off", "label": "Gym"}]);
        fs::write("places.json", gym.to_string()).unwrap();
        let note =
            json!({"id": "n1@a", "v": 5, "t": 2.0, "src": "note", "spk": "Dana", "text": "todo"});
        let mut f = fs::OpenOptions::new()
            .append(true)
            .open("lines.jsonl")
            .unwrap();
        std::io::Write::write_all(&mut f, format!("{note}\n").as_bytes()).unwrap();
    });
    let edited = Instant::now();
    wait_for("every edit on b", || synced(b.path()) == synced(a.path()));
    // the tick is every 2 s (run.rs TICK): ~2.1 s measured; 4 s leaves room for a loaded machine
    assert!(
        edited.elapsed() < Duration::from_secs(4),
        "took {:?}",
        edited.elapsed()
    );
    for kind in ["tags", "fixes", "vocab", "places"] {
        assert!(
            synced(b.path()).iter().any(|(k, _)| k == kind),
            "{kind} on b"
        );
    }
}

#[test]
fn a_burst_of_forty_tags_arrives_whole() {
    let (a, b) = (folder(&[line("1@a", "hi")]), folder(&[]));
    let key = [7; 32];
    let (_ma, _mb) = (mac(a.path(), &key), mac(b.path(), &key));
    wait_for("first exchange", || ids(b.path()).len() == 1);
    at(a.path(), || {
        // `ozen name` tagging 40 lines: 40 writes in a row
        let mut tags = serde_json::Map::new();
        for i in 0..40 {
            tags.insert(format!("{i}@a"), json!({"v": 5, "val": "Dana"}));
            fs::write("tags.json", Value::Object(tags.clone()).to_string()).unwrap();
        }
    });
    wait_for("40 tags on b", || {
        synced(b.path()).iter().filter(|(k, _)| k == "tags").count() == 40
    });
}
