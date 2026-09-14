//! **What `query.run` spends its time on** — #261.
//!
//! A picker filters on every keystroke, so the question is whether a query is a keystroke or a
//! command. Measured against the real library.
fn main() {
    let data = std::path::PathBuf::from(std::env::args().nth(1).expect("pass the data dir"));
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());
    let now = gmuse::history::now_ms();

    let projection = gmuse::librarian::reading(&data, device, now);
    let catalogue = projection.catalogue();
    let library = gmuse::model::seam::Librarian::from_catalogue(catalogue);
    let rows = {
        use gmuse::model::rows::Listing;
        library.rows()
    };
    assert!(
        rows.len() > 1_000,
        "only {} rows — this measures nothing",
        rows.len()
    );
    println!(
        "{} rows, {} recordings",
        rows.len(),
        catalogue.recordings.len()
    );

    let t = std::time::Instant::now();
    let found = gmuse::query::rows(
        catalogue,
        &library,
        device,
        now,
        gmuse::query::Only::Everything,
        gmuse::query::Copies::Shown,
        None,
    );
    println!(
        "query::rows (the tool path):  {:>8.1?}  {} rows",
        t.elapsed(),
        found.len()
    );

    // The picker path, which is what `query.run` answers with.
    let t = std::time::Instant::now();
    let picked = gmuse::query::found::found(&library, now, None);
    println!(
        "query::found (the picker path):{:>8.1?}  {} rows",
        t.elapsed(),
        picked.len()
    );

    // What a *picker* needs: the same filtering, over rows, without a locator.
    let t = std::time::Instant::now();
    let drawn: Vec<_> = {
        use gmuse::model::rows::Listing;
        rows.iter()
            .filter_map(|id| library.listed(*id).map(|l| (id, l.title.to_owned())))
            .collect()
    };
    println!(
        "listed() over every row:      {:>8.1?}  {} rows",
        t.elapsed(),
        drawn.len()
    );

    // The browse tree, which `query.tree` answers with.
    let members: std::collections::HashSet<_> = rows.iter().copied().collect();
    let t = std::time::Instant::now();
    let shelf = gmuse::shelf::Shelf::grouped(
        &library,
        members.clone(),
        gmuse::shelf::Levels::default(),
        true,
    );
    println!(
        "Shelf::grouped:               {:>8.1?}  {} artists",
        t.elapsed(),
        shelf.artist_count()
    );

    let t = std::time::Instant::now();
    let nodes = gmuse::browse::tree(&library, members, gmuse::shelf::Levels::default(), true);
    println!(
        "browse::tree (grouped+flat):  {:>8.1?}  {} nodes",
        t.elapsed(),
        nodes.len()
    );

    // And the locator lookup alone, which the tool path does per recording.
    let t = std::time::Instant::now();
    let mut opened = 0usize;
    for recording in catalogue.recordings.keys() {
        if library.open(*recording, &[device], &[]).is_some() {
            opened += 1;
        }
    }
    println!(
        "open() per recording:         {:>8.1?}  {opened} opened",
        t.elapsed()
    );
}
