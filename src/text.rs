//! Folding text so that what you type finds what is tagged.
//!
//! Two problems with one answer. Metadata carries marks a keyboard may not
//! have — `Christian Löffler` — and this library's own filenames carry
//! fullwidth stand-ins for the characters Windows forbids, so a track tagged
//! `Air & Lack Thereof / Sparing the Horses` is `… ⧸ …` on disk. Neither is
//! something anyone should have to type.
//!
//! **Two forms, because a mark can be typed around two ways.** `Löffler` is
//! `loffler` to someone dropping the umlaut and `loeffler` to someone spelling
//! it out, and German does the second. Searching tries both, so either finds
//! him. Most characters have only one sensible answer and the two forms agree,
//! which is why [`expanded`] returns `None` far more often than not.
//!
//! Hand-rolled rather than `unicode-normalization`: NFD would decompose the
//! accents generically, but it still misses `ø æ ß ł đ œ`, which do not
//! decompose at all, and every one of the punctuation substitutions — which is
//! the half this library actually needs. Measured before choosing: the whole
//! tag cache contains `ö ü í ´ ’ ＂ ⧸ ？` and nothing else. Latin-1 and Latin
//! Extended-A are already far past what is there.

use std::borrow::Cow;

/// Marks taken off and typographic punctuation flattened, case preserved.
///
/// Borrowed when nothing changed, which is nearly always — the point is that
/// an all-ASCII library pays nothing for this.
pub fn flatten(s: &str) -> Cow<'_, str> {
    rewrite(s, false)
}

/// The same, but with marks **spelled out** the way someone without those keys
/// spells them: `Löffler` → `Loeffler`, `Straße` → `Strasse`.
///
/// `None` when it would equal [`flatten`], so a caller can skip the second
/// search entirely.
pub fn expanded(s: &str) -> Option<String> {
    let spelled = rewrite(s, true);
    let flat = rewrite(s, false);
    (spelled != flat).then(|| spelled.into_owned())
}

/// What a search compares against: flattened and lower-cased.
pub fn needle(s: &str) -> String {
    flatten(s).to_lowercase()
}

/// A search pattern, folded once, carrying its own case rule.
///
/// **Smart-case, as §7's filter language already has it and as vim and ripgrep
/// do**: insensitive unless the pattern contains a capital. `/burial` finds
/// `Burial`; `/Burial` is precise. `/` was unconditionally insensitive before
/// this, which meant the two search surfaces in one program disagreed about
/// what a capital meant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Needle {
    text: String,
    sensitive: bool,
}

impl Needle {
    pub fn new(pattern: &str) -> Self {
        let folded = flatten(pattern);
        let sensitive = folded.chars().any(char::is_uppercase);
        Self {
            text: if sensitive {
                folded.into_owned()
            } else {
                folded.to_lowercase()
            },
            sensitive,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// Where `needle` matches in `s`, as **char** indices into `s` itself.
///
/// Folding does not preserve length — `ö` is one char and folds to one, but
/// `ß` folds to `ss` and `＂` to `"` — so an offset into the folded string
/// means nothing in the original. This walks both at once and reports the
/// range in the *source*, which is what a highlight has to paint.
///
/// Called per **visible** row rather than stored per row: highlighting is a
/// rendering concern and a screenful is thirty rows, where the search itself
/// walks thousands.
pub fn match_range(s: &str, needle: &Needle) -> Option<std::ops::Range<usize>> {
    if needle.is_empty() {
        return None;
    }
    for spell in [false, true] {
        // One pass building the folded form and, for each folded char, the
        // char index in `s` it came from.
        let mut folded = String::with_capacity(s.len());
        let mut origin: Vec<usize> = Vec::with_capacity(s.len());
        for (i, c) in s.chars().enumerate() {
            let piece: String = match folding(c) {
                Some((flat, spelled)) => (if spell { spelled } else { flat }).to_string(),
                None => c.to_string(),
            };
            for folded_char in piece.chars() {
                let folded_char = if needle.sensitive {
                    folded_char
                } else {
                    folded_char.to_lowercase().next().unwrap_or(folded_char)
                };
                folded.push(folded_char);
                origin.push(i);
            }
        }
        if let Some(byte) = folded.find(&needle.text) {
            let start = folded[..byte].chars().count();
            let end = start + needle.text.chars().count();
            // `end` indexes one past the last folded char, so the source range
            // ends one past *its* origin — not at `origin[end]`, which would
            // be the next character along.
            let last = origin.get(end.saturating_sub(1)).copied()?;
            return Some(origin[start]..last + 1);
        }
        if expanded(s).is_none() {
            break; // the spelled pass would repeat the flat one
        }
    }
    None
}

/// One row folded once, so a search that walks the whole view does the work
/// per row rather than per row per keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Haystack {
    /// Folded, case preserved — what a case-*sensitive* pattern compares to.
    flat: String,
    /// The same lower-cased. Both are kept because smart-case decides which is
    /// wanted per pattern, and lowering at compare time would allocate on every
    /// row of every keystroke.
    flat_lower: String,
    spelled: Option<String>,
    spelled_lower: Option<String>,
}

impl Haystack {
    pub fn of(s: &str) -> Self {
        let flat = flatten(s).into_owned();
        let spelled = expanded(s);
        Self {
            flat_lower: flat.to_lowercase(),
            flat,
            spelled_lower: spelled.as_deref().map(str::to_lowercase),
            spelled,
        }
    }

    pub fn contains(&self, needle: &Needle) -> bool {
        let (flat, spelled) = if needle.sensitive {
            (&self.flat, self.spelled.as_deref())
        } else {
            (&self.flat_lower, self.spelled_lower.as_deref())
        };
        flat.contains(&needle.text) || spelled.is_some_and(|s| s.contains(&needle.text))
    }
}

fn rewrite(s: &str, spell: bool) -> Cow<'_, str> {
    if !s.chars().any(|c| folding(c).is_some()) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match folding(c) {
            Some((flat, spelled)) => out.push_str(if spell { spelled } else { flat }),
            None => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// What one character folds to: the mark taken off, then spelled out.
///
/// Upper case is handled by folding the lower-case form and putting the
/// capital back, so the table stays half the size and cannot disagree with
/// itself about `Ö` versus `ö`.
fn folding(c: char) -> Option<(&'static str, &'static str)> {
    if let Some(pair) = lower_folding(c) {
        return Some(pair);
    }
    // Only worth the lookup if lowering actually changes the character, and
    // only for the single-character case — `İ` lowers to two and is not Latin
    // enough to matter here.
    let mut lowered = c.to_lowercase();
    let lower = lowered.next()?;
    if lowered.next().is_some() || lower == c {
        return None;
    }
    let (flat, spelled) = lower_folding(lower)?;
    Some((capitalised(flat), capitalised(spelled)))
}

/// The capitalised form of a replacement, as a static string.
///
/// A `match` rather than `to_uppercase`, because the result has to be
/// `&'static str` — and the set of replacements is small and closed.
fn capitalised(s: &str) -> &'static str {
    match s {
        "a" => "A",
        "aa" => "Aa",
        "ae" => "Ae",
        "c" => "C",
        "d" => "D",
        "dh" => "Dh",
        "e" => "E",
        "g" => "G",
        "h" => "H",
        "i" => "I",
        "ij" => "Ij",
        "j" => "J",
        "k" => "K",
        "l" => "L",
        "n" => "N",
        "ng" => "Ng",
        "o" => "O",
        "oe" => "Oe",
        "r" => "R",
        "s" => "S",
        "ss" => "Ss",
        "t" => "T",
        "th" => "Th",
        "u" => "U",
        "ue" => "Ue",
        "w" => "W",
        "y" => "Y",
        "z" => "Z",
        // Punctuation, which has no case.
        other => match other {
            "'" => "'",
            "\"" => "\"",
            "-" => "-",
            "..." => "...",
            "/" => "/",
            "\\" => "\\",
            "?" => "?",
            "!" => "!",
            ":" => ":",
            ";" => ";",
            "*" => "*",
            "|" => "|",
            "<" => "<",
            ">" => ">",
            " " => " ",
            _ => "",
        },
    }
}

#[rustfmt::skip]
fn lower_folding(c: char) -> Option<(&'static str, &'static str)> {
    Some(match c {
        // Latin-1 Supplement. `ä ö ü ß` carry German's written work-around,
        // `å` Danish and Norwegian's, and `ø` follows `ö` because the same
        // hands type it.
        'à' | 'á' | 'â' | 'ã' => ("a", "a"),
        'ä' => ("a", "ae"),
        'å' => ("a", "aa"),
        'æ' => ("ae", "ae"),
        'ç' => ("c", "c"),
        'è' | 'é' | 'ê' | 'ë' => ("e", "e"),
        'ì' | 'í' | 'î' | 'ï' => ("i", "i"),
        'ð' => ("d", "dh"),
        'ñ' => ("n", "n"),
        'ò' | 'ó' | 'ô' | 'õ' => ("o", "o"),
        'ö' | 'ø' => ("o", "oe"),
        'ù' | 'ú' | 'û' => ("u", "u"),
        'ü' => ("u", "ue"),
        'ý' | 'ÿ' => ("y", "y"),
        'þ' => ("th", "th"),
        'ß' => ("ss", "ss"),

        // Latin Extended-A. Past anything in this library, but it is the
        // block that covers the rest of Europe and it is a table, not code.
        'ā' | 'ă' | 'ą' => ("a", "a"),
        'ć' | 'ĉ' | 'ċ' | 'č' => ("c", "c"),
        'ď' | 'đ' => ("d", "d"),
        'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => ("e", "e"),
        'ĝ' | 'ğ' | 'ġ' | 'ģ' => ("g", "g"),
        'ĥ' | 'ħ' => ("h", "h"),
        'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => ("i", "i"),
        'ĳ' => ("ij", "ij"),
        'ĵ' => ("j", "j"),
        'ķ' | 'ĸ' => ("k", "k"),
        'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => ("l", "l"),
        'ń' | 'ņ' | 'ň' | 'ŉ' => ("n", "n"),
        'ŋ' => ("ng", "ng"),
        'ō' | 'ŏ' | 'ő' => ("o", "o"),
        'œ' => ("oe", "oe"),
        'ŕ' | 'ŗ' | 'ř' => ("r", "r"),
        'ś' | 'ŝ' | 'ş' | 'š' | 'ſ' => ("s", "s"),
        'ţ' | 'ť' | 'ŧ' => ("t", "t"),
        'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => ("u", "u"),
        'ŵ' => ("w", "w"),
        'ŷ' => ("y", "y"),
        'ź' | 'ż' | 'ž' => ("z", "z"),

        // Typography. `’` for an apostrophe is what every tagger writes and
        // no keyboard types.
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' | '\u{00B4}'
        | '\u{0060}' | '\u{FF07}' => ("'", "'"),
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' | '\u{FF02}' => {
            ("\"", "\"")
        }
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' => ("-", "-"),
        '\u{2026}' => ("...", "..."),

        // This library's own doing: the characters Windows forbids are stored
        // as fullwidth stand-ins, so the tag says `/` and the filename says
        // `⧸`. Searching has to put them back or the two never meet.
        '\u{29F8}' | '\u{2215}' | '\u{FF0F}' => ("/", "/"),
        '\u{29F9}' | '\u{FF3C}' => ("\\", "\\"),
        '\u{FF1F}' => ("?", "?"),
        '\u{FF01}' => ("!", "!"),
        '\u{FF1A}' => (":", ":"),
        '\u{FF1B}' => (";", ";"),
        '\u{FF0A}' | '\u{2217}' => ("*", "*"),
        '\u{FF5C}' | '\u{2223}' => ("|", "|"),
        '\u{FF1C}' => ("<", "<"),
        '\u{FF1E}' => (">", ">"),

        // Invisibles. A soft hyphen inside a word makes it unsearchable.
        '\u{00A0}' | '\u{2007}' | '\u{202F}' => (" ", " "),
        '\u{00AD}' | '\u{200B}' | '\u{FEFF}' => ("", ""),

        _ => return None,
    })
}

#[cfg(test)]
mod tests {

    /// The whole point of `match_range`: folding does not preserve length, so
    /// an offset into the folded string is not an offset into the original.
    #[test]
    fn a_match_range_indexes_the_source_not_the_folded_form() {
        // `ö` is one char and folds to one, so this is the easy case.
        let r = match_range("Christian Löffler", &Needle::new("loffler")).expect("a match");
        assert_eq!(
            "Christian Löffler"
                .chars()
                .skip(r.start)
                .take(r.len())
                .collect::<String>(),
            "Löffler"
        );

        // `ß` folds to `ss`: one source char becomes two folded ones, and a
        // naive offset would land one character late.
        let r = match_range("Straße", &Needle::new("strasse")).expect("a match");
        assert_eq!(
            "Straße"
                .chars()
                .skip(r.start)
                .take(r.len())
                .collect::<String>(),
            "Straße"
        );

        // The library's own fullwidth stand-ins, which are what folding is for.
        let title = "Have You Got It In You？";
        let r = match_range(title, &Needle::new("in you?")).expect("a match");
        assert_eq!(
            title
                .chars()
                .skip(r.start)
                .take(r.len())
                .collect::<String>(),
            "In You？"
        );
    }

    /// Spelled-out marks find it too, and report the source range either way.
    #[test]
    fn a_spelled_match_still_points_at_the_source() {
        let r = match_range("Christian Löffler", &Needle::new("loeffler")).expect("a match");
        assert_eq!(
            "Christian Löffler"
                .chars()
                .skip(r.start)
                .take(r.len())
                .collect::<String>(),
            "Löffler"
        );
    }

    /// Smart-case, as §7's filter language has it and as vim and ripgrep do.
    #[test]
    fn a_capital_makes_the_search_precise() {
        let hay = Haystack::of("Burial — Untrue");
        assert!(
            hay.contains(&Needle::new("burial")),
            "lower-case should be insensitive"
        );
        assert!(
            hay.contains(&Needle::new("Burial")),
            "and still find the real thing"
        );
        assert!(
            !hay.contains(&Needle::new("BURIAL")),
            "a capital should mean it"
        );
        // The rule is about the *pattern*, so an all-lower pattern matches a
        // capitalised haystack, which is the common case.
        assert!(Haystack::of("BURIAL").contains(&Needle::new("burial")));
    }

    #[test]
    fn an_empty_needle_matches_nothing_to_highlight() {
        assert!(match_range("anything", &Needle::new("")).is_none());
    }
    use super::*;

    #[test]
    fn an_ascii_string_is_borrowed_untouched() {
        let s = "Anderson .Paak";
        assert!(
            matches!(flatten(s), Cow::Borrowed(_)),
            "should not allocate"
        );
        assert_eq!(expanded(s), None);
    }

    #[test]
    fn an_umlaut_folds_two_ways() {
        assert_eq!(flatten("Christian Löffler"), "Christian Loffler");
        assert_eq!(
            expanded("Christian Löffler").as_deref(),
            Some("Christian Loeffler")
        );
    }

    #[test]
    fn either_spelling_finds_him() {
        let hay = Haystack::of("Christian Löffler");
        for typed in ["löffler", "loffler", "loeffler", "Löffler"] {
            assert!(
                hay.contains(&Needle::new(typed)),
                "{typed:?} should have matched"
            );
        }
        // `LOFFLER` used to match too, when every search was unconditionally
        // insensitive. Under smart-case a capital means it, so an all-caps
        // pattern is precise and correctly misses — which is what vim and
        // ripgrep do, and what §7's filter language already did.
        assert!(
            !hay.contains(&Needle::new("LOFFLER")),
            "smart-case should make an all-caps pattern precise"
        );
    }

    #[test]
    fn a_wrong_spelling_still_misses() {
        let hay = Haystack::of("Christian Löffler");
        assert!(
            !hay.contains(&Needle::new("lieffler")),
            "folding must not turn search into fuzzy matching"
        );
    }

    #[test]
    fn the_fullwidth_stand_ins_fold_back_to_what_the_tag_says() {
        // What this library writes to disk, and what the tag it came from says.
        let hay = Haystack::of("HEK004 - Air & Lack Thereof ⧸ Sparing the Horses");
        assert!(hay.contains(&Needle::new("Air & Lack Thereof / Sparing")));
        assert!(
            hay.contains(&Needle::new("thereof ⧸ sparing")),
            "and as written"
        );
    }

    #[test]
    fn a_typographic_apostrophe_is_an_apostrophe() {
        let hay = Haystack::of("Baby, Please Don’t Go");
        assert!(hay.contains(&Needle::new("don't go")));
        assert!(hay.contains(&Needle::new("don’t go")));
    }

    #[test]
    fn case_survives_flattening() {
        assert_eq!(flatten("ÖSTERREICH Über"), "OSTERREICH Uber");
        assert_eq!(expanded("Über").as_deref(), Some("Ueber"));
    }

    #[test]
    fn eszett_and_the_slashed_o_have_no_decomposition_and_still_fold() {
        // The characters NFD would have left alone, which is why this is a
        // table and not a call to `unicode-normalization`.
        assert_eq!(flatten("Straße"), "Strasse");
        assert_eq!(flatten("Sigur Rós"), "Sigur Ros");
        assert_eq!(flatten("Mørk"), "Mork");
        assert_eq!(expanded("Mørk").as_deref(), Some("Moerk"));
    }

    #[test]
    fn a_soft_hyphen_does_not_hide_a_word() {
        assert!(Haystack::of("Un\u{00AD}known").contains(&Needle::new("unknown")));
    }

    #[test]
    fn expanded_is_none_when_it_would_only_repeat_flatten() {
        // `é` has one sensible answer, so a second search would be wasted.
        assert_eq!(flatten("Beyoncé"), "Beyonce");
        assert_eq!(expanded("Beyoncé"), None);
    }
}
