//! **The boundary, asserted rather than documented** — #191.
//!
//! Modelled on `model/seam.rs`'s `the_model_does_not_know_the_player_exists`,
//! which walks `use` lines and fails on a forbidden import. That test has
//! caught two live mistakes, which is two more than the paragraph it replaced.

use std::path::Path;

/// Every `.rs` file in a directory, with its text.
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

/// The `use` lines of a directory, minus this file — which reads the source
/// tree by definition and would otherwise trip over its own list.
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

/// **A read never opens a music file.**
///
/// The absolute rule of `access-layers.md` §2, and the one that stops the
/// answer to a question depending on what a tagger last wrote. An `artist=`
/// filter returning four rows used to parse the tags of 7,267 files; nothing
/// in the type system said it should not, and nothing in a review would have
/// caught the line that did it.
#[test]
fn the_query_layer_opens_no_music() {
    let forbidden = [
        "walkdir",
        "lofty",
        "symphonia",
        "crate::audio_id",
        "crate::scan",
    ];

    // **One test fixture reaches sideways, and it is named rather than
    // exempted.** `sort`'s tests build a `Holdings` to sort, which is a
    // scanner type; nothing is read from disk and no production line does it.
    // Listed so that a *second* one fails, which a blanket `#[cfg(test)]`
    // exemption would have let through silently.
    let tolerated = ["src/query/sort.rs: use crate::scan::Holdings as Scan;"];

    let mut leaks = Vec::new();
    for (file, line) in imports("src/query") {
        let at = format!("{file}: {line}");
        if tolerated.contains(&at.as_str()) {
            continue;
        }
        for f in forbidden {
            if line.contains(f) {
                leaks.push(at.clone());
                break;
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "the query layer reached for a file reader: {leaks:#?}"
    );
}

/// **A read does not reach past the projection to the player.**
///
/// The same direction rule the model already keeps: a layer may call the one
/// beneath it, and nothing may reach up.
#[test]
fn the_query_layer_does_not_know_the_player_exists() {
    let forbidden = [
        "crate::ui",
        "crate::cmd",
        "crate::audio::",
        "crate::shelf",
        "ratatui",
        "crossterm",
        "cpal",
    ];
    let mut leaks = Vec::new();
    for (file, line) in imports("src/query") {
        for f in forbidden {
            if line.contains(f) {
                leaks.push(format!("{file}: {line}"));
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "the query layer reached upward: {leaks:#?}"
    );
}

/// **`std::fs` in the query layer, counted, so the number may only fall.**
///
/// A ratchet rather than an assertion, which is how `src/ui/`'s `Store` count
/// already works and the only honest shape for it today: [`super::rows`] walks
/// the library because nothing else notices a file that arrived — `:rescan` is
/// a stub and a launch no longer scans (#64).
///
/// **The number is written out loud** so a regression is a diff rather than a
/// silent pass. #195 takes it to zero, and only together with #196, because
/// cutting the walk without giving the filesystem a door of its own would swap
/// a slow-but-current library for a fast-and-quietly-wrong one.
///
/// The evaluator is already clean and this is worth saying: `filter`, `format`
/// and `sort` reach for no file at all. What is left is the row assembly.
#[test]
fn the_walk_that_is_left_is_counted_and_may_only_shrink() {
    /// Every `std::fs` mention under `src/query`, today.
    const TODAY: usize = 5;
    /// And every one of them is in this file.
    const ONLY_IN: &str = "rows.rs";

    let mut found = Vec::new();
    for (file, text) in read("src/query") {
        if file.ends_with("boundary.rs") {
            continue;
        }
        for line in text.lines() {
            // **Code, not prose.** The module doc above explains this very
            // ratchet and would otherwise trip it, which is the same trap
            // `the_model_does_not_know_the_player_exists` avoids by reading
            // only `use` lines.
            let code = line.trim();
            if code.starts_with("//") {
                continue;
            }
            if code.contains("std::fs") {
                found.push((file.clone(), line.trim().to_string()));
            }
        }
    }

    for (file, line) in &found {
        assert!(
            file.ends_with(ONLY_IN),
            "the walk spread beyond {ONLY_IN}: {file}: {line}"
        );
    }
    assert!(
        found.len() <= TODAY,
        "the query layer reads more files than it did: {} against {TODAY}\n{found:#?}",
        found.len(),
    );
    assert_eq!(
        found.len(),
        TODAY,
        "the count fell to {} — good, and TODAY should now say so",
        found.len(),
    );
}
