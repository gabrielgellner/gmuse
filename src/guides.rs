//! The guides — view 7.
//!
//! Prose, not a key list. `<leader>?` answers "what does this key do"; these
//! answer "how do I do the thing", which is a different question and the one
//! that is hard to discover by pressing keys. `dd` then `p` swapping two
//! tracks is not deducible from a table of bindings.
//!
//! Compiled in with `include_str!` rather than read at runtime. They are real
//! files — readable and editable in the repository, and diffable when the
//! behaviour they describe changes — but the binary needs no data directory
//! and cannot ship without them.

/// Where a guide's text comes from.
pub enum Body {
    /// A file in `docs/guides/`, compiled in.
    Text(&'static str),
    /// Rendered from the binding table at read time.
    ///
    /// The listing that view 7 used to be (#47). Generated rather than written
    /// because it is the one guide that is a *table of what is true now*
    /// rather than an explanation: a `:bind` at runtime shows up in it, and
    /// there is no second copy of the keymap to fall out of step. vim keeps
    /// the same thing under `:help index`.
    Keymap,
}

/// One guide: its title, and where its text comes from.
pub struct Guide {
    pub title: &'static str,
    pub body: Body,
}

/// In the order someone meets them, not alphabetical. Moving around comes
/// first because it is what you need before anything else is reachable.
pub const GUIDES: &[Guide] = &[
    Guide {
        title: "Moving around",
        body: Body::Text(include_str!("../docs/guides/moving-around.md")),
    },
    Guide {
        title: "The library tree",
        body: Body::Text(include_str!("../docs/guides/the-library-tree.md")),
    },
    Guide {
        title: "Building playlists",
        body: Body::Text(include_str!("../docs/guides/building-playlists.md")),
    },
    Guide {
        title: "Finding things",
        body: Body::Text(include_str!("../docs/guides/finding-things.md")),
    },
    Guide {
        title: "Rules that make lists",
        body: Body::Text(include_str!("../docs/guides/saved-rules.md")),
    },
    Guide {
        title: "What the library says",
        body: Body::Text(include_str!("../docs/guides/what-the-library-says.md")),
    },
    Guide {
        title: "Writing the lines",
        body: Body::Text(include_str!("../docs/guides/format-strings.md")),
    },
    // Last, because it is the one you come to already knowing what you want —
    // the others are read in order the first time and this one is consulted.
    Guide {
        title: "Index",
        body: Body::Keymap,
    },
];

/// A guide's text: the file for a written one, the live keymap for the index.
///
/// `Cow` rather than `String`, so the written guides stay the `&'static str`
/// they have always been and nothing is copied to read one.
pub fn body(
    guide: &Guide,
    bindings: &crate::cmd::bind::Bindings,
) -> std::borrow::Cow<'static, str> {
    match guide.body {
        Body::Text(text) => std::borrow::Cow::Borrowed(text),
        Body::Keymap => std::borrow::Cow::Owned(keymap(bindings)),
    }
}

/// Every binding, as a guide body — in the shape the written guides use, so it
/// renders through exactly the same code: a heading per context, and an
/// indented `key  command` table under each.
///
/// `bind::sorted` is the same ordering view 7 showed, and the format is
/// `parse::format`, which round-trips — so what is listed is a line you could
/// type back.
fn keymap(bindings: &crate::cmd::bind::Bindings) -> String {
    use std::fmt::Write as _;
    let mut out = String::from(
        "# Index\n\nEvery key that is bound, read off the binding table — so a `:bind` appears here at once.\n\n         Keys under `common` work in every view. A view's own binding hides the common one.\n",
    );
    let mut context = None;
    for (ctx, key, cmd) in crate::cmd::bind::sorted(bindings) {
        if context != Some(ctx) {
            let _ = write!(out, "\n## {}\n\n", ctx.name());
            context = Some(ctx);
        }
        // Two spaces between the columns, because that is what `kind` reads as
        // an example row and what the guide audit splits on.
        let _ = writeln!(out, "    {key}  {}", crate::cmd::parse::format(cmd));
    }
    out
}

/// The body as display lines, with the leading `# Title` dropped — the pane
/// is already titled, and repeating it wastes the first row.
pub fn lines(body: &str) -> Vec<&str> {
    body.lines()
        .skip_while(|l| l.starts_with("# ") || l.trim().is_empty())
        .collect()
}

/// The guide as display lines, wrapped to `width`.
///
/// Prose wraps; **examples and headings do not.** An example is a key table or
/// a snippet of config whose alignment carries meaning, and reflowing it turns
/// a diagram into rubble. Prettier reflows the source to 120 columns, which no
/// pane is, so wrapping has to happen here rather than in the file.
///
/// The heading marker is dropped: `##` is markdown's way of saying "heading",
/// and on screen the style says it instead.
pub fn wrapped(body: &str, width: usize) -> Vec<(Kind, String)> {
    let width = width.max(20);
    let mut out = Vec::new();
    // Consecutive prose lines are one paragraph, joined before wrapping.
    // Wrapping each source line on its own leaves a short line wherever the
    // file happened to break — and the file is broken at 120 columns by
    // prettier, which is nothing to do with the pane.
    let mut paragraph = String::new();

    let flush = |paragraph: &mut String, out: &mut Vec<(Kind, String)>| {
        if paragraph.is_empty() {
            return;
        }
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if !current.is_empty() && current.chars().count() + 1 + word.chars().count() > width {
                out.push((Kind::Prose, std::mem::take(&mut current)));
            }
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
        out.push((Kind::Prose, current));
        paragraph.clear();
    };

    for line in lines(body) {
        match kind(line) {
            Kind::Prose if !line.trim().is_empty() => {
                if !paragraph.is_empty() {
                    paragraph.push(' ');
                }
                paragraph.push_str(line.trim());
            }
            other => {
                flush(&mut paragraph, &mut out);
                match other {
                    Kind::Heading => {
                        out.push((Kind::Heading, line.trim_start_matches('#').trim().into()))
                    }
                    Kind::Example => out.push((Kind::Example, line.to_string())),
                    Kind::Prose => out.push((Kind::Prose, String::new())),
                }
            }
        }
    }
    flush(&mut paragraph, &mut out);
    out
}

/// How a line should be drawn. Deliberately not a markdown parser: these are
/// three visual kinds, and a dependency to tell them apart would be absurd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `## something`
    Heading,
    /// Indented — an example, a key table, a snippet of config.
    Example,
    Prose,
}

pub fn kind(line: &str) -> Kind {
    if line.starts_with('#') {
        Kind::Heading
    } else if line.starts_with("    ") {
        Kind::Example
    } else {
        Kind::Prose
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every guide is compiled in, so a missing file is a build error rather
    /// than an empty pane. This checks they are not *empty*, which the
    /// compiler cannot.
    #[test]
    fn every_guide_has_a_title_and_a_body() {
        assert!(!GUIDES.is_empty());
        let bindings = crate::cmd::bind::defaults();
        for g in GUIDES {
            assert!(!g.title.is_empty());
            let text = body(g, &bindings);
            assert!(
                lines(&text).len() > 5,
                "{} is too short to be a guide",
                g.title
            );
        }
    }

    /// The pane is titled already; repeating the heading wastes the first row.
    #[test]
    fn the_leading_heading_is_dropped() {
        let bindings = crate::cmd::bind::defaults();
        for g in GUIDES {
            let text = body(g, &bindings);
            let first = lines(&text)[0];
            assert!(!first.starts_with("# "), "{}: {first:?}", g.title);
            assert!(!first.trim().is_empty(), "{} starts blank", g.title);
        }
    }

    /// A paragraph the file broke at 120 columns must not keep that break —
    /// otherwise a short line appears wherever prettier happened to wrap.
    #[test]
    fn prose_is_rewrapped_as_paragraphs() {
        let out = wrapped(
            "# t\n\nthe quick brown fox jumped over\nthe lazy dog and kept going\n\n    an example\n",
            30,
        );
        let prose: Vec<&str> = out
            .iter()
            .filter(|(k, l)| *k == Kind::Prose && !l.is_empty())
            .map(|(_, l)| l.as_str())
            .collect();

        // Rejoined, then rewrapped at 30 — not split where the source was.
        assert!(prose.iter().all(|l| l.chars().count() <= 30), "{prose:?}");
        assert_eq!(
            prose.join(" "),
            "the quick brown fox jumped over the lazy dog and kept going"
        );
        // Only the last line of a paragraph may be short.
        for line in &prose[..prose.len() - 1] {
            assert!(
                line.chars().count() > 15,
                "a short line mid-paragraph: {prose:?}"
            );
        }
        // The example keeps its own shape.
        assert!(
            out.iter()
                .any(|(k, l)| *k == Kind::Example && l == "    an example")
        );
    }

    /// The pane is styled; `##` on screen is markdown leaking through.
    #[test]
    fn heading_markers_are_dropped() {
        let bindings = crate::cmd::bind::defaults();
        for g in GUIDES {
            for (kind, line) in wrapped(&body(g, &bindings), 70) {
                if kind == Kind::Heading {
                    assert!(!line.starts_with('#'), "{}: {line:?}", g.title);
                }
            }
        }
    }

    #[test]
    fn lines_are_classified_by_shape() {
        assert_eq!(kind("## Undo"), Kind::Heading);
        assert_eq!(kind("    yy      copy"), Kind::Example);
        assert_eq!(kind("The register is global."), Kind::Prose);
        assert_eq!(kind(""), Kind::Prose);
    }

    /// Prose that mentions a key should mention one that exists. Not every
    /// binding, but the ones the guides lean on hardest.
    #[test]
    fn the_keys_the_guides_teach_are_really_bound() {
        let bindings = crate::cmd::bind::defaults();
        let bound: Vec<String> = crate::cmd::bind::sorted(&bindings)
            .into_iter()
            .map(|(_, key, _)| key)
            .collect();
        for key in [
            "y y",
            "d d",
            "p",
            "P",
            "u",
            "^r",
            "l",
            "h",
            "o",
            "a",
            "e",
            "z r",
            "z m",
            "z R",
            "z M",
            "z C",
            "<leader> x",
        ] {
            assert!(
                bound.iter().any(|b| b == key),
                "the guides teach {key:?}, which is not bound"
            );
        }
    }

    /// The named keys, whose capitalisation is only ever typography: prose
    /// writes `Enter` and `g<Tab>` where the binding table says `enter` and
    /// `g tab`.
    const NAMES: &[&str] = &[
        "leader", "enter", "tab", "space", "home", "end", "pageup", "pagedown", "up", "down",
        "left", "right", "esc",
    ];

    /// A key as the guides would write it: no angle brackets, no spaces, and
    /// named keys lower-cased.
    ///
    /// **Letters keep their case**, because here it is meaning rather than
    /// typography — `r` and `R` are different bindings, as are `zc` and `zC`.
    /// Folding everything let a sentence about `c` stand in for the bound `C`,
    /// which is how `C` passed this test before it deserved to.
    fn normalised(s: &str) -> String {
        let mut out = s.to_string();
        for name in NAMES {
            let mut at = 0;
            while let Some(i) = out[at..].to_lowercase().find(name) {
                let (from, to) = (at + i, at + i + name.len());
                out.replace_range(from..to, name);
                at = to;
            }
        }
        out.chars()
            .filter(|c| !matches!(c, '<' | '>' | ' ' | '`'))
            .collect()
    }

    /// Every key the guides mention, taken from backticked spans and from the
    /// indented example blocks — the two places a key is ever written.
    ///
    /// Extracted rather than grepped for, so that a single-letter binding like
    /// `o` has to appear *as a key* and cannot be satisfied by the letter o
    /// turning up inside a word.
    /// **Written** guides only. The index is generated from the binding table,
    /// so it contains every key by construction — counting it would make the
    /// audit below pass for every binding that has ever existed, and the prose
    /// guides would silently stop being checked at all. A green test that
    /// checks nothing is worse than no test, and this is exactly the shape
    /// that produces one.
    fn keys_the_guides_write() -> std::collections::HashSet<String> {
        let mut out = std::collections::HashSet::new();
        for guide in GUIDES {
            let Body::Text(body) = guide.body else {
                continue;
            };
            for span in body.split('`').skip(1).step_by(2) {
                out.insert(normalised(span));
            }
            // Only the **key column** of an example row: everything up to the
            // first run of two spaces. Taking every token would let the words
            // of a description stand in for a key, and single-letter bindings
            // like `r` and `m` would pass on prose alone — which is exactly
            // what happened when this was first written.
            for line in body.lines().filter(|l| l.starts_with("    ")) {
                let column = line.trim_start().split("  ").next().unwrap_or_default();
                if !column.is_empty() {
                    out.insert(normalised(column));
                }
            }
        }
        out
    }

    /// Bindings deliberately left untaught, and why.
    ///
    /// An alias for a key that *is* taught needs no prose of its own —
    /// documenting both would only suggest they differ.
    const UNTAUGHT: &[(&str, &str)] = &[("<leader> =", "an alias for `<leader> +`")];

    /// The other direction, and the one that actually rots: a key can be added
    /// or moved without anybody remembering the guides exist.
    ///
    /// It caught a real hole the first time it ran — the whole transport row
    /// had moved behind `<leader>` without a word written about it anywhere,
    /// and `moving-around` still said there were seven views.
    fn untaught(bindings: &crate::cmd::bind::Bindings) -> Vec<String> {
        let written = keys_the_guides_write();
        crate::cmd::bind::sorted(bindings)
            .into_iter()
            .map(|(_, key, _)| key)
            .filter(|key| !written.contains(&normalised(key)))
            .filter(|key| !UNTAUGHT.iter().any(|(k, _)| k == key))
            .collect()
    }

    /// The guard that keeps the audit meaningful. The index lists every
    /// binding, so if `keys_the_guides_write` read it the audit would pass
    /// unconditionally — including for a key no prose mentions anywhere.
    ///
    /// Checked by asking for a key that is bound and deliberately *not*
    /// written about: it must still be reported. If this ever fails, the audit
    /// above has become decorative.
    #[test]
    fn the_generated_index_does_not_count_as_teaching() {
        let mut bindings = crate::cmd::bind::defaults();
        let seq = crate::cmd::keys::parse_seq("g z q").expect("test keys");
        bindings.insert(
            (crate::cmd::Context::Common, seq),
            crate::cmd::Command::Quit,
        );

        let listed = keymap(&bindings);
        assert!(
            listed.contains("g z q"),
            "the index should list every binding"
        );
        assert_eq!(
            untaught(&bindings),
            vec!["g z q".to_string()],
            "a key only the generated index mentions must still count as untaught"
        );
    }

    #[test]
    fn every_binding_is_taught_by_some_guide() {
        let found = untaught(&crate::cmd::bind::defaults());
        assert!(
            found.is_empty(),
            "bound but taught nowhere: {found:?}\n\
             Add it to a guide, or to `UNTAUGHT` with the reason."
        );
    }

    /// The test above can only fail when something is undocumented, so with
    /// everything documented it would pass even if the audit looked at
    /// nothing. This is the half that keeps it honest.
    #[test]
    fn the_audit_notices_an_undocumented_key() {
        let mut bindings = crate::cmd::bind::defaults();
        bindings.insert(
            (
                crate::cmd::Context::Common,
                crate::cmd::keys::parse_seq("z 9").expect("parses"),
            ),
            crate::cmd::Command::Quit,
        );
        assert_eq!(
            untaught(&bindings),
            vec!["z 9".to_string()],
            "a binding no guide mentions should have been reported"
        );
    }

    /// And the exemptions have to stay real: a key that stops being bound
    /// should not sit in the list forever pretending to be documented.
    #[test]
    fn every_exemption_is_still_a_binding() {
        let bindings = crate::cmd::bind::defaults();
        let bound: Vec<String> = crate::cmd::bind::sorted(&bindings)
            .into_iter()
            .map(|(_, key, _)| key)
            .collect();
        for (key, why) in UNTAUGHT {
            assert!(
                bound.iter().any(|b| b == key),
                "{key:?} is exempt as {why}, but is not bound at all"
            );
        }
    }
}
