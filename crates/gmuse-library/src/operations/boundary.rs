//! **The rule, in both directions** — `access-layers.md` §2.
//!
//! Three tests said "no `std::fs` under this directory" and none said where it
//! *was* allowed. A prohibition with nowhere to point is how `populate` came to
//! do 32 filesystem operations from inside `src/model/`: no rule was broken,
//! because no rule named a home.

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

/// Everything before a file's **test module**.
///
/// Not before the first `#[cfg(test)]`: that attribute also marks test-only
/// items in the middle of a file, and cutting there silently skipped the rest.
/// `scan.rs` has three of them by line 110 and 400 lines of production code
/// after, so a rule scoped this way was reading a fifth of the file and
/// passing.
fn production(text: &str) -> &str {
    let mut at = 0;
    while let Some(found) = text[at..].find("#[cfg(test)]") {
        let here = at + found;
        let rest = text[here..].trim_start_matches("#[cfg(test)]").trim_start();
        if rest.starts_with("mod ") {
            return &text[..here];
        }
        at = here + "#[cfg(test)]".len();
    }
    text
}

/// **These are the only things that open a file, and they really do.**
///
/// The positive half, which is the one that was missing. A named operation
/// that stopped touching the filesystem would be a named operation that had
/// moved somewhere else without anyone noticing — so this fails if one goes
/// quiet, not only if something else starts.
#[test]
fn the_named_operations_are_where_the_filesystem_is_reached() {
    let mut silent = Vec::new();
    for (file, text) in read("src/operations") {
        if file.ends_with("boundary.rs") || file.ends_with("mod.rs") {
            continue;
        }
        let body = production(&text);
        let touches = body.contains("std::fs")
            || body.contains("walkdir")
            || body.contains("File::open")
            || body.contains("audio_files");
        if !touches {
            silent.push(file);
        }
    }
    assert!(
        silent.is_empty(),
        "a named operation stopped reaching the filesystem — which means it is \
         no longer the thing this directory is for: {silent:#?}"
    );
}

/// **A named operation reads downward and never reaches an adapter.**
///
/// It may hold a projection and the roots. It may not know there is a player,
/// a `:` line or a Python module — the direction rule, for the layer that sits
/// beside the others rather than under them.
#[test]
fn a_named_operation_does_not_know_who_asked() {
    let forbidden = [
        "crate::ui",
        "crate::cmd",
        "crate::audio::",
        "crate::python",
        "ratatui",
        "crossterm",
        "cpal",
    ];
    let mut leaks = Vec::new();
    for (file, text) in read("src/operations") {
        if file.ends_with("boundary.rs") {
            continue;
        }
        for line in production(&text).lines().map(str::trim) {
            if !line.starts_with("use ") {
                continue;
            }
            for f in forbidden {
                if line.contains(f) {
                    leaks.push(format!("{file}: {line}"));
                }
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "a named operation reached for whoever called it: {leaks:#?}"
    );
}

/// **And the model is the domain again.**
///
/// `src/model/` holds entities and value types — what a recording is, how an
/// id is minted, how a name folds. It had `populate` in it, which walked the
/// library and appended facts, and nothing said that was odd because the
/// prohibitions all pointed at other directories.
///
/// `durable` is the exception and is named: it is the log's own reader and
/// writer, which is file access in service of L0 rather than of the library's
/// music.
#[test]
fn the_model_does_not_open_music() {
    let allowed = ["durable.rs", "ingest.rs"];
    let mut leaks = Vec::new();
    for (file, text) in read("src/model") {
        if allowed.iter().any(|a| file.ends_with(a)) {
            continue;
        }
        for line in production(&text).lines().map(str::trim) {
            if line.starts_with("//") {
                continue;
            }
            if line.contains("walkdir") || line.contains("File::open") {
                leaks.push(format!("{file}: {line}"));
            }
        }
    }
    assert!(
        leaks.is_empty(),
        "the model walked the library, which is a named operation's job: {leaks:#?}"
    );
}
