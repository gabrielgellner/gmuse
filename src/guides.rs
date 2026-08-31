//! The guides — view 8.
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

/// One guide: its title, and its text.
pub struct Guide {
    pub title: &'static str,
    pub body: &'static str,
}

/// In the order someone meets them, not alphabetical. Moving around comes
/// first because it is what you need before anything else is reachable.
pub const GUIDES: &[Guide] = &[
    Guide {
        title: "Moving around",
        body: include_str!("../docs/guides/moving-around.md"),
    },
    Guide {
        title: "The library tree",
        body: include_str!("../docs/guides/the-library-tree.md"),
    },
    Guide {
        title: "Building playlists",
        body: include_str!("../docs/guides/building-playlists.md"),
    },
    Guide {
        title: "Finding things",
        body: include_str!("../docs/guides/finding-things.md"),
    },
    Guide {
        title: "Rules that make lists",
        body: include_str!("../docs/guides/saved-rules.md"),
    },
    Guide {
        title: "Writing the lines",
        body: include_str!("../docs/guides/format-strings.md"),
    },
];

/// The body as display lines, with the leading `# Title` dropped — the pane
/// is already titled, and repeating it wastes the first row.
pub fn lines(guide: &Guide) -> Vec<&'static str> {
    guide
        .body
        .lines()
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
pub fn wrapped(guide: &Guide, width: usize) -> Vec<(Kind, String)> {
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

    for line in lines(guide) {
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
        for g in GUIDES {
            assert!(!g.title.is_empty());
            assert!(lines(g).len() > 5, "{} is too short to be a guide", g.title);
        }
    }

    /// The pane is titled already; repeating the heading wastes the first row.
    #[test]
    fn the_leading_heading_is_dropped() {
        for g in GUIDES {
            let first = lines(g)[0];
            assert!(!first.starts_with("# "), "{}: {first:?}", g.title);
            assert!(!first.trim().is_empty(), "{} starts blank", g.title);
        }
    }

    /// A paragraph the file broke at 120 columns must not keep that break —
    /// otherwise a short line appears wherever prettier happened to wrap.
    #[test]
    fn prose_is_rewrapped_as_paragraphs() {
        let g = Guide {
            title: "t",
            body: "# t\n\nthe quick brown fox jumped over\nthe lazy dog and kept going\n\n    an example\n",
        };
        let out = wrapped(&g, 30);
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
        for g in GUIDES {
            for (kind, line) in wrapped(g, 70) {
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
}
