//! **L0's boundary, asserted** — #191's mechanism, #193's split.
//!
//! `src/facts/` holds records and hands them back in order. It does not know
//! what a row is, what a query is, or that a projection exists — and until the
//! split these lived in the same 3,917-line file as `Catalogue` and `fold`, so
//! there was no boundary a wrong line would have had to cross.

use std::path::Path;

fn read(dir: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![Path::new(dir).to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&p) else {
            continue;
        };
        for e in entries.flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs")
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                out.push((path.display().to_string(), text));
            }
        }
    }
    out
}

fn imports(dir: &str) -> Vec<(String, String)> {
    read(dir)
        .into_iter()
        .filter(|(file, _)| !file.ends_with("boundary.rs"))
        .flat_map(|(file, text)| {
            text.lines()
                .map(str::trim)
                .filter(|l| l.starts_with("use "))
                .map(|l| (file.clone(), l.to_string()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// **The log does not know what is folded from it.**
///
/// The direction rule of `access-layers.md` §2, at the bottom layer: facts are
/// the truth and everything else is derived from them, so nothing here may
/// reach for a projection, a query, or a file.
#[test]
fn the_facts_do_not_know_what_reads_them() {
    let forbidden = [
        "crate::projection",
        "crate::query",
        "crate::ui",
        "crate::cmd",
        "crate::audio::",
        "crate::shelf",
        "walkdir",
        "lofty",
        "symphonia",
        "ratatui",
        "crossterm",
        "cpal",
    ];
    let mut leaks = Vec::new();
    for (file, line) in imports("src/facts") {
        for f in forbidden {
            if line.contains(f) {
                leaks.push(format!("{file}: {line}"));
            }
        }
    }
    assert!(leaks.is_empty(), "the log reached past itself: {leaks:#?}");
}

/// **A projection does not know who queries it.**
///
/// It may read facts — that is its whole job — and may not reach up. `Listened`
/// is the one exception and it is a *trait impl*: `Catalogue` supplies a
/// capability the query layer asks for, which is the direction this rule
/// allows, so the import is named here rather than pretended away.
#[test]
fn the_projection_does_not_know_who_reads_it() {
    let tolerated = [
        "crate::query::filter::Listened",
        "crate::query::filter::Named",
    ];
    let forbidden = [
        "crate::ui",
        "crate::cmd",
        "crate::audio::",
        "crate::shelf",
        "walkdir",
        "lofty",
        "symphonia",
        "ratatui",
        "crossterm",
        "cpal",
    ];
    let mut leaks = Vec::new();
    for (file, line) in imports("src/projection") {
        if tolerated.iter().any(|t| line.contains(t)) {
            continue;
        }
        for f in forbidden {
            if line.contains(f) {
                leaks.push(format!("{file}: {line}"));
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "the projection reached upward: {leaks:#?}"
    );
}

/// **Neither layer opens a file.**
///
/// The absolute rule of §2. A fact is appended by a named operation that has
/// already read what it needed; a projection folds facts and never bytes.
/// `durable` does the reading and lives in `src/model/`, which is where the
/// I/O belongs.
#[test]
fn neither_layer_reads_the_filesystem() {
    let mut leaks = Vec::new();
    for dir in ["src/facts", "src/projection"] {
        for (file, text) in read(dir) {
            if file.ends_with("boundary.rs") {
                continue;
            }
            let production = match text.find("#[cfg(test)]") {
                Some(at) => &text[..at],
                None => &text[..],
            };
            for line in production.lines().map(str::trim) {
                if line.starts_with("//") {
                    continue;
                }
                if line.contains("std::fs") {
                    leaks.push(format!("{file}: {line}"));
                }
            }
        }
    }
    assert!(leaks.is_empty(), "a layer opened a file: {leaks:#?}");
}
