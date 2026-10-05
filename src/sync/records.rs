//! The synced records here as a protocol version carries them, and their marks (protocol.rs).
use super::protocol::{Id, Mark};
use crate::crdt::{live_ids, v};
use crate::merge::{Records, Synced};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Every synced record in `s` (every kind in `crdt::SYNCED`) as protocol `version` carries it: from v2,
/// lines without the fields only this Mac means (`crdt::synced_line`). Rows written before ids get
/// theirs from `live_ids`, as `merge_rows` does.
pub(super) fn records(s: &Synced, version: u8) -> BTreeMap<Id, Value> {
    let mut out = BTreeMap::new();
    for (kind, _) in crate::crdt::SYNCED {
        match s.get(kind).expect("merge::Synced holds every synced kind") {
            Records::Map(m) => {
                for (k, e) in m {
                    out.insert((kind.into(), k.clone()), e.clone());
                }
            }
            Records::Rows(rows) => {
                for r in live_ids(rows) {
                    let r = match kind {
                        "lines" if version >= 2 => crate::crdt::synced_line(r),
                        _ => r,
                    };
                    let k = r
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    out.insert((kind.into(), k), Value::Object(r));
                }
            }
        }
    }
    out
}

pub(super) fn mark(r: &Value) -> Mark {
    let h = Sha256::digest(crate::crdt::canonical(r).as_bytes());
    (v(r), h[..8].iter().map(|b| format!("{b:02x}")).collect())
}
