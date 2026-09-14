//! **What a row costs when the library is somewhere else** — #261.
//!
//! `Listing::listed` answers one row, and a filter reads one per row of the whole library. Over
//! a socket that is a round trip each, and the question is whether that is a keystroke or a
//! wait. Measured against a running engine.
fn main() {
    let socket = gmuse::engine::socket::default_path();
    let client = std::sync::Arc::new(
        gmuse_client::Client::connect(&socket, "measure", "0")
            .expect("an engine to be listening — start one with `gmuse engine`"),
    );
    let remote = gmuse::ui::holding_remote::Remote::open(client);

    use gmuse::model::rows::Listing;
    let ids = remote.rows();
    assert!(
        ids.len() > 100,
        "only {} rows — this measures nothing",
        ids.len()
    );
    println!("{} rows", ids.len());

    let some: Vec<_> = ids.iter().take(40).copied().collect();
    let t = std::time::Instant::now();
    let got = some.iter().filter_map(|id| remote.listed(*id)).count();
    let screenful = t.elapsed();
    assert!(got > 0, "no rows came back — the engine answered nothing");
    println!(
        "listed() over a screenful ({got}):  {:>9.1?}  {:>7.0?} each",
        screenful,
        screenful / got as u32
    );

    // What a filter does: one per row of the whole library.
    let t = std::time::Instant::now();
    let all = ids.iter().filter_map(|id| remote.listed(*id)).count();
    println!(
        "listed() over every row ({all}):  {:>9.1?}  what a `:view` with terms costs",
        t.elapsed()
    );
}
