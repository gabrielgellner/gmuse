//! **What the info panel's history costs, step by step** — the K-over-an-artist lag.
//!
//! An attached view holds no records (#261), so `Desk::history` reads the log from disk and
//! then `facts_about` walks every record for the focus. Measured against the real library.
fn main() {
    let data = std::path::PathBuf::from(std::env::args().nth(1).expect("pass the data dir"));
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());
    let now = gmuse::history::now_ms();

    let projection = gmuse::librarian::reading(&data, device, now);
    let library = gmuse::model::seam::Librarian::from_catalogue(projection.catalogue());

    // An artist with plenty of credits: the one with the most recordings.
    let mut credited: std::collections::BTreeMap<_, usize> = Default::default();
    for c in &library.reference.credits {
        *credited.entry(c.artist.id()).or_default() += 1;
    }
    let artist = credited
        .iter()
        .max_by_key(|(_, n)| **n)
        .map(|(a, _)| *a)
        .expect("an artist");
    let focus = gmuse::model::focus::Focus::Artist(artist);

    let t = std::time::Instant::now();
    let (records, _) = gmuse::model::durable::read_all(&data);
    let parsed = t.elapsed();
    assert!(
        records.len() > 10_000,
        "{} records — nothing measured",
        records.len()
    );

    let t = std::time::Instant::now();
    let happened = library.facts_about(&records, focus);
    let walked = t.elapsed();

    // **What the engine does instead now**: grep the text for the id, parse the matches.
    let t = std::time::Instant::now();
    let mentioned = gmuse::model::durable::mentioning(&data, &focus.uuid().to_string());
    let grepped = t.elapsed();
    let t = std::time::Instant::now();
    let same = library.facts_about(&mentioned, focus);
    let walked_few = t.elapsed();
    assert_eq!(
        same.len(),
        happened.len(),
        "the grep found a different history"
    );
    println!(
        "  mentioning (grep the log)  {grepped:>9.1?}   {} lines parsed",
        mentioned.len()
    );
    println!("  facts_about over those     {walked_few:>9.1?}");

    let t = std::time::Instant::now();
    let about = gmuse::model::rows::Listing::about(
        &library,
        focus,
        gmuse::model::about::History::Shown(&happened),
    );
    let assembled = t.elapsed();

    println!(
        "{} records, artist focus {:?}",
        records.len(),
        library.reference.artists[&artist].name
    );
    println!("  read_all (parse the log)   {parsed:>9.1?}");
    println!(
        "  facts_about (walk it)      {walked:>9.1?}   {} facts about the focus",
        happened.len()
    );
    println!(
        "  about (assemble the panel) {assembled:>9.1?}   {} sections",
        about.sections.len()
    );
    println!(
        "  = one K press              {:>9.1?}",
        parsed + walked + assembled
    );

    // **And then every frame.** `info_about` hands back `about.clone()` on each call and
    // the draw path calls it at least twice a frame, so a panel with thousands of history
    // rows pays this on every keystroke and every tick.
    let t = std::time::Instant::now();
    let copies: Vec<_> = (0..20).map(|_| about.clone()).collect();
    let cloned = t.elapsed() / copies.len() as u32;
    assert_eq!(copies.len(), 20);
    let rows: usize = about.sections.iter().map(|s| s.rows.len()).sum();
    println!("  about.clone(), per call    {cloned:>9.1?}   {rows} rows — twice a frame");
}
