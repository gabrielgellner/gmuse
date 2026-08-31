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

/// One row folded once, so a search that walks the whole view does the work
/// per row rather than per row per keystroke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Haystack {
    flat: String,
    spelled: Option<String>,
}

impl Haystack {
    pub fn of(s: &str) -> Self {
        Self {
            flat: needle(s),
            spelled: expanded(s).map(|e| e.to_lowercase()),
        }
    }

    /// `needle` must already have been through [`needle`].
    pub fn contains(&self, needle: &str) -> bool {
        self.flat.contains(needle) || self.spelled.as_deref().is_some_and(|s| s.contains(needle))
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
        for typed in ["löffler", "loffler", "loeffler", "Löffler", "LOFFLER"] {
            assert!(
                hay.contains(&needle(typed)),
                "{typed:?} should have matched"
            );
        }
    }

    #[test]
    fn a_wrong_spelling_still_misses() {
        let hay = Haystack::of("Christian Löffler");
        assert!(
            !hay.contains(&needle("lieffler")),
            "folding must not turn search into fuzzy matching"
        );
    }

    #[test]
    fn the_fullwidth_stand_ins_fold_back_to_what_the_tag_says() {
        // What this library writes to disk, and what the tag it came from says.
        let hay = Haystack::of("HEK004 - Air & Lack Thereof ⧸ Sparing the Horses");
        assert!(hay.contains(&needle("Air & Lack Thereof / Sparing")));
        assert!(hay.contains(&needle("thereof ⧸ sparing")), "and as written");
    }

    #[test]
    fn a_typographic_apostrophe_is_an_apostrophe() {
        let hay = Haystack::of("Baby, Please Don’t Go");
        assert!(hay.contains(&needle("don't go")));
        assert!(hay.contains(&needle("don’t go")));
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
        assert!(Haystack::of("Un\u{00AD}known").contains(&needle("unknown")));
    }

    #[test]
    fn expanded_is_none_when_it_would_only_repeat_flatten() {
        // `é` has one sensible answer, so a second search would be wasted.
        assert_eq!(flatten("Beyoncé"), "Beyonce");
        assert_eq!(expanded("Beyoncé"), None);
    }
}
