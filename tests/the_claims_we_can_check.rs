//! **Claims a machine can hold** — #252.
//!
//! §13's testing rule works because a test is executable: "has this stopped being
//! true" is a question something can be made to answer. A sentence in a comment is
//! not, and the two failure modes are indistinguishable from outside — nothing happens
//! either way.
//!
//! The audit behind #250 found ten claims that had gone false and nothing that had
//! noticed. #252 records the decision about which of them are worth an instrument; this
//! file is the instrument. It holds the one check that is **local, deterministic, and
//! needs no issue tracker** — so it can live in `just ci` rather than in a habit.
//!
//! ## What it checks, and why this one
//!
//! An unreachable `pub` item in a library crate is invisible: `dead_code` does not fire
//! for it, so `clippy -D warnings` cannot. That invisibility is how §7.10's
//! disconnect policy stayed unimplemented — the only trace was an unreachable
//! `Outbox::over_limit`, and the comment beside it handed the decision to "whoever owns
//! the socket", who was nobody (#251).
//!
//! So every unreferenced `pub fn` must carry `#[allow(dead_code, reason = …)]`. The
//! point is **not** the attribute; it is that writing the reason forces somebody to say
//! out loud what would give the function a caller. Every one of the four that already
//! existed does exactly that, and they are the best-documented deferrals in the
//! codebase precisely because the compiler made someone write them.
//!
//! ## What it deliberately does not check
//!
//! Whether a deferral's cited issue is still open. That needs the tracker, and a test
//! that needs the network is a test that gets `#[ignore]`d the first time it fails on a
//! train. `just stale-claims` does it as a recipe instead — see #252 for why that split
//! is the decision rather than a compromise.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Every `.rs` file in the workspace, production and test alike.
fn sources() -> Vec<(PathBuf, String)> {
    let mut roots = vec![PathBuf::from("src"), PathBuf::from("tests")];
    let crates = std::fs::read_dir("crates").expect("crates/ is where the workspace lives");
    for entry in crates.flatten() {
        let src = entry.path().join("src");
        if src.is_dir() {
            roots.push(src);
        }
    }

    // **This file is excluded.** It is the scanner, so its own pattern strings are
    // found by its own scan — which is not a finding, it is a mirror. Caught on the
    // first run, when it reported four of its own `lower.contains(…)` lines as
    // ownerless deferrals.
    let me = Path::new(file!()).file_name().map(|n| n.to_owned());

    let mut out = Vec::new();
    let mut stack = roots;
    while let Some(at) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&at) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "rs")
                && path.file_name().map(|n| n.to_owned()) != me
                && let Ok(text) = std::fs::read_to_string(&path)
            {
                out.push((path, text));
            }
        }
    }
    out
}

/// The half of a file before its test module.
///
/// Crude on purpose: a definition inside `#[cfg(test)]` is a test helper and not a
/// claim about the shipped surface.
fn production(text: &str) -> &str {
    match text.find("#[cfg(test)]") {
        Some(at) => &text[..at],
        None => text,
    }
}

/// The attributes and doc comments attached to the item on `line`.
///
/// **Walks back rather than forward**, because an `#[allow]` may sit either side of the
/// doc comment — `instance.rs` had three that preceded it. Stops at a blank line or at
/// anything that looks like the end of a previous item, which is what separates one
/// item's attributes from the one above it.
fn attached<'a>(lines: &[&'a str], line: usize) -> Vec<&'a str> {
    let mut out = Vec::new();
    for above in lines[..line].iter().rev() {
        let t = above.trim();
        if t.is_empty() || t.ends_with('}') || t.ends_with(';') {
            break;
        }
        out.push(*above);
    }
    out
}

/// **Every unreachable `pub fn` says why it is still here** — #252, #251.
///
/// A `pub` item in a library crate never trips `dead_code`, so this is the only place
/// an unreferenced one is reported at all. The requirement is an `#[allow(dead_code)]`
/// with a `reason`, because the reason is the part that carries information: it is
/// where somebody records what would give the function a caller, and #251 is what
/// happens when nobody has to.
#[test]
fn an_unreachable_public_function_says_why_it_is_here() {
    let files = sources();

    // **The instrument is attached.** Every assertion below is over a set that a
    // missing directory or a bad glob would silently empty — the shape that turned a
    // boundary test into `assert!(0 <= 0)` in #213.
    assert!(
        files.len() > 80,
        "only {} source files found from {:?} — this test reads nothing and would pass \
         for the wrong reason",
        files.len(),
        std::env::current_dir().unwrap_or_default()
    );

    // name -> (file, is it marked)
    let mut declared: BTreeMap<String, (PathBuf, bool)> = BTreeMap::new();
    for (path, text) in &files {
        let live = production(text);
        let lines: Vec<&str> = live.lines().collect();
        for (n, line) in lines.iter().enumerate() {
            let t = line.trim_start();
            let Some(rest) = t
                .strip_prefix("pub fn ")
                .or_else(|| t.strip_prefix("pub const fn "))
            else {
                continue;
            };
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_')
                .collect();
            if name.is_empty() {
                continue;
            }
            let marked = attached(&lines, n).iter().any(|a| a.contains("dead_code"));
            declared.insert(name, (path.clone(), marked));
        }
    }

    assert!(
        declared.len() > 300,
        "only {} public functions found, which is too few to be the whole workspace",
        declared.len()
    );

    // **Names the trait methods and the conventions cannot speak for.** `new` and
    // friends are reached through a trait or a derive, so a source scrape cannot see
    // the call; counting them would make this test a list of exceptions.
    const BY_CONVENTION: &[&str] = &[
        "new", "default", "fmt", "from", "drop", "next", "len", "is_empty", "clone", "of", "parse",
        "render", "main", "eq", "cmp", "hash", "as_str", "deref",
    ];

    let mut silent: Vec<String> = Vec::new();
    for (name, (path, marked)) in &declared {
        if *marked || BY_CONVENTION.contains(&name.as_str()) {
            continue;
        }
        let mut referenced = false;
        'files: for (_, text) in &files {
            for line in text.lines() {
                let t = line.trim_start();
                // The definition itself, and prose about it, are not uses.
                if t.starts_with("pub fn ") || t.starts_with("pub const fn ") {
                    continue;
                }
                if t.starts_with("//") {
                    continue;
                }
                if mentions(line, name) {
                    referenced = true;
                    break 'files;
                }
            }
        }
        if !referenced {
            silent.push(format!("{name}  ({})", path.display()));
        }
    }

    assert!(
        silent.is_empty(),
        "these public functions have no caller and do not say why they are still \
         here. Give each one `#[allow(dead_code, reason = \"…\")]` naming what would \
         call it — or delete it. An unreachable `pub fn` in a library crate is \
         invisible to `dead_code`, which is how §7.10's disconnect policy went \
         unimplemented (#251):\n{silent:#?}"
    );
}

/// Whether `line` names `ident` as a whole word.
fn mentions(line: &str, ident: &str) -> bool {
    let bytes = line.as_bytes();
    let mut from = 0;
    while let Some(at) = line[from..].find(ident) {
        let start = from + at;
        let end = start + ident.len();
        let before_ok = start == 0 || !is_word(bytes[start - 1]);
        let after_ok = end == bytes.len() || !is_word(bytes[end]);
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// **Silencing `dead_code` requires an explanation somewhere a reader will find it** —
/// #252.
///
/// The attribute silences the compiler either way, so the explanation is the only thing
/// a reader gets. This codebase already writes good ones — "the #74 rule that a
/// compilation's genre loses to an album's is not wired up" names exactly what is
/// missing.
///
/// **A `reason =` inside the attribute or a doc comment on the item both count**, and
/// the first version of this test accepted only the former. That flagged `Key::new`,
/// which explains itself in the line above — *"used by tests and by callers building a
/// binding directly; the default table goes through the parser instead"* — and turning
/// that into a churn of eleven attribute edits would have been a formatting rule
/// wearing a claim-quality rule's clothes.
#[test]
fn every_silenced_dead_code_gives_a_reason() {
    let files = sources();
    assert!(files.len() > 80, "read nothing: {} files", files.len());

    let mut bare: Vec<String> = Vec::new();
    let mut found = 0;
    for (path, text) in &files {
        for (n, line) in text.lines().enumerate() {
            if !line.contains("dead_code") {
                continue;
            }
            // An attribute may span lines, so look at the whole of it.
            let window: String = text.lines().skip(n.saturating_sub(2)).take(6).collect();
            if !window.contains("allow") && !window.contains("expect") {
                continue;
            }
            found += 1;
            let lines: Vec<&str> = text.lines().collect();
            let explained = window.contains("reason")
                // **A `//` comment counts as much as a `///` one.** The first version
                // took only doc comments and flagged four sites whose plain comment
                // above says exactly what is missing — "mutation arrives with the
                // command layer in M6". The rule is that a reader gets an
                // explanation, not that it is rustdoc.
                || attached(&lines, n)
                    .iter()
                    .any(|a| a.trim_start().starts_with("//"));
            if !explained {
                bare.push(format!("{}:{}", path.display(), n + 1));
            }
        }
    }

    // **The instrument is attached**, and this one matters more than usual: if the
    // scan found no attributes at all, `bare` is empty and the test passes while
    // checking nothing.
    assert!(
        found >= 4,
        "found only {found} dead-code attributes, so this test is not looking at the \
         thing it claims to check"
    );
    assert!(
        bare.is_empty(),
        "these silence `dead_code` without saying why, which leaves a reader nothing:\n{bare:#?}"
    );
}

/// Where a deferral's owner is recorded, for `just stale-claims` to check.
///
/// **Not asserted here on purpose.** Whether `#123` is still open is a question for the
/// issue tracker, and a test that needs the network is one that gets ignored the first
/// time it fails offline. This only asserts the *form* — that a deferral names an issue
/// at all — because a deferral with no owner cannot be checked by anything, ever.
#[test]
fn a_deferral_names_the_issue_that_owns_it() {
    let files = sources();
    assert!(files.len() > 80, "read nothing: {} files", files.len());

    let mut ownerless: Vec<String> = Vec::new();
    let mut found = 0;
    for (path, text) in &files {
        for (n, line) in text.lines().enumerate() {
            // Forward-looking deferrals only. "was X until #117" is a record of
            // something finished and is correct as written — conflating the two
            // grammars is what turned three findings into a reported seven (§13,
            // 2026-09-13).
            let lower = line.to_ascii_lowercase();
            let defers = lower.contains("stays until")
                || lower.contains("is not wired up")
                || lower.contains("has no caller yet")
                || lower.contains("gets a caller");
            if !defers {
                continue;
            }
            found += 1;
            // The issue may be named on a neighbouring line of the same comment.
            let window: String = text
                .lines()
                .skip(n.saturating_sub(3))
                .take(8)
                .collect::<Vec<_>>()
                .join(" ");
            if !has_issue_reference(&window) {
                ownerless.push(format!("{}:{}  {}", path.display(), n + 1, line.trim()));
            }
        }
    }

    assert!(
        found >= 2,
        "found only {found} deferrals, so the patterns above no longer match how this \
         codebase writes one — which makes this test silent rather than satisfied"
    );
    assert!(
        ownerless.is_empty(),
        "these defer something without naming an issue that owns it, so nothing can \
         ever check whether they are still true:\n{ownerless:#?}"
    );
}

/// Whether the text carries a `#123`-style issue reference.
fn has_issue_reference(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(at) = text[from..].find('#') {
        let start = from + at + 1;
        let digits = bytes[start..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits > 0 {
            return true;
        }
        from = start;
    }
    false
}

/// Helper, so the checks above are read rather than trusted.
#[allow(
    dead_code,
    reason = "exercised by the tests below; kept beside what it proves"
)]
fn unused_example() {}

#[cfg(test)]
mod proving_the_scanner {
    use super::*;

    /// **The word matcher does not match substrings**, which is the bug that made my
    /// first audit scan report `worth_entering` as dead while it had three callers —
    /// the opposite error, from the same root.
    #[test]
    fn a_name_is_matched_as_a_whole_word() {
        assert!(mentions("    let x = rewrite(&path, &set);", "rewrite"));
        assert!(mentions("foo(bar)", "bar"));
        assert!(!mentions("    rewrite_all(&log);", "rewrite"));
        assert!(!mentions("    let prewrite = 1;", "rewrite"));
        // A bare mention counts: a function passed by reference is a use, and missing
        // that is how `filter_entry(worth_entering)` looked unreferenced.
        assert!(mentions(
            "    .filter_entry(worth_entering)",
            "worth_entering"
        ));
    }

    /// An issue reference is digits after a hash, and nothing else is.
    #[test]
    fn an_issue_reference_is_a_hash_and_digits() {
        assert!(has_issue_reference("until #251 gives it a caller"));
        assert!(!has_issue_reference("a # on its own"));
        assert!(!has_issue_reference("#[allow(dead_code)]"));
    }

    /// **The attribute walk finds an `#[allow]` on either side of the doc comment.**
    /// `instance.rs` carried three that preceded it, so a walk that stopped at the
    /// first `///` would have called them unmarked.
    #[test]
    fn an_attribute_is_found_above_a_doc_comment() {
        let text = "#[allow(dead_code, reason = \"x\")]\n/// Doc.\npub fn thing() {}\n";
        let lines: Vec<&str> = text.lines().collect();
        let found = attached(&lines, 2);
        assert!(
            found.iter().any(|l| l.contains("dead_code")),
            "the walk stopped at the doc comment: {found:?}"
        );
    }

    /// And it does not reach past a blank line into the item above.
    #[test]
    fn the_walk_stops_at_the_previous_item() {
        let text = "#[allow(dead_code)]\npub fn other() {}\n\npub fn thing() {}\n";
        let lines: Vec<&str> = text.lines().collect();
        let found = attached(&lines, 3);
        assert!(
            !found.iter().any(|l| l.contains("dead_code")),
            "it borrowed the attribute from the function above: {found:?}"
        );
    }
}
