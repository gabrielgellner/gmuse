//! **Rows for a picker** — #261, §7.11.
//!
//! [`crate::query::rows`] is the *tool* path: it hands a pass an id and a locator, which is the
//! contract `apply` completes. Producing a locator costs a `Librarian::open` per recording —
//! **208 ms of a 298 ms query** on the real library — and a picker never looks at one.
//!
//! ```text
//! query::rows (the tool path)   298.5ms
//!   of which open() per row     208.5ms
//! listed() over every row         0.65ms
//! ```
//!
//! So this filters over the fold's rows and returns what a view draws. It resolves a locator
//! **only when the filter asks for one** — `path=` and `filename=` are the two fields that do,
//! and `Expr::needs_a_locator` is how it knows.

use crate::model::id::Id;
use crate::model::rows::{CopyId, Listing};
use crate::model::seam::Librarian;

/// One row, as a picker draws it.
///
/// **No locator and no encoding.** A view identifies a track by its recording (I17), and a path
/// is not a user-facing grain — `Listed`'s own comment is that a row "holds identities and
/// values and **never a location**".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Found {
    pub row: CopyId,
    pub recording: Option<Id<crate::model::entity::Recording>>,
    pub title: String,
    pub file_under: String,
    pub album: String,
    pub artist: Option<String>,
    pub length_ms: u64,
    /// **Where the filter matched, so a picker can underline it** — #261.
    ///
    /// Empty when nothing was asked or nothing is pointable-at. Ranges are **character**
    /// indices into the strings in this same struct, which is the only reason they are
    /// meaningful on the far side of a socket: the match ran over folded, lower-cased text
    /// whose characters do not line up with these, and `Glob::spans` maps them back.
    ///
    /// **Here rather than in the view** is the whole point (§7.11's done-when). If the rows
    /// crossed without their spans, a picker would need the matcher — and then the rows — to
    /// draw an underline, which is holding a library again by the back door.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub matched: Vec<Marked>,
}

/// One run of characters the filter matched, in one of the row's fields.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Marked {
    /// The field's name as the query language spells it — `title`, `artist`, `album`,
    /// `file_under`. A name rather than an index, because a reply a client has to switch on by
    /// position is a reply nothing can document.
    pub field: String,
    /// Character index of the first marked character.
    pub at: usize,
    /// Character index one past the last.
    pub to: usize,
}

/// Every row matching `filter`, in the shelf's order.
///
/// **Ordered, because a picker is a list.** `Listing::rows` is the library's own order and a
/// caller pages into it; two calls with the same filter and the same fold give the same page.
pub fn found(
    library: &Librarian,
    listened: &dyn crate::query::filter::Listened,
    now: i64,
    filter: Option<&crate::query::filter::Expr>,
) -> Vec<Found> {
    // **The history, because a picker asks about it** — #261.
    //
    // This passed `None`, and the whole class of fields that reads it answered as though the
    // library had never been played: `rating>=4` found **0 rows where the browse tree found
    // 35**, and so did `plays`, `last_played` and `days_since_*`. Silently — a filter over a
    // field nothing can satisfy is a filter that matches nothing, which is what an empty
    // picker looks like anyway.
    let env = crate::query::filter::Env {
        listened: Some(listened),
        named: Some(library),
        now: now / 1000,
    };
    let keep = filter.map(|f| f.compile(env));
    // The whole cost of the tool path, asked once instead of assumed.
    let locators = filter.is_some_and(|f| f.needs_a_locator());

    let mut out = Vec::new();
    for id in library.rows() {
        let Some(listed) = library.describe(id) else {
            continue;
        };
        if let Some(keep) = &keep {
            // **`None` unless the filter reads it.** A `path=` filter against `None`
            // matches nothing, which is why this is asked rather than always skipped.
            let at = locators
                .then(|| library.playable(id).map(|p| p.locator))
                .flatten();
            let subject = crate::model::rows::Subject {
                row: listed,
                locator: at.as_deref(),
                recording: library.recording_of(id),
            };
            if !keep(&subject) {
                continue;
            }
        }
        let title = listed.title.to_owned();
        let file_under = listed.file_under.to_owned();
        let album = listed.album.to_owned();
        let artist = listed.artist.map(str::to_owned);
        let matched = filter
            .map(|f| marks(f, &title, &file_under, &album, artist.as_deref()))
            .unwrap_or_default();
        out.push(Found {
            row: id,
            recording: library.recording_of(id),
            title,
            file_under,
            album,
            artist,
            length_ms: listed.duration.as_millis() as u64,
            matched,
        });
    }
    out
}

/// Where each of the filter's text patterns landed in the four fields a picker draws.
///
/// **Only those four.** A filter may look at a path, a year or a claim's basis, and none of
/// them is on the line — a span pointing at text the reply does not carry is one a client
/// cannot draw and cannot check.
fn marks(
    filter: &crate::query::filter::Expr,
    title: &str,
    file_under: &str,
    album: &str,
    artist: Option<&str>,
) -> Vec<Marked> {
    use crate::query::filter::Selector;
    use crate::query::format::Field;

    let drawn: [(&str, Field, &str); 4] = [
        ("title", Field::Title, title),
        ("file_under", Field::FileUnder, file_under),
        ("album", Field::Album, album),
        ("artist", Field::Artist, artist.unwrap_or("")),
    ];

    let mut out = Vec::new();
    for (selector, glob) in filter.patterns() {
        for (name, field, text) in drawn {
            let looks_here = match selector {
                Selector::Any => true,
                Selector::Field(_, _, f) => *f == field,
                Selector::History(_) => false,
            };
            if !looks_here || text.is_empty() {
                continue;
            }
            out.extend(glob.spans(text).into_iter().map(|r| Marked {
                field: name.to_string(),
                at: r.start,
                to: r.end,
            }));
        }
    }
    // **Deduplicated and ordered**, because `any="x" & title="*x*"` is two patterns finding one
    // run, and a client drawing both would style the same characters twice.
    out.sort_by(|a, b| (&a.field, a.at, a.to).cmp(&(&b.field, b.at, b.to)));
    out.dedup();

    out
}
