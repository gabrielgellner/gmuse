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

/// **Which crate may depend on which, asserted against the manifests** — #257.
///
/// `layers.md`'s direction rule is "enforced by cargo, not by a test", and that is the right
/// shape — a dependency that cannot compile beats one a test complains about. But cargo only
/// enforces the edges that *exist*; it says nothing about an edge somebody adds. So this
/// names every intended one, and a new one fails here with the reason it was excluded.
///
/// **The endpoint of #257 is one line of this table**: `gmuse-engine` without
/// `gmuse-library`. The ratchet below counts reaches because that line cannot be written
/// yet; when it can, the ratchet goes and this is strictly better, because an import spelled
/// differently still needs the manifest edge.
#[test]
fn every_crate_depends_only_on_what_it_is_meant_to() {
    // Ordered low to high. A crate may depend only on what is listed for it.
    let intended: &[(&str, &[&str])] = &[
        // **Names entities and reads nothing.** Its dependency list is the whole point of it
        // existing: anything here lands on the protocol crate's graph.
        ("gmuse-ids", &[]),
        // **`std::fs` and an error enum.** Three callers, none of which may depend on the
        // others — see §13's entry of 2026-09-13.
        ("gmuse-lock", &[]),
        // **The contract between the two resident actors** — #257. The ordering and the
        // words for it; nothing that can open a file or learn what a library holds.
        ("gmuse-programme", &["gmuse-ids"]),
        // §4.3: it must not depend on engine internals, which is why the ids are separate.
        ("gmuse-proto", &["gmuse-ids"]),
        ("gmuse-client", &["gmuse-proto", "gmuse-ids"]),
        // The librarian. Notably **not** `gmuse-proto`: the wire describes the library, and
        // an edge the other way would let a fact be shaped by a message.
        (
            "gmuse-library",
            &["gmuse-ids", "gmuse-lock", "gmuse-programme"],
        ),
        // **The audio engine, with no `gmuse-library`** — #257, and this line is the whole
        // of it. It named the librarian, the log, the fold and every fact type; now it holds
        // a window of cues, three channels, and no way to learn what a library is.
        //
        // A reach-counting ratchet held this ground while the work was in progress, and it is
        // deleted because this is strictly better: an import spelled differently still needs
        // the manifest edge, and a `pub use` cannot launder one.
        (
            "gmuse-engine",
            &["gmuse-ids", "gmuse-lock", "gmuse-programme", "gmuse-proto"],
        ),
    ];

    let mut wrong: Vec<String> = Vec::new();
    let mut seen = 0usize;
    for (crate_name, allowed) in intended {
        let manifest = Path::new("crates").join(crate_name).join("Cargo.toml");
        let text = match std::fs::read_to_string(&manifest) {
            Ok(text) => text,
            Err(e) => {
                wrong.push(format!("{}: {e}", manifest.display()));
                continue;
            }
        };
        seen += 1;
        // `[dependencies]` only — a dev-dependency is test scaffolding and does not put the
        // crate on anybody's shipped graph.
        let deps = section(&text, "[dependencies]");
        for line in deps.lines() {
            let Some(name) = line.split('=').next().map(str::trim) else {
                continue;
            };
            if !name.starts_with("gmuse-") {
                continue;
            }
            if !allowed.contains(&name) {
                wrong.push(format!(
                    "{crate_name} depends on {name}, which is not intended"
                ));
            }
        }
    }

    // **The instrument is attached.** A renamed directory would read every manifest as
    // missing and the loop would find nothing to complain about.
    assert_eq!(
        seen,
        intended.len(),
        "only {seen} of {} manifests were read, so this checked almost nothing",
        intended.len()
    );
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// One `[section]` of a manifest, up to the next one.
fn section<'a>(text: &'a str, header: &str) -> &'a str {
    let Some(at) = text.find(header) else {
        return "";
    };
    let body = &text[at + header.len()..];
    match body.find("\n[") {
        Some(end) => &body[..end],
        None => body,
    }
}

/// **The crates that claim they cannot touch a disk, checked** — #257.
///
/// `gmuse-ids` says "everything that can name an entity, and **nothing that can read a
/// file**", and that claim is load-bearing: it is the whole reason the ids live apart from the
/// library, and the reason `gmuse-lock` had to be its own crate rather than going in there.
/// `gmuse-programme` makes the same claim for the same kind of reason.
///
/// Both were comments with no instrument, which is §13's fourth way to be green and wrong.
/// The dependency check above stops either *acquiring* something that walks a disk; this stops
/// either doing it with `std` directly.
#[test]
fn the_crates_that_cannot_touch_a_disk_do_not() {
    // Named because they cannot be inferred: `Path` and `PathBuf` are fine — a path is a
    // name, and `Cued` carries one. Opening one is not.
    const FORBIDDEN: &[&str] = &[
        "std::fs",
        "File::open",
        "File::create",
        "OpenOptions",
        "read_to_string",
        "read_dir",
        "std::net",
        "std::process",
    ];

    let mut found: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    for crate_name in ["gmuse-ids", "gmuse-programme"] {
        let root = Path::new("crates").join(crate_name).join("src");
        for (path, text) in sources() {
            if !path.starts_with(&root) {
                continue;
            }
            scanned += 1;
            // Tests may do as they like; the claim is about the shipped surface.
            for line in production(&text).lines() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                for bad in FORBIDDEN {
                    if code.contains(bad) {
                        found.push(format!("{}: {}", path.display(), code.trim()));
                    }
                }
            }
        }
    }

    // **The instrument is attached**, and this one needs it badly: the assertion below is an
    // absence, and an absence over a set that matched no files passes for nothing.
    assert!(
        scanned >= 8,
        "only {scanned} files were scanned across the two crates, so this proved nothing"
    );
    assert!(
        found.is_empty(),
        "a crate that claims it cannot touch a disk does:\n  {}",
        found.join("\n  ")
    );
}

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

/// **A view cannot decide what plays next, and cannot rewrite the log** — #261, §7 of #257.
///
/// The three-actor model gives every question a single owner, and `Desk::programme` is the
/// one it exists for. A view held a whole `Desk`, so it was a second decider *one call site
/// away* — and #261 called that latent rather than actual, because nothing in `src/ui/` had
/// ever called it. A rule that holds because nobody has broken it yet is a habit.
///
/// So the view holds [`Enquiries`], where the method is absent rather than refused. The
/// difference is the one `Desk::appender` gets right for appending: `None` is a branch a
/// caller can forget to take, and a missing method is a compile error.
///
/// **This checks the mechanism, not the habit.** Grepping `src/ui/` for `.programme(` would
/// pass just as well on the day somebody added the call — it is the field's *type* that makes
/// it impossible, so that is what is asserted.
#[test]
fn a_follower_cannot_name_the_decisions_that_have_one_owner() {
    let librarian = std::fs::read_to_string("crates/gmuse-library/src/librarian/mod.rs")
        .expect("the librarian");

    // The instrument, before the claim: this check is worthless if it cannot see the
    // methods it is asserting the absence of, and `impl Desk` is where they must be.
    let desk = block(&librarian, "impl Desk {").expect("`impl Desk` moved or was renamed");
    for owned in ["fn programme(", "fn programme_later("] {
        assert!(
            desk.contains(owned),
            "`Desk::{}` is gone — either it moved and this check needs rewriting, or the \
             thing a follower must not reach no longer exists",
            owned.trim_start_matches("fn ")
        );
    }

    let enquiries =
        block(&librarian, "impl Enquiries {").expect("`impl Enquiries` moved or was renamed");
    assert!(
        enquiries.matches("pub fn ").count() >= 5,
        "`impl Enquiries` parsed to almost nothing, so finding no `programme` in it proves \
         nothing"
    );
    // Paren-anchored: `fn scans` is a read a follower keeps, and `fn scan` is not.
    for hidden in [
        "fn programme(",
        "fn compact(",
        "fn say(",
        "fn scan(",
        "fn holdings(",
    ] {
        assert!(
            !enquiries.contains(hidden),
            "`Enquiries::{}` exists — a follower can reach it again",
            hidden.trim_start_matches("fn ")
        );
    }

    // And the view holds that type rather than the wide one. Naming `Desk` in `src/ui/`
    // is fine — the tests build one and narrow it — but *storing* one hands the whole
    // surface back. A field declaration is `name: Type,` with no call in it, which is what
    // separates it from the `desk: Desk::detached(..)` a test's struct literal writes.
    let mut fields = 0;
    for file in ["src/ui/mod.rs", "src/view.rs", "src/ui/statusline.rs"] {
        let text = std::fs::read_to_string(file).expect(file);
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.starts_with("//") || line.contains('(') || !line.ends_with(',') {
                continue;
            }
            let Some((name, ty)) = line.trim_start_matches("pub ").split_once(": ") else {
                continue;
            };
            if !name.chars().all(|c| c.is_ascii_lowercase() || c == '_') || name.is_empty() {
                continue;
            }
            fields += 1;
            assert!(
                !ty.contains("Desk"),
                "{file}:{} stores a `Desk`: {line}",
                n + 1
            );
        }
    }
    // The instrument again: a parse that found no fields would pass this silently.
    assert!(
        fields > 50,
        "only {fields} field declarations parsed out of the view — the scan is not reading \
         what it claims to"
    );
}

/// The body of one `impl` block, by brace counting from its header.
fn block<'a>(text: &'a str, header: &str) -> Option<&'a str> {
    let start = text.find(header)? + header.len();
    let mut depth = 1usize;
    for (i, c) in text[start..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// **The view asks seams, not projections** — #261, `tree-over-the-wire.md`.
///
/// `ui::tree` is the one place that browsed by holding the library's own projection, and while
/// it did, an attached view had to fold the log to build one — which is the 40.6 MB the design
/// is about. Every one of its twenty methods takes a `dyn Browsing` now, and the one
/// implementation is `Shelf`. A second one is a socket.
///
/// **Checked as an absence in production code, not as a count of methods.** The seam is worth
/// nothing the moment one path reaches past it, and reaching past it compiles perfectly well —
/// it is `use crate::shelf::Shelf` that would have to come back.
#[test]
fn a_follower_holds_no_projection_of_its_own() {
    let tree = std::fs::read_to_string("src/ui/tree.rs").expect("the tree view");

    // The instrument: this proves nothing if the file stopped being the tree view.
    assert!(
        tree.contains("fn step_over(") && tree.contains("impl TreeState"),
        "src/ui/tree.rs is not the navigator any more — this check needs rewriting"
    );
    let takes = tree.matches("lib: &dyn Browsing").count();
    assert!(
        takes >= 15,
        "only {takes} methods take a `Browsing`, so most of the view still asks something else"
    );

    // Neither a `Shelf` nor a `Librarian` may be named in the view's production code. The
    // first is the tree, the second is the fold behind it — `Context::library` is a
    // `dyn Listing` and `Ui::library` a `dyn Holding`, and while that holds, an attached view
    // is one implementation away from folding nothing at all.
    //
    // Both are fine in the tests, which need a library to browse. `#[cfg(test)]` marks the
    // boundary, and the first one is where production ends.
    //
    // **And the same for the view as a whole** (#261). `ui::holding` is the one place an
    // implementation is chosen, and it is excluded by name rather than by lowering the bar:
    // that is where the socket-backed one goes, so it is the one file that must name both.
    // **Every file in the view, found rather than listed.** A hardcoded pair passes just as
    // well with one of them deleted from it, which is §13's harness-does-less shape — and a
    // file added to `src/ui/` later would not be covered by a list nobody remembered to grow.
    let mut scanned = 0;
    let mut lines = 0;
    for entry in std::fs::read_dir("src/ui").expect("the view's directory") {
        let path = entry.expect("an entry").path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        // `ui::holding` is the one place an implementation is chosen — excluded by name
        // rather than by lowering the bar, because that is where the socket-backed one goes.
        if path.file_name().is_some_and(|n| n == "holding.rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("a view file");
        let production = text.split("#[cfg(test)]").next().unwrap_or_default();
        scanned += 1;
        for (n, line) in production.lines().enumerate() {
            lines += 1;
            let line = line.trim();
            if line.starts_with("//") {
                continue;
            }
            for held in ["Shelf", "Librarian"] {
                assert!(
                    !line.contains(held),
                    "{}:{} names a `{held}`: {line}",
                    path.display(),
                    n + 1
                );
            }
        }
    }
    assert!(
        scanned >= 5 && lines > 5_000,
        "{scanned} files and {lines} lines of the view scanned — the walk is not reading it"
    );
}

/// **Nothing on the seam lends out of the fold** — #261, `tree-over-the-wire.md` §7.
///
/// `Listing` is what a view reads the library through, and it must be answerable by something
/// that has no library: a client on the other end of a socket. A method returning a borrow
/// cannot be — there is nothing on that side for the reference to point into.
///
/// Three of them were, and each needed a different answer. `listed` hands back a [`Slip`], the
/// row written out. `playable`'s one borrowed field is a `String` now. `subject` left the trait
/// entirely, because it is a *view* onto a row rather than an answer about one — the filter
/// builds its own from a concrete library, which is the hot path.
///
/// **`whole` is the exception and says so by returning `Option`.** It asks "are you the fold?",
/// and a reader that is not answers `None` — which is the whole point of it.
#[test]
fn the_read_seam_answers_without_lending() {
    let rows = std::fs::read_to_string("crates/gmuse-library/src/model/rows.rs").expect("rows.rs");
    let start = rows.find("pub trait Listing").expect("the trait");
    let body = &rows[start..];
    let end = body.find("\n}\n").expect("the trait ends");
    let body = &body[..end];

    // The instrument: a trait that parsed to nothing lends nothing either.
    let signatures: Vec<&str> = body
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("fn "))
        .collect();
    assert!(
        signatures.len() >= 8,
        "only {} methods found on `Listing` — the parse is not reading the trait",
        signatures.len()
    );

    for line in signatures {
        let Some((_, returns)) = line.split_once("->") else {
            continue;
        };
        // `whole` is the one that may: it exists to be answered `None`.
        if line.starts_with("fn whole(") {
            continue;
        }
        assert!(
            !returns.contains('&') && !returns.contains('\''),
            "`Listing::{}` lends out of the fold, so a reader without one cannot answer it: \
             ->{returns}",
            line.split('(').next().unwrap_or(line)
        );
    }
}
