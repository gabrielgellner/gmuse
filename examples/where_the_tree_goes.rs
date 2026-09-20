//! **Which half of a tree costs the 0.40 s** — #261.
//!
//! `query.tree` calls `Shelf::grouped` and then flattens it. If the cost is in the grouping,
//! a fold state can travel with a request and the shelf can be kept; if it is in the
//! flattening, it cannot, because every `zo` would rebuild.
fn main() {
    let data = std::path::PathBuf::from(std::env::args().nth(1).expect("pass the data dir"));
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());
    let now = gmuse::history::now_ms();

    let projection = gmuse::librarian::reading(&data, device, now);
    let catalogue = projection.catalogue();
    let library = gmuse::model::seam::Librarian::from_catalogue(catalogue);
    let members: std::collections::HashSet<_> = {
        use gmuse::model::rows::Listing;
        library.rows().into_iter().collect()
    };
    assert!(
        members.len() > 1_000,
        "only {} rows — this measures nothing",
        members.len()
    );
    let levels = gmuse::shelf::Levels::default();
    println!("{} rows", members.len());

    // **The grouping's two halves** — #270. `Tree::grouped` decides where every row goes;
    // `describe` per row writes the line that is drawn. Which of them is the 800 ms decides
    // whether a tree could be cached across a rating (the shape) or must be re-derived (the
    // lines).
    let pair = levels.pair().expect("grouped");
    let t = std::time::Instant::now();
    let tree = gmuse::model::tree::Tree::grouped(&library, pair, true, &members);
    let shaping = t.elapsed();
    let t = std::time::Instant::now();
    let described = members
        .iter()
        .filter_map(|id| library.describe(*id))
        .count();
    let describing = t.elapsed();
    assert!(
        described > 100,
        "describe answered {described} rows — nothing was measured"
    );
    println!(
        "  Tree::grouped (the shape):    {shaping:>8.1?}   {} artists",
        tree.artists().count()
    );
    println!("  describe() over every row:    {describing:>8.1?}   {described} rows");

    let t = std::time::Instant::now();
    let shelf = gmuse::shelf::Shelf::grouped(&library, members.clone(), levels, true);
    let grouping = t.elapsed();
    println!(
        "Shelf::grouped:                {:>8.1?}   {} artists",
        grouping,
        shelf.artist_count()
    );

    // The flatten, over a shelf already built — what a fold change would have to redo.
    let t = std::time::Instant::now();
    let folds = gmuse::fold::Expansion::default();
    let nodes = gmuse::browse::flatten(&shelf, &folds, true);
    let flatten = t.elapsed();
    assert!(
        nodes.len() > 1_000,
        "only {} nodes — the flatten measured nothing",
        nodes.len()
    );
    println!(
        "  flatten over it:             {:>8.1?}   {} nodes",
        flatten,
        nodes.len()
    );

    let t = std::time::Instant::now();
    let whole = gmuse::browse::tree(&library, members, levels, true);
    println!(
        "browse::tree (both together):  {:>8.1?}   {} nodes",
        t.elapsed(),
        whole.len()
    );
    println!(
        "\nthe flatten is {:.1}% of the pair",
        100.0 * flatten.as_secs_f64() / (grouping + flatten).as_secs_f64()
    );
}
