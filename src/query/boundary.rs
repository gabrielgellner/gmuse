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

/// **The query layer opens no file. Asserted, not counted** — #195.
///
/// This was a ratchet at five, because [`super::rows`] walked the library on
/// every read: nothing else noticed a file that arrived, since `:rescan` was a
/// stub and a launch no longer scans (#64). `shelf scan` is that door now
/// (#196), so the walk is gone and the number is zero.
///
/// **What it bought is not only speed.** A read assembled partly from the log
/// and partly from the disk answers differently depending on what a tagger
/// last wrote, and nothing in a signature said so — 36 rows of this library
/// were showing a file's tag over the library's own record. A read is a
/// function of the projection now: the same records give the same rows, on any
/// machine, whatever is on disk that nobody has declared.
///
/// Test fixtures are exempt and nothing else is. They write temp files to have
/// something to fold; production code here opens nothing at all.
#[test]
fn the_query_layer_opens_no_file_at_all() {
    let mut leaks = Vec::new();
    for (file, text) in read("src/query") {
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
            if line.contains("std::fs") || line.contains("File::open") {
                leaks.push(format!("{file}: {line}"));
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "a read opened a file — which is how the answer starts depending on \
         what a tagger last wrote: {leaks:#?}"
    );
}
