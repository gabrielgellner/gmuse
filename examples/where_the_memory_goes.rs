//! **What the engine holds, step by step** — #255.
//!
//! `Projected` measured 121.8 MB in a library test, and the engine's resident set is 390 MB.
//! This is the difference, attributed rather than guessed: the same steps the engine takes at
//! startup, each one measured by the accounting allocator.
//!
//! Run against the real library: `cargo run --release --example where_the_memory_goes -- <dir>`
use gmuse_library::accounting::{self, mb};

#[global_allocator]
static ACCOUNTING: accounting::Accounting = accounting::Accounting;

/// **The bytes a fact's `String`s own**, which `size_of` does not count.
///
/// Serialised and measured, rather than a match over every variant: `Fact` has dozens, the
/// question is an order of magnitude, and a match that forgot a variant would answer low and
/// look right. JSON is longer than the strings it quotes, so this is an over-estimate — which
/// is the safe direction for deciding that interning is *not* the lever.
fn weigh(fact: &gmuse::facts::Fact) -> usize {
    serde_json::to_string(fact)
        .map(|text| text.bytes().filter(|b| *b != b'"').count())
        .unwrap_or(0)
}

/// This process's resident set, in MB.
///
/// **Shelled out rather than an FFI call**, because `task_info` needs `unsafe` and the project
/// allows two of those, neither here (§12). A `ps` per step is noise next to a fold.
fn rss_mb() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output();
    out.ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|t| t.trim().parse::<f64>().ok())
        .map(|kb| kb / 1024.0)
        .unwrap_or(0.0)
}

fn main() {
    let data = std::path::PathBuf::from(std::env::args().nth(1).expect("pass the data dir"));
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());

    println!(
        "{:<34} {:>9} {:>9} {:>9} {:>9}",
        "step", "kept MB", "peak MB", "live MB", "RSS MB"
    );
    println!("{}", "-".repeat(74));

    // **`live` and `RSS` side by side, because they answer different questions.** The
    // allocator says what the program is holding; the kernel says what it has not given back.
    // Reasoning about one from the other is what this issue was opened for.
    let say = |what: &str, kept: usize, peak: usize| {
        println!(
            "{what:<34} {:>9.1} {:>9.1} {:>9.1} {:>9.1}",
            mb(kept),
            mb(peak),
            mb(accounting::live()),
            rss_mb()
        );
    };

    // 1. The log, as the engine parses it.
    let (log, kept, peak) =
        accounting::measure(|| gmuse::model::instance::Instance::open(&data, device, 0).0);
    say("Instance::open (the records)", kept, peak);
    let records = log.records().len();
    assert!(
        records > 1_000,
        "only {records} records — this measures nothing"
    );

    // 2. The store, as the launch scan builds it. **The real roots**, because with none it
    //    is zero bytes and the measurement says nothing — which the first run of this did.
    let roots: Vec<std::path::PathBuf> = std::env::args()
        .skip(2)
        .map(std::path::PathBuf::from)
        .collect();
    assert!(!roots.is_empty(), "pass the music roots after the data dir");
    let (held, kept, peak) = accounting::measure(|| {
        let mut holdings = gmuse::operations::scan::Holdings::new();
        let report = gmuse::operations::scan::scan(&mut holdings, &roots);
        let _ = report;
        holdings
    });
    say("Holdings (the launch scan)", kept, peak);
    assert!(held.len() > 1_000, "the scan found {} files", held.len());

    // 2b. What the scan costs a second time, which is what `Job::Scan` clones.
    let (clone, kept, peak) = accounting::measure(|| held.clone());
    say("  a clone of it, per scan", kept, peak);
    drop(clone);

    // 3. The fold.
    let (answer, kept, peak) = accounting::measure(|| {
        let mut projected = gmuse::projection::Projected::default();
        projected.advance(log.records());
        projected
    });
    say("Projected::advance (the fold)", kept, peak);

    // 4. The librarian a view reads from, built off the catalogue.
    let (library, kept, peak) =
        accounting::measure(|| gmuse::model::seam::Librarian::from_catalogue(answer.catalogue()));
    say("Librarian::from_catalogue", kept, peak);

    // 4b. What one more published `Answer` costs.
    //
    // **Its own `Librarian` and nothing else.** The catalogue and the holdings are `Arc`s
    // shared with the projection, not copies — the first version of this measured
    // `catalogue().clone()` and got a no-op, because `Catalogue` is not `Clone` and that
    // cloned the reference. `noop_method_call` said so; the number would have been wrong in
    // the flattering direction.
    let (published, kept, peak) = accounting::measure(|| {
        (
            gmuse::model::seam::Librarian::from_catalogue(answer.catalogue()),
            std::sync::Arc::clone(&std::sync::Arc::new(held.clone())),
        )
    });
    say("one more retained Answer", kept, peak);

    // 5. A second fold — what a `library` event used to cost, and what an extra retained
    //    `Answer` costs now.
    let (second, kept, peak) = accounting::measure(|| {
        let mut projected = gmuse::projection::Projected::default();
        projected.advance(log.records());
        gmuse::model::seam::Librarian::from_catalogue(projected.catalogue())
    });
    say("a second fold, retained", kept, peak);

    println!("{}", "-".repeat(56));
    println!(
        "{:<34} {:>9.1} MB live   {:>9.1} MB RSS",
        "at the end",
        mb(accounting::live()),
        rss_mb()
    );
    println!(
        "{:<34} {:>9.1}x",
        "RSS over live",
        rss_mb() / mb(accounting::live())
    );
    println!("{:<34} {:>9}", "records", records);
    println!("{:<34} {:>9}", "holdings", held.len());

    // **What an attached view holds now** — #261. `Desk::read_only` follows the projection
    // snapshot, so it parses no log and keeps no records.
    let (follower, kept, peak) =
        accounting::measure(|| gmuse::librarian::reading(&data, device, 0));
    say("a follower's fold (the snapshot)", kept, peak);
    assert!(
        !follower.catalogue().recordings.is_empty(),
        "the snapshot folded nothing, so the number above means nothing"
    );

    // **What a view would hold if it kept only what it draws** — the question behind #261's
    // Goal 1: a view needs *rows*, not records and not a catalogue.
    let (drawn, kept, peak) = accounting::measure(|| {
        use gmuse::model::rows::Listing;
        library
            .rows()
            .into_iter()
            .filter_map(|id| library.listed(id).map(|l| l.owned()))
            .collect::<Vec<_>>()
    });
    say("every row a view can draw", kept, peak);
    assert!(drawn.len() > 1_000, "only {} rows drawn", drawn.len());
    println!(
        "{:<34} {:>9} rows, {:>6.1} MB",
        "  = what a view actually needs",
        drawn.len(),
        mb(kept)
    );

    // **What the browse tree costs** — #261. `ui.library` is a `Shelf`, built from the fold by
    // `Shelf::grouped`. If it is cheap, a view can hold one and drop the fold; if it is not,
    // the tree has to be paged too.
    let (shelf, kept, peak) = accounting::measure(|| {
        use gmuse::model::rows::Listing;
        let members: std::collections::HashSet<_> = library.rows().into_iter().collect();
        gmuse::shelf::Shelf::from_members(&library, members, true)
    });
    say("the browse tree (a Shelf)", kept, peak);
    assert!(shelf.sorted().len() > 1_000, "the shelf is empty");

    // **Where a record's bytes are** — #256 proposes an incremental fold to stop holding them;
    // that is a second implementation of a 672-line multi-pass fold. Worth knowing first
    // whether the bytes are the *structure* or the *strings* in it, because interning is a much
    // smaller change than refolding incrementally.
    let mut strings = 0usize;
    let mut count = 0usize;
    for record in log.records() {
        count += 1;
        strings += weigh(&record.fact);
    }
    let structs = count * std::mem::size_of::<gmuse::facts::Record>();
    println!();
    println!(
        "{:<34} {:>9}",
        "a Record, by size_of",
        std::mem::size_of::<gmuse::facts::Record>()
    );
    println!(
        "{:<34} {:>9.1} MB",
        "  x records (the structure)",
        mb(structs)
    );
    println!("{:<34} {:>9.1} MB", "  strings inside them", mb(strings));
    println!("{:<34} {:>9.1} MB", "  = accounted", mb(structs + strings));

    // **Which variant sets the size**, because an enum is as big as its widest one and every
    // record pays for it. Boxing a fat variant is a much smaller change than refolding
    // incrementally, so it is worth knowing whether there is one.
    println!();
    println!(
        "{:<34} {:>9}",
        "Stamp",
        std::mem::size_of::<gmuse::model::clock::Stamp>()
    );
    println!(
        "{:<34} {:>9}",
        "Fact",
        std::mem::size_of::<gmuse::facts::Fact>()
    );
    // **How many of each kind**, which is what decides whether boxing the widest variant is a
    // win at all: an enum is as wide as its widest variant, but boxing one that is also the
    // *most common* trades 112 inline bytes for a pointer plus a heap allocation, which is
    // worse.
    let mut how_many: std::collections::BTreeMap<String, usize> = Default::default();
    for record in log.records() {
        let debug = format!("{:?}", record.fact);
        let kind = debug
            .split([' ', '(', '{'])
            .next()
            .unwrap_or("?")
            .to_owned();
        *how_many.entry(kind).or_default() += 1;
    }
    let mut ranked: Vec<(usize, String)> = how_many.into_iter().map(|(k, n)| (n, k)).collect();
    ranked.sort_by_key(|(n, _)| std::cmp::Reverse(*n));
    println!();
    println!("{:<34} {:>9}", "the commonest facts", "count");
    for (n, kind) in ranked.iter().take(8) {
        println!("  {kind:<32} {n:>9}");
    }

    // Held so nothing above is dropped before the total.
    let _ = (&log, &held, &answer, &library, &second, &published);
}
