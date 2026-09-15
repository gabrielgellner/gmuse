//! **Where a view chooses what it holds** — #261, `tree-over-the-wire.md` §7.
//!
//! A view browses through `Holding`: the tree, the flat ordering, and four counts. It names no
//! projection anywhere else, which is the point — the type is what stops a follower reaching
//! past the seam, and reaching past it would otherwise compile perfectly well.
//!
//! **So this is the one place an implementation is picked, and today there is one.** A detached
//! view *is* the librarian (§4.4), so it folds the log and groups a shelf, which is correct and
//! costs what it costs. An attached view should not: its librarian is the engine, and
//! `query.tree` and `query.reveal` are the trait's own methods on the wire. That second
//! implementation belongs here, beside this one, and until it exists an attached view folds a
//! second copy of a library the engine already has — the 40.6 MB in §1.

use std::collections::HashSet;

use crate::browse::Holding;
use crate::model::rows::CopyId;
use crate::model::seam::Librarian;
use crate::shelf::{Levels, Shelf};

/// Build the reader for a view — #261.
///
/// **The view crosses the wire, not the membership.** An attached view used to resolve its own
/// terms and hand the result here, where it was dropped on the floor: the engine was never told
/// which rows were in the view, so an attached gmuse drew the whole library whatever `:view`
/// said. It also cost 732-824ms a change to compute the set that was then discarded.
///
/// A reader is built per view rather than retuned, because `Holding::sorted` lends and the flat
/// ordering is the view's — see [`crate::ui::holding_remote::Remote::showing`].
pub fn reader(
    client: std::sync::Arc<gmuse_client::Client>,
    was: Option<&crate::ui::holding_remote::Remote>,
    showing: crate::ui::holding_remote::Showing,
) -> crate::ui::holding_remote::Remote {
    match was {
        // The same view asks the same questions, and rebuilding would re-walk the ordering.
        Some(remote) if *remote.shows() == showing => remote.clone(),
        _ => crate::ui::holding_remote::Remote::showing(client, showing),
    }
}

/// Group `members` into what a view browses, or ask whoever already has.
///
/// **The engine wins when there is one.** §4.2 says an attached view's librarian is the engine,
/// and the engine has folded this library already — a second fold in this process would be the
/// same answer computed twice and held twice. A detached view has no engine and *is* the
/// librarian, so it groups its own.
///
/// `fold` is `None` for a view that holds none, which is where this is going; an empty shelf is
/// what a view with neither gets, and it draws an empty library rather than failing.
pub fn of(
    fold: Option<&Librarian>,
    engine: Option<crate::ui::holding_remote::Remote>,
    members: HashSet<CopyId>,
    levels: Levels,
    smart_artist_sort: bool,
) -> Box<dyn Holding> {
    if let Some(remote) = engine {
        return Box::new(remote);
    }
    // A detached view resolved the members itself, which is the walk an attached one no
    // longer does: its view crossed the wire and the librarian resolved it (#261).
    match fold {
        Some(library) => Box::new(Shelf::grouped(library, members, levels, smart_artist_sort)),
        None => Box::new(Shelf::default()),
    }
}
