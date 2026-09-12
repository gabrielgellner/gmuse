//! Normalising a name for ordering — issue #84.
//!
//! Lives in the model because how a library sorts its own names is a librarian
//! question, not a player one. `library.rs` re-exports it, which is the
//! allowed direction: the player may reach into the model and never the
//! reverse (#77).

/// A normalised sort key: case-folded, marks flattened, and with a leading
/// article moved out of the way when `smart_artist_sort` is on.
///
/// Kept separate from the display string, which stays on the `Track` — "The
/// Beatles" must sort under B and still render with its "The".
///
/// The marks are flattened for **ordering**, not only for looks: `ö` is U+00F6,
/// so a raw comparison files Löffler after every artist beginning `lz`, which
/// is to say off the end of the L section entirely.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SortKey(String);

impl SortKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Leading articles stripped for sorting. Lower-case; the input is folded first.
const ARTICLES: &[&str] = &["the ", "a ", "an "];

/// Build a sort key.
///
/// `smart` strips a leading article, matching cmus's `smart_artist_sort`. It is
/// applied to artists only — "The Wall" is an album whose T matters.
pub fn sort_key(s: &str, smart: bool) -> SortKey {
    let folded = crate::text::needle(s.trim());
    if smart {
        for article in ARTICLES {
            if let Some(rest) = folded.strip_prefix(article) {
                let rest = rest.trim_start();
                if !rest.is_empty() {
                    return SortKey(rest.to_string());
                }
            }
        }
    }
    SortKey(folded)
}
