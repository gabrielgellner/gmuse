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
    engine: Option<std::sync::Arc<gmuse_client::Client>>,
    members: HashSet<CopyId>,
    levels: Levels,
    smart_artist_sort: bool,
) -> Box<dyn Holding> {
    if let Some(client) = engine {
        return Box::new(crate::ui::holding_remote::Remote::open(client));
    }
    match fold {
        Some(library) => Box::new(Shelf::grouped(library, members, levels, smart_artist_sort)),
        None => Box::new(Shelf::default()),
    }
}
