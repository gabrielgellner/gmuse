//! **What an attached view's tick does to a hand-queued entry** — #301.
//!
//! Written to catch a suspect that turned out to be innocent, and kept because the property
//! it checks is one nothing else states: `keep_up` runs `push_the_queue` on every tick, and
//! `queue.set` is `Ask::Play`, which is `replace` — so a mirror that drifted out of step
//! would silently swap the whole programme for one page and take every `by_hand` entry with
//! it. That is exactly the reported symptom, and it is *not* what happens: `read_the_queue`
//! sets `ui.queue_mirror` on every normal read, so the guard holds.
//!
//! The real cause was in the engine — a queue edit resynced to the head, so the window that
//! would have played the queued track was replaced by the programme's first entry.
//!
//! Run it against a live engine while something is playing. The `by_hand` index must not move
//! across ticks, and `total` must not collapse to the page size.
//!
fn main() {
    let socket = gmuse::engine::socket::default_path();
    let client = std::sync::Arc::new(
        gmuse_client::Client::connect(&socket, "view", "0").expect("an engine to be listening"),
    );
    let other = gmuse_client::Client::connect(&socket, "other", "0").expect("connect");

    // **The engine's view, not a desk's** — a read-only `Desk` here reports no holdings for
    // rows `shelf query` can locate, which is #298 and would make this measure nothing.
    let reader = gmuse::ui::holding_remote::Remote::open(std::sync::Arc::clone(&client));
    let ctx = gmuse::ui::Context {
        library: &reader,
        player: None,
        gapless: None,
    };

    let look = |tag: &str| {
        let a = other.call("queue.get", None).expect("queue.get");
        let hands: Vec<usize> = a["by_hand"]
            .as_array()
            .map(|v| {
                v.iter()
                    .enumerate()
                    .filter(|(_, h)| h.as_bool() == Some(true))
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
        println!(
            "{tag:<26} version={:<5} total={:<6} page={:<4} by_hand at {:?}",
            a["version"],
            a["total"],
            a["entries"].as_array().map(|e| e.len()).unwrap_or(0),
            hands
        );
    };

    let mut ui = gmuse::ui::Ui::new(Default::default());
    ui.engine = Some(std::sync::Arc::clone(&client));

    look("before anything");
    gmuse::ui::read_the_queue(&mut ui, &ctx);
    look("after read_the_queue");

    // The row `E` would queue: whatever the library holds for the first Kongos recording.
    let row = gmuse::browse::Holding::sorted(&reader)
        .iter()
        .copied()
        .find(|id| {
            gmuse::model::rows::Listing::listed(&reader, *id)
                .map(|l| l.title.contains("Come With Me Now"))
                .unwrap_or(false)
        })
        .expect("a row to queue");

    gmuse::ui::enqueue_with_the_engine(&mut ui, &ctx, &[row], true, None);
    gmuse::ui::read_the_queue(&mut ui, &ctx);
    look("after E");

    // **One tick of the loop an attached view actually runs.**
    for n in 1..=3 {
        gmuse::ui::keep_up(&mut ui, &ctx);
        look(&format!("after keep_up #{n}"));
    }
}
