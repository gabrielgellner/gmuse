//! A fuzzy picker over what the library holds — issue #54, extended by #119.
//!
//! Telescope's shape and snacks': a floating input that narrows as you type,
//! ranked, with `Enter` going to the result.
//!
//! ## One input, two languages
//!
//! §7 is explicit that the filter language has **no bare words** — `ferrell`
//! alone is an error — and a picker inverts that, because bare text *is* the
//! interface. Both live in one line:
//!
//! ```text
//! genre="*electron*" burial
//! └──── predicate ───┘ └───┘ fuzzy
//! ```
//!
//! Anything that parses as a predicate narrows the candidates — the existing
//! language, so `field?`, `&`, `|`, `!` and parentheses come free — and what
//! is left is the needle, ranked by nucleo over the **rendered row**.
//!
//! **And the picker says which half is which**, in its title. That is what
//! disarms the objection to guessing: this project's habit is that an inferred
//! state is fine when it is *visible* — the pending-key indicator, the mode
//! line, which-key. A silent guess is what §7 refuses; a displayed one is what
//! the status line already does.
//!
//! ## It does not replace `/`
//!
//! `/` is vim's in-list search: it moves the **cursor** to the next match in
//! the list you are looking at. "Find anything and go there" is a different
//! act, and vim has both. `/` is untouched.
//!
//! ## Two sources, because there are two kinds of thing to find
//!
//! Tracks, and the **groups** you have made — collections and saved views
//! (#119). They stay separate rather than being merged into one ranked list:
//! "is this a list somebody made, or a rule that recomputes" is the thing you
//! most need to know before choosing one, and merging would hide it.

use crate::model::rows::CopyId;

/// What the picker is searching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Every row the library holds.
    Tracks,
    /// Collections and saved views.
    Groups,
    /// **A set already found**, named by what kind of thing it holds — #130.
    ///
    /// The other two go and get their candidates; this one is handed them.
    /// `:info artist="Bodzin"` matches four things and will not choose
    /// between them, and choosing between things is what a picker is.
    Among(&'static str),
}

/// What one row of the picker is, once chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chosen {
    Track(CopyId),
    /// `in="<name>"` — a collection.
    Collection(String),
    /// `saved="<name>"` — a definition.
    Saved(String),
    /// **Look at this thing** — #130, and the info panel opens on it.
    ///
    /// A variant rather than making `Chosen` and `Focus` one type (#129):
    /// `Saved` is a definition to *apply* and has no page, so a merge would
    /// force `Focus` to grow a variant nothing could describe. The picker
    /// learns to hand back a focus without becoming one.
    Look(crate::model::focus::Focus),
}

/// One candidate: what it looks like, what it is, and what to say about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What is matched against and drawn. **The rendered row, not the raw
    /// fields** — what you see is what you match, the same rule the completion
    /// menu follows.
    pub label: String,
    /// The right-hand column: a group's kind and size, or nothing for a track.
    pub detail: String,
    pub chosen: Chosen,
}

/// The picker's state while it is open.
#[derive(Debug, Clone)]
pub struct Picker {
    pub source: Source,
    /// What has been typed, both halves together.
    pub input: String,
    /// Which of the visible rows is under the cursor.
    pub selected: usize,
    /// Candidates, ranked, best first.
    pub shown: Vec<Candidate>,
    /// The predicate half, if the input holds one that parses.
    pub predicate: Option<String>,
    /// The fuzzy half.
    pub needle: String,
    /// A predicate that was typed and does not parse, so the title can say so
    /// rather than the list silently ignoring it.
    pub bad_predicate: Option<String>,
    /// The candidates, when they were handed over rather than gone and got.
    ///
    /// **Ranked, never re-sourced.** A `Source::Among` picker is a choice
    /// between things somebody already found, and going back to the library
    /// for more would answer a different question than the one asked.
    pub offered: Vec<Candidate>,
}

impl Picker {
    pub fn new(source: Source) -> Self {
        Self {
            source,
            input: String::new(),
            selected: 0,
            shown: Vec::new(),
            predicate: None,
            needle: String::new(),
            bad_predicate: None,
            offered: Vec::new(),
        }
    }

    /// A picker over a set already found — #130.
    pub fn among(what: &'static str, candidates: Vec<Candidate>) -> Self {
        Self {
            offered: candidates,
            ..Self::new(Source::Among(what))
        }
    }

    /// Whether the whole input is a needle.
    ///
    /// **A handed-over set has no predicate half.** `genre="x"` narrows rows;
    /// these are not rows, and reading part of the input as a filter would
    /// silently drop it.
    pub fn all_needle(&self) -> bool {
        matches!(self.source, Source::Among(_))
    }

    /// What the title says it is doing.
    ///
    /// **The whole licence for splitting the input implicitly.** An inferred
    /// state that is shown is not a guess the user has to reverse-engineer.
    pub fn title(&self) -> String {
        let what = match self.source {
            Source::Tracks => "tracks",
            Source::Groups => "groups",
            Source::Among(kind) => kind,
        };
        let mut parts = vec![what.to_string()];
        if let Some(bad) = &self.bad_predicate {
            parts.push(format!("not a filter: {bad}"));
        } else if let Some(p) = &self.predicate {
            parts.push(format!("filter: {p}"));
        }
        if !self.needle.is_empty() {
            parts.push(format!("fuzzy: {}", self.needle));
        }
        parts.join(" · ")
    }

    pub fn under_cursor(&self) -> Option<&Candidate> {
        self.shown.get(self.selected)
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.shown.is_empty() {
            self.selected = 0;
            return;
        }
        let last = self.shown.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }
}

/// Split the input into the predicate half and the needle.
///
/// **Longest parsing prefix wins.** Predicates are written left to right and
/// the needle is what trails them, so the split is found by trying the whole
/// input as a filter and giving words back to the needle until what remains
/// parses. A single pass from the left would stop at the first word that
/// happens to parse and leave the rest of an expression in the needle.
///
/// Returns `(predicate, needle, unparseable)`. The third is a predicate-shaped
/// prefix that does not parse — `genre=` with nothing after it — which the
/// title reports rather than the list silently dropping.
pub fn split(input: &str) -> (Option<String>, String, Option<String>) {
    let input = input.trim();
    if input.is_empty() {
        return (None, String::new(), None);
    }
    let words: Vec<&str> = input.split_whitespace().collect();
    for take in (0..=words.len()).rev() {
        let head = words[..take].join(" ");
        if head.is_empty() {
            break;
        }
        if crate::filter::Filter::parse(&head).is_ok() {
            return (Some(head), words[take..].join(" "), None);
        }
    }
    // Nothing parses. If it *looks* like a filter — a bare word cannot be one,
    // and `=` is what makes it look like one — say so rather than ranking the
    // whole thing as a needle and quietly matching nothing.
    let looks_like_a_filter = words[0].contains('=') || words[0].ends_with('?');
    if looks_like_a_filter {
        (None, String::new(), Some(input.to_string()))
    } else {
        (None, input.to_string(), None)
    }
}

/// Rank candidates by the needle, best first.
///
/// Ranked over `label`, which is the rendered row. Candidates the needle does
/// not match at all are dropped; an empty needle keeps everything in the order
/// it arrived, so a picker opened with no input is a plain list.
pub fn rank(mut candidates: Vec<Candidate>, needle: &str) -> Vec<Candidate> {
    if needle.trim().is_empty() {
        return candidates;
    }
    use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
    use nucleo_matcher::{Config, Matcher};

    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(needle.trim(), CaseMatching::Ignore, Normalization::Smart);

    let mut scored: Vec<(u32, usize, Candidate)> = candidates
        .drain(..)
        .enumerate()
        .filter_map(|(at, c)| {
            let mut buf = Vec::new();
            let haystack = nucleo_matcher::Utf32Str::new(&c.label, &mut buf);
            pattern.score(haystack, &mut matcher).map(|s| (s, at, c))
        })
        .collect();
    // Best first, and **the original order breaks a tie** — so equally-good
    // matches stay in library order rather than shuffling between keystrokes.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.into_iter().map(|(_, _, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(label: &str) -> Candidate {
        Candidate {
            label: label.to_string(),
            detail: String::new(),
            chosen: Chosen::Track(CopyId::from_uuid(uuid::Uuid::from_u128(1))),
        }
    }

    /// **Bare text is a needle, and a predicate is a predicate** — the split
    /// #54 called the design question.
    #[test]
    fn the_input_splits_into_a_filter_and_a_needle() {
        let (p, n, bad) = split(r#"genre="*electron*" burial"#);
        assert_eq!(p.as_deref(), Some(r#"genre="*electron*""#));
        assert_eq!(n, "burial");
        assert!(bad.is_none());
    }

    /// A bare word alone is all needle — which §7 refuses on the `:` line and
    /// this surface exists to allow.
    #[test]
    fn a_bare_word_is_the_needle() {
        let (p, n, _) = split("burial");
        assert!(p.is_none());
        assert_eq!(n, "burial");
    }

    /// And a predicate alone is all filter.
    #[test]
    fn a_predicate_alone_leaves_no_needle() {
        let (p, n, _) = split("year>=2015");
        assert_eq!(p.as_deref(), Some("year>=2015"));
        assert!(n.is_empty());
    }

    /// **The longest parsing prefix wins**, so a multi-word expression is not
    /// cut in half by the first word that happens to parse on its own.
    #[test]
    fn a_multi_word_expression_stays_whole() {
        let (p, n, _) = split(r#"year>=2015 & genre="techno" burial"#);
        assert_eq!(p.as_deref(), Some(r#"year>=2015 & genre="techno""#));
        assert_eq!(n, "burial");
    }

    /// A half-typed filter is reported rather than ranked as text.
    ///
    /// Typing `genre=` mid-thought would otherwise become a fuzzy needle
    /// matching nothing, which looks exactly like a filter that matched
    /// nothing — the confusion this project keeps paying for.
    #[test]
    fn a_filter_that_does_not_parse_is_named_not_ranked() {
        let (p, n, bad) = split("genre=");
        assert!(p.is_none());
        assert!(n.is_empty());
        assert_eq!(bad.as_deref(), Some("genre="));
    }

    /// The title says which half is which — the whole licence for splitting
    /// the input without a separator.
    #[test]
    fn the_title_says_what_it_is_doing() {
        let mut p = Picker::new(Source::Tracks);
        p.predicate = Some(r#"genre="*electron*""#.into());
        p.needle = "burial".into();
        assert_eq!(
            p.title(),
            r#"tracks · filter: genre="*electron*" · fuzzy: burial"#
        );

        let mut p = Picker::new(Source::Groups);
        p.bad_predicate = Some("genre=".into());
        assert_eq!(p.title(), "groups · not a filter: genre=");
    }

    #[test]
    fn ranking_puts_the_better_match_first() {
        let out = rank(
            vec![
                track("Sierra Ferrell — Long Time Coming"),
                track("Burial — Untrue"),
            ],
            "burial",
        );
        assert_eq!(out.len(), 1, "a non-match should be dropped");
        assert_eq!(out[0].label, "Burial — Untrue");
    }

    /// **An empty needle is a plain list**, in the order it arrived — a picker
    /// opened with no input shows the library rather than nothing.
    #[test]
    fn no_needle_keeps_everything_in_order() {
        let all = vec![track("one"), track("two")];
        let out = rank(all.clone(), "  ");
        assert_eq!(out, all);
    }

    /// Equally-good matches keep library order, so the list does not shuffle
    /// under the cursor between keystrokes.
    #[test]
    fn a_tie_is_broken_by_the_order_they_arrived_in() {
        let out = rank(vec![track("aaa x"), track("aaa y")], "aaa");
        assert_eq!(out[0].label, "aaa x");
    }

    #[test]
    fn the_cursor_stays_on_a_row_that_exists() {
        let mut p = Picker::new(Source::Tracks);
        p.move_by(1);
        assert_eq!(p.selected, 0, "an empty list has nowhere to move");

        p.shown = vec![track("a"), track("b")];
        p.move_by(5);
        assert_eq!(p.selected, 1);
        p.move_by(-5);
        assert_eq!(p.selected, 0);
    }
}
