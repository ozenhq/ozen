//! Speaker tagging and voiceprint training.
//!
//! Tags are written by `ozen tag <line-id> "Dana Levi"` (what the panel does), which then retrains.
//!
//! Retraining makes each person's voiceprint the average of every line tagged as them (kept in the
//! registry under samples/, so tags accumulate across meetings), measures leave-one-out accuracy
//! over the tags, calibrates the same-voice threshold from them (config.json, read live by the
//! transcriber), relabels untagged lines with a confidence and marks the least certain ones for you
//! to tag next (labels.json "unsure"), logs the trend (history.jsonl), and pushes the registry.
//! That is the loop: tag what it asks -> better prints and threshold -> fewer uncertain lines.
//!
//! Lines tagged IGNORE (`ozen ignore <line-id>...`) are not a person, so they never become a voiceprint here;
//! src/ignore.rs turns them into ignore.json and labels after this retrain, and the transcriber drops that voice.
//!
//! Registry files are written exactly as the Python version of this wrote them (json.dumps), so a Mac on
//! either version doesn't rewrite every file on the other's next retrain.
use crate::fixes::lines;
use crate::ignore::IGNORE;
use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;
use serde_json::ser::Formatter;
use serde_json::{Value, json};
use std::fs;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

const REPO: &str = "voices"; // clone of the voices registry
const TAGS: &str = "tags.json";
const LABELS: &str = "labels.json";
const STATS: &str = "stats.json";
const ECAPA: &str = "speechbrain/spkrec-ecapa-voxceleb";
const DEFAULT_THRESHOLD: f64 = 0.4; // until there are enough tags to calibrate one
const UNSURE: f64 = 0.08; // a line this close to the threshold, or to a second person, gets queued for review

/// A person's tagged lines: line id -> {"w": weight, "e": voiceprint}. Ordered, like the files.
type Samples = IndexMap<String, IndexMap<String, Value>>;

/// Python's json.dumps: ", " and ": " separators, or one-space indent; floats as Python's repr.
struct Py {
    indent: bool,
    depth: usize,
    has_value: bool,
}

impl Py {
    fn sep<W: ?Sized + io::Write>(&self, w: &mut W, first: bool) -> io::Result<()> {
        if !first {
            w.write_all(if self.indent { b"," } else { b", " })?;
        }
        if self.indent {
            w.write_all(b"\n")?;
            w.write_all(" ".repeat(self.depth).as_bytes())?;
        }
        Ok(())
    }

    fn close<W: ?Sized + io::Write>(&mut self, w: &mut W, c: &[u8]) -> io::Result<()> {
        self.depth -= 1;
        if self.indent && self.has_value {
            w.write_all(b"\n")?;
            w.write_all(" ".repeat(self.depth).as_bytes())?;
        }
        self.has_value = true;
        w.write_all(c)
    }
}

impl Formatter for Py {
    fn write_f64<W: ?Sized + io::Write>(&mut self, w: &mut W, v: f64) -> io::Result<()> {
        w.write_all(py_float(v).as_bytes())
    }
    fn begin_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.depth += 1;
        self.has_value = false;
        w.write_all(b"[")
    }
    fn end_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.close(w, b"]")
    }
    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.sep(w, first)
    }
    fn end_array_value<W: ?Sized + io::Write>(&mut self, _: &mut W) -> io::Result<()> {
        self.has_value = true;
        Ok(())
    }
    fn begin_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.depth += 1;
        self.has_value = false;
        w.write_all(b"{")
    }
    fn end_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.close(w, b"}")
    }
    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.sep(w, first)
    }
    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        w.write_all(b": ")
    }
    fn end_object_value<W: ?Sized + io::Write>(&mut self, _: &mut W) -> io::Result<()> {
        self.has_value = true;
        Ok(())
    }
}

/// Python's float repr: the shortest digits that round trip, the nearest of those (ties to even), as a
/// decimal from 1e-4 to 1e16 and otherwise like 1e-05 / 1.5e+16.
fn py_float(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return format!("{v:?}");
    }
    // Debug finds the shortest length but may pick the other of two equally near digit strings.
    let shortest = format!("{v:?}");
    let digits: String = shortest
        .split('e')
        .next()
        .unwrap()
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let n = digits.trim_matches('0').len().max(1);
    let exact = format!("{:.*e}", n - 1, v.abs()); // exact decimal, ties to even
    let (m, e) = exact.split_once('e').unwrap();
    let e: i32 = e.parse().unwrap();
    let d = m.replace('.', "");
    let d = match d.trim_end_matches('0') {
        "" => "0",
        d => d,
    };
    let body = if (-4..16).contains(&e) && e >= 0 {
        let e = e as usize;
        let int = format!("{:0<w$}", &d[..d.len().min(e + 1)], w = e + 1);
        format!(
            "{int}.{}",
            d.get(e + 1..).filter(|f| !f.is_empty()).unwrap_or("0")
        )
    } else if (-4..16).contains(&e) {
        format!("0.{}{d}", "0".repeat((-e - 1) as usize))
    } else {
        let frac = if d.len() > 1 {
            format!(".{}", &d[1..])
        } else {
            String::new()
        };
        format!(
            "{}{frac}e{}{:02}",
            &d[..1],
            if e < 0 { '-' } else { '+' },
            e.abs()
        )
    };
    format!("{}{body}", if v < 0.0 { "-" } else { "" })
}

fn dump(v: &impl Serialize, indent: bool) -> String {
    let mut out = vec![];
    let py = Py {
        indent,
        depth: 0,
        has_value: false,
    };
    v.serialize(&mut serde_json::Serializer::with_formatter(&mut out, py))
        .expect("json");
    String::from_utf8(out).expect("utf-8")
}

fn put(path: impl AsRef<Path>, text: &str) {
    let path = path.as_ref();
    fs::write(path, text).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

fn read<T: serde::de::DeserializeOwned>(path: impl AsRef<Path>) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Python's round(v, n): the nearest n-decimal value to v's exact binary value.
fn round(v: f64, n: usize) -> f64 {
    format!("{v:.n$}").parse().unwrap()
}

fn slug(name: &str) -> String {
    let re = Regex::new(r"[^\w]+").unwrap();
    re.replace_all(&name.trim().to_lowercase(), "-")
        .trim_matches('-')
        .to_string()
}

fn vec(e: &Value) -> Vec<f64> {
    e.as_array()
        .map(|a| a.iter().map(|x| x.as_f64().unwrap_or(0.0)).collect())
        .unwrap_or_default()
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The unit-length weighted sum of these samples' prints.
fn print_of<'a>(s: impl Iterator<Item = &'a IndexMap<String, Value>>) -> Vec<f64> {
    let mut sum: Vec<f64> = vec![];
    for x in s {
        let w = x.get("w").and_then(Value::as_f64).unwrap_or(1.0);
        let e = vec(&x["e"]);
        sum.resize(sum.len().max(e.len()), 0.0);
        for (a, b) in sum.iter_mut().zip(e) {
            *a += w * b;
        }
    }
    let n = dot(&sum, &sum).sqrt();
    sum.iter().map(|x| x / n).collect()
}

/// Match threshold that best separates same-person from different-person similarities in the tags.
fn calibrate(genuine: &[f64], impostor: &[f64]) -> f64 {
    if genuine.len() < 3 || impostor.len() < 3 {
        return DEFAULT_THRESHOLD;
    }
    let share = |xs: &[f64], f: &dyn Fn(f64) -> bool| {
        xs.iter().filter(|&&x| f(x)).count() as f64 / xs.len() as f64
    };
    let step = (0.25 + 0.01) - 0.25; // np.arange(0.25, 0.66, 0.01)'s grid, value for value
    let (mut best, mut best_score) = (0.25, f64::MIN);
    for i in 0..41 {
        let t = 0.25 + i as f64 * step;
        let score = (share(genuine, &|x| x >= t) + share(impostor, &|x| x < t)) / 2.0; // balanced accuracy
        if score > best_score {
            (best, best_score) = (t, score);
        }
    }
    round(best, 2)
}

/// git in the registry, given up on after a minute: offline, a sync failure must never lose the tag.
fn git(args: &[&str]) -> bool {
    let Ok(mut child) = Command::new("git")
        .arg("-C")
        .arg(REPO)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(s)) => return s.success(),
            Ok(None) if start.elapsed() < Duration::from_secs(60) => {
                sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

fn now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

pub fn retrain(retry: bool) {
    // Start from the newest registry. Local registry state is disposable: this machine's tags live in
    // tags.json and lines.jsonl (append-only), so rebuilding re-applies them on top of everyone else's.
    if git(&["fetch", "-q"]) {
        git(&["reset", "-q", "--hard", "@{u}"]);
    } else {
        eprintln!("registry fetch failed; training on the local copy");
    }
    // By id, the last copy of a line winning (two transcribers on one chunk).
    let lines: IndexMap<String, Value> = lines()
        .into_iter()
        .filter_map(|mut r| Some((r.get("id")?.as_str()?.to_string(), r.remove("e")?)))
        .filter(|(_, e)| e.as_array().is_some_and(|a| !a.is_empty()))
        .collect();
    let tags: IndexMap<String, Value> = read(TAGS).unwrap_or_default();
    let voices = Path::new(REPO).join("voices");
    let sample_dir = Path::new(REPO).join("samples");
    fs::create_dir_all(&voices).expect("create voices/voices");
    fs::create_dir_all(&sample_dir).expect("create voices/samples");
    let registry = || -> Vec<(std::path::PathBuf, IndexMap<String, Value>)> {
        let mut files: Vec<_> = fs::read_dir(&voices)
            .into_iter()
            .flatten()
            .flatten()
            .map(|f| f.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        files
            .into_iter()
            .filter_map(|p| {
                let v: IndexMap<String, Value> = read(&p).unwrap_or_default();
                (v.get("model").and_then(Value::as_str) == Some(ECAPA)).then_some((p, v))
            })
            .collect()
    };

    // The registry keeps tags from earlier meetings.
    let mut samples: IndexMap<String, Samples> = IndexMap::new();
    for (f, v) in registry() {
        let name = v
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let s = read(sample_dir.join(f.file_name().unwrap())).unwrap_or_else(|| {
            // voice enrolled before tagging existed: keep its print as one weighted sample
            let legacy = [
                ("w", v.get("count").cloned().unwrap_or(json!(1))),
                ("e", v["embedding"].clone()),
            ];
            [(
                "legacy".to_string(),
                legacy.map(|(k, x)| (k.to_string(), x)).into(),
            )]
            .into()
        });
        samples.insert(name, s);
    }
    for (sid, name) in &tags {
        // a retagged or cleared line leaves its old person
        for (n, s) in samples.iter_mut() {
            if Some(n.as_str()) != name.as_str() {
                s.shift_remove(sid);
            }
        }
    }
    for (sid, name) in &tags {
        let Some(name) = name.as_str().filter(|n| !n.is_empty() && *n != IGNORE) else {
            continue;
        };
        if let Some(e) = lines.get(sid) {
            let x = [("w".to_string(), json!(1)), ("e".to_string(), e.clone())];
            samples
                .entry(name.to_string())
                .or_default()
                .insert(sid.clone(), x.into());
        }
    }
    samples.retain(|_, s| !s.is_empty());

    let prints: IndexMap<&str, Vec<f64>> = samples
        .iter()
        .map(|(n, s)| (n.as_str(), print_of(s.values())))
        .collect();
    for (n, s) in &samples {
        let ws: Vec<&Value> = s.values().map(|x| &x["w"]).collect();
        let count = if ws.iter().all(|w| w.is_u64()) {
            json!(ws.iter().map(|w| w.as_u64().unwrap()).sum::<u64>())
        } else {
            json!(ws.iter().map(|w| w.as_f64().unwrap_or(0.0)).sum::<f64>())
        };
        let embedding: Vec<f64> = prints[n.as_str()].iter().map(|x| round(*x, 6)).collect();
        let rec = json!({"name": n, "model": ECAPA, "count": count, "embedding": embedding});
        let file = format!("{}.json", slug(n));
        put(
            voices.join(&file),
            &(dump(
                &ordered(rec, &["name", "model", "count", "embedding"]),
                true,
            ) + "\n"),
        );
        put(sample_dir.join(&file), &(dump(s, false) + "\n"));
    }
    for (f, v) in registry() {
        // everyone's tags were cleared: drop the person
        if !samples.contains_key(v.get("name").and_then(Value::as_str).unwrap_or_default()) {
            let _ = fs::remove_file(sample_dir.join(f.file_name().unwrap()));
            fs::remove_file(&f).expect("remove a cleared voice");
        }
    }

    // Leave-one-out: predict each tagged line from prints built without it. The same pass collects
    // genuine (own print) and impostor (other prints) similarities to calibrate the match threshold.
    let (mut correct, mut evaluated) = (0, 0);
    let (mut genuine, mut impostor) = (vec![], vec![]);
    for (n, s) in &samples {
        if s.len() < 2 {
            continue;
        }
        for (sid, x) in s.iter().filter(|(sid, _)| *sid != "legacy") {
            let e = vec(&x["e"]);
            let mut cand = prints.clone();
            cand[n.as_str()] = print_of(s.iter().filter(|(k, _)| *k != sid).map(|(_, y)| y));
            evaluated += 1;
            // max() keeps the first of equals, like Python's
            let top = cand.iter().fold(None, |b: Option<(&str, f64)>, (k, p)| {
                let d = dot(p, &e);
                if b.is_none_or(|(_, bd)| d > bd) {
                    Some((k, d))
                } else {
                    b
                }
            });
            correct += usize::from(top.is_some_and(|(k, _)| k == n));
            genuine.push(dot(&cand[n.as_str()], &e));
            impostor.extend(
                cand.iter()
                    .filter(|(m, _)| *m != n)
                    .map(|(_, p)| dot(p, &e)),
            );
        }
    }
    let threshold = calibrate(&genuine, &impostor);
    let config = json!({"same_speaker": threshold,
                        "calibrated_on": {"genuine": genuine.len(), "impostor": impostor.len()}});
    put(
        Path::new(REPO).join("config.json"),
        &(dump(&ordered(config, &["same_speaker", "calibrated_on"]), true) + "\n"),
    );

    let mut labels: IndexMap<String, IndexMap<String, Value>> = IndexMap::new();
    for (sid, e) in &lines {
        if tags.contains_key(sid) || prints.is_empty() {
            continue;
        }
        let e = vec(e);
        let mut ranked: Vec<(f64, &str)> = prints.iter().map(|(k, p)| (dot(p, &e), *k)).collect();
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(a.1)));
        let (best, name) = ranked[0];
        let margin = best - ranked.get(1).map_or(threshold, |r| r.0);
        // Unsure = near the threshold or nearly tied between two people: tagging these teaches the most.
        let doubt = (best - threshold).abs().min(margin);
        let label = json!({"spk": (best >= threshold).then_some(name), "sim": round(best, 3),
                           "margin": round(margin, 3), "unsure": doubt < UNSURE});
        labels.insert(
            sid.clone(),
            ordered(label, &["spk", "sim", "margin", "unsure"]),
        );
    }
    put(LABELS, &dump(&labels, false));

    let accuracy = (evaluated > 0).then(|| round(correct as f64 / evaluated as f64, 3));
    let tagged = tags
        .values()
        .filter(|v| v.as_str().is_some_and(|n| !n.is_empty() && n != IGNORE))
        .count();
    let people: IndexMap<&str, usize> = samples
        .iter()
        .map(|(n, s)| (n.as_str(), s.keys().filter(|k| *k != "legacy").count()))
        .collect();
    let point = [
        ("tagged", json!(tagged)),
        ("evaluated", json!(evaluated)),
        ("accuracy", json!(accuracy)),
        ("threshold", json!(threshold)),
    ];
    let hist_path = Path::new(REPO).join("history.jsonl");
    let mut hist: Vec<IndexMap<String, Value>> = fs::read_to_string(&hist_path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    if hist.last().is_none_or(|h| {
        point
            .iter()
            .any(|(k, v)| h.get(*k).unwrap_or(&Value::Null) != v)
    }) {
        let mut h: IndexMap<String, Value> = [("at".to_string(), json!(now()))].into();
        h.extend(point.iter().map(|(k, v)| (k.to_string(), v.clone())));
        let mut text = fs::read_to_string(&hist_path).unwrap_or_default();
        text += &(dump(&h, false) + "\n");
        put(&hist_path, &text);
        hist.push(h);
    }
    let first = hist
        .iter()
        .find_map(|h| h.get("accuracy").filter(|a| !a.is_null()).cloned());
    let stats: IndexMap<&str, Value> = [
        ("accuracy", json!(accuracy)),
        ("evaluated", json!(evaluated)),
        ("tagged", json!(tagged)),
        ("threshold", json!(threshold)),
        (
            "unsure",
            json!(labels.values().filter(|l| l["unsure"] == true).count()),
        ),
        ("people", json!(people)),
        ("accuracy_first", first.unwrap_or(Value::Null)),
    ]
    .into();
    put(STATS, &dump(&stats, true));

    git(&[
        "add",
        "-A",
        "voices",
        "samples",
        "config.json",
        "history.jsonl",
    ]);
    if !git(&["diff", "--cached", "--quiet"]) {
        let acc = accuracy.map_or("None".into(), py_float);
        let msg = format!(
            "Retrain voiceprints: {tagged} tagged lines, accuracy {acc}, threshold {}",
            py_float(threshold)
        );
        git(&["commit", "-m", &msg]);
    }
    if !git(&["push", "-q"]) {
        if retry {
            // another machine pushed since the fetch: rebuild on top of it, once
            return retrain(false);
        }
        eprintln!(
            "registry push failed; committed locally, rebuilt and pushed on the next retrain"
        );
    }
    println!("{}", dump(&stats, false));
}

/// `v`'s keys in this order (json! sorts them).
fn ordered(v: Value, keys: &[&str]) -> IndexMap<String, Value> {
    let Value::Object(mut m) = v else {
        unreachable!()
    };
    keys.iter()
        .map(|k| (k.to_string(), m.remove(*k).unwrap_or(Value::Null)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_json_like_python() {
        let v: IndexMap<String, Value> = serde_json::from_str(
            r#"{"b": [1, 0.5, 1e-05, 1.5e+16, -0.0001], "a": {}, "c": [], "d": "אורן"}"#,
        )
        .unwrap();
        assert_eq!(
            dump(&v, false),
            r#"{"b": [1, 0.5, 1e-05, 1.5e+16, -0.0001], "a": {}, "c": [], "d": "אורן"}"#
        );
        assert_eq!(
            dump(&v, true),
            "{\n \"b\": [\n  1,\n  0.5,\n  1e-05,\n  1.5e+16,\n  -0.0001\n ],\n \"a\": {},\n \"c\": [],\n \"d\": \"אורן\"\n}"
        );
    }

    #[test]
    fn floats_like_python() {
        // Python's repr of each; the first is a tie between ...062 and ...063 that Python breaks to even.
        let cases = "-0.07583999633789062 0.1 100.0 1e+16 1.5e+16 1e-05 0.0001 123456.789 -2.5e-07 0.3 1e+100 1e+23";
        for want in cases.split(' ') {
            assert_eq!(py_float(want.parse().unwrap()), want);
        }
    }

    #[test]
    fn rounds_and_calibrates_like_python() {
        assert_eq!(round(0.0005, 3), 0.001); // 0.0005 is just above half in binary
        assert_eq!(round(2.675, 2), 2.67); // just below
        assert_eq!(calibrate(&[0.9, 0.8], &[0.1, 0.2, 0.3]), DEFAULT_THRESHOLD); // too few to calibrate
        assert_eq!(calibrate(&[0.9, 0.8, 0.7], &[0.1, 0.2, 0.3]), 0.3); // first grid step above 0.3 (0.30000000000000004) separates them
        assert_eq!(slug("  Dana Levi! "), "dana-levi");
        assert_eq!(slug("אורן דן"), "אורן-דן");
    }
}
