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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Found {
    pub row: CopyId,
    pub recording: Option<Id<crate::model::entity::Recording>>,
    pub title: String,
    pub file_under: String,
    pub album: String,
    pub artist: Option<String>,
    pub length_ms: u64,
}

/// Every row matching `filter`, in the shelf's order.
///
/// **Ordered, because a picker is a list.** `Listing::rows` is the library's own order and a
/// caller pages into it; two calls with the same filter and the same fold give the same page.
pub fn found(
    library: &Librarian,
    now: i64,
    filter: Option<&crate::query::filter::Expr>,
) -> Vec<Found> {
    // **Compiled once, not per row** — the same reason `rows` does it.
    let env = crate::query::filter::Env {
        listened: None,
        named: Some(library),
        now: now / 1000,
    };
    let keep = filter.map(|f| f.compile(env));
    // The whole cost of the tool path, asked once instead of assumed.
    let locators = filter.is_some_and(|f| f.needs_a_locator());

    let mut out = Vec::new();
    for id in library.rows() {
        let Some(listed) = library.listed(id) else {
            continue;
        };
        if let Some(keep) = &keep {
            let subject = crate::model::rows::Subject {
                row: listed,
                // **`None` unless the filter reads it.** A `path=` filter against `None`
                // matches nothing, which is why this is asked rather than always skipped.
                locator: locators
                    .then(|| library.playable(id).map(|p| p.locator))
                    .flatten(),
                recording: library.recording_of(id),
            };
            if !keep(&subject) {
                continue;
            }
        }
        out.push(Found {
            row: id,
            recording: library.recording_of(id),
            title: listed.title.to_owned(),
            file_under: listed.file_under.to_owned(),
            album: listed.album.to_owned(),
            artist: listed.artist.map(str::to_owned),
            length_ms: listed.duration.as_millis() as u64,
        });
    }
    out
}
