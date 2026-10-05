//! What a received record must look like before it may touch the files (OFE-68). Records come from
//! other Macs, maybe a buggy or newer ozen; one with a wrong type (a string `t`, a 3-float voiceprint, a
//! missing id) would be written as-is and could crash the panel, the transcriber or retrain on every
//! Mac it reaches. Only the id is required (readers already cope with missing fields, and older data
//! has rows without some); known fields must have their type, and unknown extra fields pass (a newer
//! ozen may add some).
use serde_json::Value;

/// A voiceprint's length (ecapa.rs: 192 values).
const PRINT: usize = 192;
/// Latest time a line may carry: 2100-01-01, in seconds since 1970.
const LAST_T: f64 = 4_102_444_800.0;
/// Longest key and largest record: a record must fit one frame anyway (seal.rs).
const KEY: usize = 256;
const SIZE: usize = 60 << 10;

type Obj = serde_json::Map<String, Value>;

fn finite(x: &Value) -> Option<f64> {
    x.as_f64().filter(|f| f.is_finite())
}

/// `field` is absent or passes `ok`.
fn opt(o: &Obj, field: &str, ok: impl Fn(&Value) -> bool) -> Result<(), String> {
    match o.get(field) {
        Some(x) if !ok(x) => Err(format!("bad {field}: {x}")),
        _ => Ok(()),
    }
}

/// A row (line or place) keyed by its "id"; a tombstone needs nothing else.
fn row<'a>(key: &str, r: &'a Value) -> Result<Option<&'a Obj>, String> {
    let o = r.as_object().ok_or("not an object")?;
    if o.get("id").and_then(Value::as_str) != Some(key) {
        return Err("id doesn't match its key".into());
    }
    opt(o, "del", Value::is_boolean)?;
    Ok((o.get("del") != Some(&Value::Bool(true))).then_some(o))
}

fn line(o: &Obj) -> Result<(), String> {
    opt(o, "t", |t| {
        finite(t).is_some_and(|t| (0.0..=LAST_T).contains(&t))
    })?;
    opt(o, "text", Value::is_string)?;
    for f in ["spk", "src", "heard"] {
        opt(o, f, Value::is_string)?;
    }
    opt(o, "d", |d| finite(d).is_some_and(|d| d >= 0.0))?;
    opt(o, "doubt", |d| finite(d).is_some())?;
    opt(o, "e", |e| {
        e.is_null()
            || e.as_array()
                .is_some_and(|a| a.len() == PRINT && a.iter().all(|x| finite(x).is_some()))
    })
}

fn place(o: &Obj) -> Result<(), String> {
    opt(o, "action", |a| {
        matches!(a.as_str(), Some("record" | "meetings" | "off"))
    })?;
    opt(o, "label", Value::is_string)?;
    opt(o, "lat", |x| {
        finite(x).is_some_and(|x| (-90.0..=90.0).contains(&x))
    })?;
    opt(o, "lon", |x| {
        finite(x).is_some_and(|x| (-180.0..=180.0).contains(&x))
    })?;
    opt(o, "radius", |x| finite(x).is_some_and(|x| x > 0.0))
}

/// A map entry: {"v", "val"} or a tombstone {"v"}; `val` must pass `ok`. A bare value (from before
/// entries existed) is accepted if it passes `ok` too.
fn entry(e: &Value, ok: impl Fn(&Value) -> bool) -> Result<(), String> {
    match e.as_object() {
        Some(o) => opt(o, "val", ok),
        None if ok(e) => Ok(()),
        None => Err(format!("bad value: {e}")),
    }
}

/// Whether record `r`, received as (`kind`, `key`), may be merged. A kind this build doesn't know is
/// passed through (protocol.rs leaves it out of the merge, as from a newer ozen).
pub fn record(kind: &str, key: &str, r: &Value) -> Result<(), String> {
    let err = |e: String| format!("{kind} {key}: {e}");
    if key.is_empty() || key.len() > KEY {
        return Err(err("key empty or too long".into()));
    }
    if r.to_string().len() > SIZE {
        return Err(err("record too large".into()));
    }
    if let Some(o) = r.as_object() {
        opt(o, "v", Value::is_u64).map_err(err)?;
    }
    match kind {
        "lines" => row(key, r).and_then(|o| o.map_or(Ok(()), line)),
        "places" => row(key, r).and_then(|o| o.map_or(Ok(()), place)),
        "tags" | "fixes" => entry(r, Value::is_string),
        "vocab" => entry(r, |x| x == &Value::Bool(true)),
        _ => Ok(()),
    }
    .map_err(err)
}

/// The records of a received batch that may be merged, and why each of the others may not.
pub fn keep(recs: Vec<(String, String, Value)>) -> (Vec<(String, String, Value)>, Vec<String>) {
    let mut errors = vec![];
    let ok = recs
        .into_iter()
        .filter(|(k, key, r)| record(k, key, r).map_err(|e| errors.push(e)).is_ok())
        .collect();
    (ok, errors)
}

#[cfg(test)]
#[path = "valid_tests.rs"]
mod tests;
