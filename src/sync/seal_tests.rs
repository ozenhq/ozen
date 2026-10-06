use super::*;

const KEY: Key = [7; 32];
const VAULT: &str = "ab12";

#[test]
fn round_trips() {
    for n in [0, 1, 500, 5000, MAX] {
        let p: Vec<u8> = (0..n).map(|i| i as u8).collect();
        assert_eq!(
            open(&KEY, VAULT, &seal(&KEY, VAULT, &p).unwrap()).unwrap(),
            p
        );
    }
}

#[test]
fn sealed_sizes_are_exactly_the_buckets() {
    let edge = |b: usize| b - NONCE - TAG - LEN;
    for (n, want) in [
        (0, BUCKETS[0]),
        (edge(BUCKETS[0]), BUCKETS[0]),
        (edge(BUCKETS[0]) + 1, BUCKETS[1]),
        (edge(BUCKETS[1]) + 1, BUCKETS[2]),
        (edge(BUCKETS[2]) + 1, BUCKETS[3]),
        (MAX, BUCKETS[3]),
    ] {
        assert_eq!(
            seal(&KEY, VAULT, &vec![1; n]).unwrap().len(),
            want,
            "{n} bytes"
        );
    }
    assert!(BUCKETS[3] < 64 << 10);
}

#[test]
fn too_big_is_an_error() {
    assert!(seal(&KEY, VAULT, &vec![0; MAX + 1]).is_err());
}

#[test]
fn nonces_differ() {
    assert_ne!(
        seal(&KEY, VAULT, b"x").unwrap(),
        seal(&KEY, VAULT, b"x").unwrap()
    );
}

#[test]
fn tampered_or_mismatched_frames_do_not_open() {
    let f = seal(&KEY, VAULT, b"tag edit").unwrap().into_bytes();
    for i in [0, 30, f.len() - 1] {
        let mut bad = f.clone();
        bad[i] ^= 1;
        assert!(open(&KEY, VAULT, &bad).is_err(), "flipped byte {i}");
    }
    assert!(open(&[8; 32], VAULT, &f).is_err(), "wrong key");
    assert!(open(&KEY, "ab13", &f).is_err(), "wrong vault id");
    assert!(open(&KEY, VAULT, &f[..f.len() - 1]).is_err(), "truncated");
    assert!(
        open(&KEY, VAULT, &[&f[..], &[0]].concat()).is_err(),
        "extended"
    );
    assert!(
        open(&KEY, VAULT, &f[..10]).is_err(),
        "truncated below the nonce"
    );
}

/// What enforces "only sealed frames leave the Mac" (OFE-28): `Sealed`'s field is private to seal.rs, so
/// nothing else can wrap plaintext in one, and both senders take only `Sealed` (talk.rs `write_frame` to a
/// same-network Mac, link.rs `send` to the relay). This pins the full list of places in src/sync that
/// write to a socket, so a new raw write path can't be added without changing this test: the
/// handshake's random nonce and HMAC proof (local.rs) carry no data, and talk.rs's private `write_bytes`
/// is what `write_frame` calls. (ozen is a binary crate, so a `compile_fail` doc-test can't run here.)
#[test]
fn every_socket_write_in_sync_is_a_sealed_frame_or_the_handshake() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/sync");
    let mut writes = vec![];
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
            continue;
        }
        for l in std::fs::read_to_string(&p).unwrap().lines() {
            let l = l.trim();
            let raw =
                l.contains(".write_all(") || l.contains("ws.send(") || l.contains("ws.write(");
            if raw && !l.starts_with("//") {
                writes.push(format!("{name}: {l}"));
            }
        }
    }
    writes.sort();
    assert_eq!(
        writes,
        [
            "link.rs: ws.send(Message::Binary(f.into_bytes().into()))", // `f: Sealed`
            "local.rs: s.write_all(&mine).map_err(err)?;",              // handshake nonce
            "local.rs: s.write_all(&proof(me).finalize().into_bytes())", // handshake proof
            "talk.rs: .and_then(|()| s.write_all(f))", // write_bytes, behind write_frame(&Sealed)
            "talk.rs: s.write_all(&(f.len() as u32).to_be_bytes())", // its length prefix
        ]
    );
}

/// The `compile_fail` proof a doc-test would give in a library crate: the real `Sealed` definition (cut
/// from seal.rs) in a module, and a sender that takes `Sealed`, compiled with rustc. Wrapping bytes as
/// `Sealed` outside its module, or handing the sender a `Vec<u8>`, must not compile; reading a frame's
/// bytes must (the control, so a broken setup can't pass for a refusal).
#[test]
fn sending_anything_but_a_sealed_frame_does_not_compile() {
    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/sync/seal.rs")).unwrap();
    let start = src
        .find("#[derive(Debug, Clone, PartialEq, Eq)]\npub struct Sealed")
        .unwrap();
    let end = src.find("/// `plaintext` encrypted").unwrap();
    let module = format!(
        "mod seal {{\n{}\npub fn seal(p: &[u8]) -> Sealed {{ Sealed(p.to_vec()) }}\n}}\n\
         fn send(f: seal::Sealed) -> usize {{ f.len() + f.into_bytes().len() }}\n",
        &src[start..end]
    );
    let dir = tempfile::tempdir().unwrap();
    let compiles = |name: &str, main: &str| {
        let file = dir.path().join(format!("{name}.rs"));
        std::fs::write(&file, format!("{module}fn main() {{ {main} }}\n")).unwrap();
        let out =
            std::process::Command::new(std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into()))
                .args(["--edition", "2024", "--crate-type", "bin", "-o"])
                .arg(dir.path().join(name))
                .arg(&file)
                .output()
                .unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    let (ok, err) = compiles("control", "let _ = send(seal::seal(b\"x\"));");
    assert!(ok, "the control must compile:\n{err}");
    let (ok, err) = compiles(
        "wrap",
        "let _ = send(seal::Sealed(b\"plaintext\".to_vec()));",
    );
    assert!(
        !ok && err.contains("private"),
        "wrapping plaintext as Sealed compiled:\n{err}"
    );
    let (ok, err) = compiles("raw", "let _ = send(b\"plaintext\".to_vec());");
    assert!(
        !ok && err.contains("mismatched types"),
        "sending a Vec<u8> compiled:\n{err}"
    );
}
