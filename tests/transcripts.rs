//! **The wire, pinned** — #215.
//!
//! Each `tests/transcripts/*.ndjson` is a conversation: lines beginning `> `
//! are sent, lines beginning `< ` are what the engine must answer. A change to
//! one fails until it is updated on purpose, which is the point — **method
//! names and error shapes are public API** (§7.4) and rename only with a
//! protocol version bump.
//!
//! ## What is normalised, and nothing else
//!
//! `engine_generation` is `<generation>` and `engine_now_ns` is `0`, because
//! both are different every run by design: one is random per engine (§7.6), the
//! other is a clock. Everything else is compared byte for byte after a
//! canonical re-serialisation, so a reordered field is not a diff and a renamed
//! one is.
//!
//! Run `GMUSE_BLESS=1 cargo test --test transcripts` to rewrite them from what
//! the engine currently says. That is the deliberate act; doing it by accident
//! takes an environment variable, and the diff it produces is the API change.

use std::path::Path;

use gmuse::engine::{Answers, Identity, serve_line_for_test};
use gmuse_proto::{Call, RpcError};

struct Golden;

impl Answers for Golden {
    fn stream_generation(&self) -> u32 {
        0
    }
    fn now_ns(&self) -> i64 {
        0
    }
    fn call(&self, request: Call) -> Result<serde_json::Value, RpcError> {
        match request {
            Call::TimeNow => Ok(serde_json::json!({ "engine_now_ns": 0 })),
            other => Err(RpcError::method_not_found(other.method())),
        }
    }
}

/// A fixed identity, so the only unstable field is the generation.
fn identity() -> Identity {
    Identity::new(48_000, vec!["golden".into()])
}

/// Replace the values that cannot be stable with their placeholders.
fn normalise(line: &str, generation: &str) -> String {
    line.replace(generation, "<generation>")
}

/// Re-serialise so field order is the serialiser's, not the file's.
fn canonical(line: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) => serde_json::to_string(&v).unwrap_or_else(|_| line.to_string()),
        Err(_) => line.to_string(),
    }
}

fn replay(path: &Path) -> (Vec<String>, Vec<String>) {
    let text = std::fs::read_to_string(path).expect("transcript");
    let id = identity();
    let generation = id.engine_generation.clone();
    let answers = Golden;
    let mut greeted = false;

    let (mut expected, mut actual) = (Vec::new(), Vec::new());
    for line in text.lines() {
        if let Some(sent) = line.strip_prefix("> ") {
            let (reply, _fatal) = serve_line_for_test(sent, &id, &answers, &mut greeted);
            let body = reply
                .map(|r| serde_json::to_string(&r).expect("serialise"))
                .unwrap_or_default();
            if !body.is_empty() {
                actual.push(format!("< {}", normalise(&body, &generation)));
            }
            expected.push(format!("> {sent}"));
            actual.insert(actual.len() - 1, format!("> {sent}"));
        } else if line.starts_with("< ") {
            expected.push(line.to_string());
        }
    }
    (expected, actual)
}

#[test]
fn the_transcripts_still_describe_the_engine() {
    let dir = Path::new("tests/transcripts");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .expect("tests/transcripts exists — a golden suite that reads nothing passes for the wrong reason")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ndjson"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "no transcripts found in {}",
        dir.display()
    );

    let bless = std::env::var_os("GMUSE_BLESS").is_some();
    let mut failures = Vec::new();

    for path in &files {
        let (expected, actual) = replay(path);
        if bless {
            std::fs::write(path, format!("{}\n", actual.join("\n"))).expect("write");
            continue;
        }
        let same = expected.len() == actual.len()
            && expected.iter().zip(&actual).all(|(e, a)| {
                let (e, a) = (e.trim_start_matches("< "), a.trim_start_matches("< "));
                canonical(e) == canonical(a)
            });
        if !same {
            failures.push(format!(
                "\n--- {} ---\nexpected:\n{}\n\nactual:\n{}",
                path.display(),
                expected.join("\n"),
                actual.join("\n")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "the wire changed.{}\n\nIf that was the point, re-bless with \
         GMUSE_BLESS=1 cargo test --test transcripts — and remember that a \
         method name or an error shape is public API (§7.4).",
        failures.join("")
    );
}
