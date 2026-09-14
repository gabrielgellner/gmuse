//! **What pairing recordings to copies costs when the library is elsewhere** — #261.
//!
//! `read_the_queue` and a restored session both arrive holding recordings and draw copies.
//! Measured against a running engine, for a queue-sized ask and for the whole library.
fn main() {
    use gmuse::model::rows::Listing;

    let socket = gmuse::engine::socket::default_path();
    let client = std::sync::Arc::new(
        gmuse_client::Client::connect(&socket, "measure", "0")
            .expect("an engine to be listening — start one with `gmuse engine`"),
    );
    let remote = gmuse::ui::holding_remote::Remote::open(client);

    let ids = remote.rows();
    assert!(ids.len() > 100, "only {} rows", ids.len());

    // The recordings a queue names, taken from the library so they are real.
    let some: Vec<_> = ids
        .iter()
        .take(110)
        .filter_map(|id| remote.recording_of(*id))
        .collect();
    assert!(some.len() > 50, "only {} recordings", some.len());

    let t = std::time::Instant::now();
    let got = remote.copies_of(&some);
    println!(
        "copies_of, a queue of {:>5}:  {:>9.1?}   {} paired",
        some.len(),
        t.elapsed(),
        got.len()
    );

    let all: Vec<_> = ids
        .iter()
        .filter_map(|id| remote.recording_of(*id))
        .collect();
    let t = std::time::Instant::now();
    let got = remote.copies_of(&all);
    println!(
        "copies_of, the whole {:>5}:  {:>9.1?}   {} paired",
        all.len(),
        t.elapsed(),
        got.len()
    );
}
