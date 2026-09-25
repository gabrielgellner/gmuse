//! **What view 2 pays per frame, and which half of it** — #295.
//!
//! The session log said `draw 540.2ms — 7289 rows, 1 window(s)` with no panel and no page
//! on the line, and `examples/what_a_view_costs` accounts for only 67ms of it by paging the
//! flat ordering once. The rest is in the draw, and the draw runs it again every frame.
//!
//! `draw_track_list` windows what it *formats* (#261) but not what it *groups*: with
//! `album_headers` on — the default — it calls `ahead` and `list::rows` over the whole list
//! before the window is taken, so a 7,289-row view names 7,289 rows a keystroke.
//!
//! Four numbers, because the fix has to know which one to remove.
fn main() {
    use gmuse::browse::Holding;
    use gmuse::ui::holding_remote::{Remote, Showing};

    let socket = gmuse::engine::socket::default_path();
    let client = std::sync::Arc::new(
        gmuse_client::Client::connect(&socket, "measure", "0")
            .expect("an engine to be listening — start one with `gmuse engine`"),
    );

    let remote = Remote::showing(
        std::sync::Arc::clone(&client),
        Showing {
            view: None,
            levels: None,
        },
    );

    // Paid once, and already accounted for by `what_a_view_costs`.
    let t = std::time::Instant::now();
    let ids: Vec<_> = Holding::sorted(&remote).to_vec();
    println!(
        "{:<34} {:>10.1?}  {} rows",
        "ordering (cached after this)",
        t.elapsed(),
        ids.len()
    );

    let lib: &dyn gmuse::model::rows::Listing = &remote;

    // What the draw does before it takes its window, twice over, because both halves are
    // per-frame and only one of them is obvious.
    for pass in ["first", "second (cache warm)"] {
        let t = std::time::Instant::now();
        lib.ahead(&ids);
        let ahead = t.elapsed();

        let t = std::time::Instant::now();
        let rows = gmuse::ui::list::rows(lib, &ids);
        let grouping = t.elapsed();

        println!(
            "{:<34} {:>10.1?}  ahead\n{:<34} {:>10.1?}  list::rows -> {} screen rows\n{:<34} {:>10.1?}  == per frame",
            format!("[{pass}]"),
            ahead,
            "",
            grouping,
            rows.len(),
            "",
            ahead + grouping,
        );
    }

    // **And through the cache the fix adds** (#295), which is what a frame actually pays once
    // the list has been grouped: the same key, so the shared `Rc` is cloned and nothing is read.
    {
        use gmuse::ui::GroupedKey;
        let key: GroupedKey = (
            "sorted".to_string(),
            ids.len(),
            ids.first().copied(),
            ids.last().copied(),
            true,
        );
        let ui = gmuse::ui::Ui::new(Default::default());
        let built = std::rc::Rc::new(gmuse::ui::list::rows(lib, &ids));
        ui.grouped.replace(Some((key, std::rc::Rc::clone(&built))));

        let t = std::time::Instant::now();
        let hit = ui
            .grouped
            .borrow()
            .as_ref()
            .filter(|(had, _)| had.1 == ids.len() && had.4)
            .map(|(_, rows)| std::rc::Rc::clone(rows));
        let cached = t.elapsed();
        println!(
            "{:<34} {:>10.1?}  {} screen rows",
            "cached frame (the fix)",
            cached,
            hit.expect("a hit").len()
        );
    }

    // And what the same frame costs with the grouping off, which is the `else` branch
    // already in the code and the shape any fix has to get back to.
    let t = std::time::Instant::now();
    let plain: Vec<_> = ids
        .iter()
        .copied()
        .map(gmuse::ui::list::Row::Track)
        .collect();
    println!(
        "{:<34} {:>10.1?}  {} screen rows",
        "album_headers = false",
        t.elapsed(),
        plain.len()
    );
}
