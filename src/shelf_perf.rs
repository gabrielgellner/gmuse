//! **What it costs to draw a view** — issue #121, rehoused by #213.
//!
//! It lived at the end of `shelf.rs`, which is now in `gmuse-library`. The
//! bench measures `view::Stack` — a query parsed through the command language
//! — so following `shelf` into the library would have meant dragging `view`
//! and `cmd`, and with them the whole command language, across the boundary to
//! run one test. It measures a binary-side type against library data, so it
//! belongs on this side.
//!
//! `just bench-shelf` is unchanged.

#[cfg(test)]
mod perf {
    //! What it costs to draw a view — issue #121.
    //!
    //! `just bench-shelf`. The resampler has had a bench since the beginning
    //! and the library path had none, so the only signal that drawing a view
    //! had become slow was somebody noticing that it felt slow. It had: on a
    //! 6,485-row library `:view in="Dance"` spent **1.3 seconds** in
    //! `Shelf::grouped`, and the cost did not depend on how many rows matched.
    //!
    //! Synthetic rather than the machine's own library, because the numbers
    //! have to mean the same thing on two machines a week apart.
    //!
    //! Three phases, kept apart because they fail differently: **resolve** is
    //! the filter, **copies** turns recordings into rows, and **shelf** groups
    //! them into the tree. The one that went wrong was the third, and a single
    //! end-to-end number would have hidden which.

    use std::collections::HashSet;
    use std::time::Instant;

    use crate::model::instance::{Instance, Scanned};
    use crate::model::rows::CopyId;
    use crate::model::rows::Listing;
    use crate::model::seam::Librarian;
    use crate::shelf::*;

    const GENRES: [&str; 6] = ["Techno", "House", "Folk", "Rock", "Jazz", "Ambient"];

    /// `albums` albums of ten tracks over `artists` artists — the shape of a
    /// real library at the size the slowness showed up at.
    fn library(artists: usize, albums: usize) -> Librarian {
        let mut scan = Vec::new();
        for a in 0..albums {
            for track in 1..=10u16 {
                let artist = format!("Artist {:04}", a % artists);
                scan.push(Scanned {
                    audio: crate::audio_id::AudioId {
                        scheme: 1,
                        hash: (a as u128) * 1000 + u128::from(track),
                    },
                    title: format!("Track {track:02} of {a:04}"),
                    artist: artist.clone(),
                    album: Some(format!("Album {a:04}")),
                    album_artist: Some(artist),
                    genres: vec![GENRES[a % GENRES.len()].to_string()],
                    disc: Some(1),
                    track: Some(track),
                    // **The totals, because the real library carries them**
                    // (#134): 88% of it does, and a bench whose releases
                    // cannot be counted never runs the counting.
                    track_total: Some(10),
                    disc_total: Some(1),
                    length_ms: 200_000,
                    ..Default::default()
                });
            }
        }
        let mut log = Instance::new(crate::fixture::device());
        log.ingest(100, &scan);
        let mut l = Librarian::from_catalogue(&log.catalogue());
        l.here(crate::fixture::device());
        l
    }

    /// The table, and the bound that makes it a test rather than a report.
    ///
    /// **Generous on purpose.** A tight budget on a shared machine is a flaky
    /// test, and what this has to catch is not a 20% drift — it is the shape
    /// of the bug it was written for, where narrowing to a tenth of the
    /// library cost what drawing all of it did. Fifty milliseconds is roughly
    /// forty times what a narrowed view takes now and still a fiftieth of what
    /// it took before the fix.
    #[test]
    fn drawing_a_view_scales_with_what_it_drew() {
        let l = library(200, 650);
        let rows = Listing::rows(&l).len();
        let listened = crate::projection::heard::Heard::default();
        let env = crate::query::filter::Env {
            listened: Some(&listened),
            named: None,
            now: 1_700_000_000,
        };

        eprintln!("\n{rows} rows\n");
        eprintln!(
            "{:<28} {:>9} {:>9} {:>9} {:>7}",
            "query", "resolve", "copies", "shelf", "rows"
        );
        // **Per drawn row, against the whole library's own rate.** The bound
        // used to be a wall clock — 50ms — and a wall clock measures the
        // machine as much as the code. Under `cargo test` this runs beside
        // 1,300 other tests and takes 52-58ms for the contention alone, where
        // on its own it takes 46-48; so `just ci` failed at random and the one
        // safe reading of a red bar was "look again".
        //
        // The claim was never about milliseconds. #121 is that drawing a view
        // cost the same whatever the filter matched — 1.3 seconds for a
        // handful of rows out of 6,485 — so what has to hold is that the cost
        // *follows the rows*. A ratio says that, and contention cancels out of
        // it because it inflates both sides equally.
        let mut full: Option<(std::time::Duration, usize)> = None;
        let mut worst: Option<(f64, &str, usize)> = None;
        for src in [
            "all",
            r#"genre="Techno""#,
            r#"artist="Artist 0001""#,
            r#"genre="Techno" &artist="Artist 0001""#,
        ] {
            let stack = crate::view::Stack::parse(src).expect("the bench's own queries parse");

            let t = Instant::now();
            let resolved = stack.resolve(&l, &listened, env);
            let resolve = t.elapsed();

            let t = Instant::now();
            let members: HashSet<CopyId> = resolved.copies(&l);
            let copies = t.elapsed();
            let n = members.len();

            let t = Instant::now();
            let shelf = Shelf::grouped(&l, members, Levels::default(), true);
            let grouped = t.elapsed();

            eprintln!(
                "{src:<28} {resolve:>9.1?} {copies:>9.1?} {grouped:>9.1?} {:>7}",
                shelf.len()
            );
            assert_eq!(shelf.len(), n, "the shelf lost rows the filter matched");

            match full {
                // "all" is first, and is the rate everything else is read
                // against: what this library costs per row when nothing is
                // filtered out.
                None => full = Some((grouped, n)),
                Some((whole, all_rows)) => {
                    let rate = whole.as_secs_f64() / all_rows as f64;
                    if n == 0 {
                        // Nothing drawn, so there is no rate to compare — what
                        // must hold is that it did not walk the library to
                        // find that out. Measured at 1.1µs against 331ms.
                        assert!(
                            grouped.as_secs_f64() < whole.as_secs_f64() / 10.0,
                            "a query matching nothing took {grouped:?} against {whole:?} for \
                             the whole library — it is walking rows it will not draw"
                        );
                    } else {
                        let mine = grouped.as_secs_f64() / n as f64;
                        if worst.is_none_or(|(w, ..)| mine / rate > w) {
                            worst = Some((mine / rate, src, n));
                        }
                    }
                }
            }
        }

        let (whole, all_rows) = full.expect("the bench asks for `all` first");
        let (ratio, src, n) = worst.expect("the bench asks for narrowed views too");
        // **Four times the full library's rate**, which is loose on purpose:
        // the numbers sit within 15% of each other in practice (51, 48 and
        // 45 microseconds a row), and the regression this exists for is not
        // subtle — drawing 1,090 rows at the whole library's *total* cost is
        // six times the rate, and 40 rows at it is a hundred and eighty.
        assert!(
            ratio < 4.0,
            "{src:?} drew {n} of {all_rows} rows at {ratio:.1}x the rate the whole library \
             manages ({whole:?} for all of it) — it is paying for rows it did not draw"
        );
    }

    /// **What it costs to relocate a projection to a thing** — #281, `just
    /// bench-reveal`.
    ///
    /// `Browsing::find` is the one door a relocation goes through: `gp` (go to
    /// playing) and `/` both use it, and #281 would use it to walk a window to
    /// whatever the panel is focused on. It **flattens the whole library
    /// unfolded** and scans — "a closed fold still holds its lines", so the
    /// line it wants does not exist until everything is open.
    ///
    /// That is the number #281 turns on, and it matters more than the issue
    /// knew: #276 made the focus follow the cursor on **every key**. If a
    /// projection relocated on every focus change, every `j` would pay this.
    ///
    /// **A ratio, not a wall clock** — the same reason as the bench above: a
    /// budget in milliseconds measures the machine, and under `cargo test` this
    /// runs beside six hundred others. What has to hold is that a reveal costs
    /// about what one flatten costs, because that is the claim — one pass, not
    /// a search that rescans per candidate.
    #[test]
    fn revealing_a_track_costs_one_flatten() {
        use crate::browse::{Browsing, Target};

        let l = library(200, 650);
        let members: HashSet<CopyId> = Listing::rows(&l).iter().copied().collect();
        let shelf = Shelf::grouped(&l, members, Levels::default(), true);
        let folds = crate::fold::Expansion::default();
        let rows = shelf.len();

        // Tracks spread through the library, so the linear scan's position
        // varies: first, middle and last are different amounts of walking.
        let ids = shelf.sorted();
        assert!(
            ids.len() > 100,
            "the bench library is too small to say much"
        );
        let probes = [0, ids.len() / 2, ids.len() - 1];

        eprintln!("\n{rows} rows, {} tracks\n", ids.len());
        eprintln!("{:<28} {:>11}", "reveal", "find");

        let mut worst = std::time::Duration::ZERO;
        for at in probes {
            let id = ids[at];
            let t = Instant::now();
            let found = shelf.find(&folds, &Target::Id(id), 0);
            let took = t.elapsed();
            assert!(found.is_some(), "the track is in the library it came from");
            eprintln!("{:<28} {took:>11.1?}", format!("track {at}"));
            worst = worst.max(took);
        }

        // The reference: one unfolded flatten, which is what `find` does before
        // it can scan at all.
        let t = Instant::now();
        let all = crate::browse::flatten(&shelf, &folds, true);
        let flatten = t.elapsed();
        eprintln!(
            "{:<28} {flatten:>11.1?}   ({} nodes)",
            "one flatten",
            all.len()
        );
        eprintln!();

        // **The instrument**: the flatten has to be doing real work, or "find
        // costs about a flatten" is two numbers near zero agreeing.
        assert!(
            all.len() > 1_000,
            "only {} nodes unfolded — this library cannot show the cost",
            all.len()
        );

        // The claim. Three flattens of headroom: `find` does one, plus a scan
        // over the same vector, and the scan is cheaper than building it.
        assert!(
            worst < flatten * 3,
            "a reveal took {worst:?} against {flatten:?} for the flatten it \
             needs — it is doing more than one pass"
        );
    }
}
