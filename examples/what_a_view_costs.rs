//! **What changing the view costs when the library is elsewhere** — #261.
//!
//! A view used to be resolved twice: once here, by reading every row over the socket, and once
//! in the engine, which had already folded the library. Only the engine's answer was drawn, so
//! an attached view never narrowed at all — and paid 732-824ms a change not to.
//!
//! Two numbers, because a rebuild pays both: asking for the view, and paging the flat ordering
//! that view 2 scrolls and the cursor is restored into.
fn main() {
    use gmuse::ui::holding_remote::{Remote, Showing};

    let socket = gmuse::engine::socket::default_path();
    let client = std::sync::Arc::new(
        gmuse_client::Client::connect(&socket, "measure", "0")
            .expect("an engine to be listening — start one with `gmuse engine`"),
    );

    println!("{:<28} {:>10} {:>10}  rows", "view", "asked", "ordering");
    for source in [
        None,
        Some("artist=\"Boards of Canada\""),
        Some("rating>=4"),
        Some("saved=\"no such view\""),
    ] {
        let t = std::time::Instant::now();
        let remote = Remote::showing(
            std::sync::Arc::clone(&client),
            Showing {
                view: source.map(str::to_string),
                levels: None,
            },
        );
        let asked = t.elapsed();

        // Forced separately: the tree never needs it, a rebuild restoring view 2's cursor does.
        let t = std::time::Instant::now();
        let rows = gmuse::browse::Holding::len(&remote);
        let ordering = t.elapsed();

        let said = remote.said();
        println!(
            "{:<28} {:>10.1?} {:>10.1?}  {rows}{}",
            source.unwrap_or("(the whole library)"),
            asked,
            ordering,
            if said.problems.is_empty() {
                String::new()
            } else {
                format!("   said: {}", said.problems.join("; "))
            }
        );
    }
}
