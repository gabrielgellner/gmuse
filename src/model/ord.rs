//! A position in an ordered collection, under a log that merges by union.
//!
//! `domain-model.md` §"Collections" calls for a **fractional index**: a key
//! strictly between its neighbours, so inserting between two entries touches
//! no other row. This is the concrete encoding, and #92 asked for it to be
//! decided here rather than discovered later — a durable fact format is not
//! something to get wrong twice.
//!
//! ## Why not a dense integer
//!
//! The obvious encoding is `position: u32`, and it is wrong for both of this
//! model's reasons. Inserting between two entries rewrites every row after it,
//! so a one-item insert becomes an n-row fact storm. And two devices inserting
//! at the same place both pick the same number, so the merge collides on every
//! subsequent position with no way to tell which list the reader meant.
//!
//! ## Why not a plain fraction either
//!
//! The first draft here was a bare base-36 fraction — `"5"` is 5/36, and
//! lexicographic `<` on the digits is `<` on the values. It is correct and it
//! is unusable, because **the room above a key shrinks as the key grows**. A
//! thousand appends produced a 167-digit key; stepping by one digit instead of
//! bisecting brought that to 30, which is the same problem wearing a smaller
//! number. It is not a tuning failure: a fraction in `[0, 1)` has finitely
//! much space above any point in it, and appending is the operation a playlist
//! does most.
//!
//! ## What this is
//!
//! The encoding the fractional-indexing literature settled on: an **unbounded
//! integer part** with a length-prefixed head, and an optional fraction after
//! it. Appending increments the integer, which is O(1) digits amortised and
//! never runs out; inserting between two neighbours that are adjacent integers
//! falls back to bisecting the fraction, which is where growth belongs.
//!
//! ```text
//! a0    the first key            head 'a' — one integer digit
//! a1    …then appending          integer 0 → 1
//! az    …35 appends later
//! b10   overflow into two digits head 'b', integer 10₃₆
//! Zz    prepending below a0      head 'Z' — one digit, negative side
//! a0V   between a0 and a1        the fraction, where bisection lives
//! ```
//!
//! A head of `a`–`z` means a positive integer of 1–26 digits; `A`–`Z` means a
//! negative one, longest first. `'A' < 'Z' < 'a' < 'z'` in ASCII, so the heads
//! sort in value order and plain lexicographic comparison on the whole string
//! is comparison on the value. `BTreeMap<OrdKey, _>` is therefore already in
//! list order, and the fold needs no comparator of its own.
//!
//! **A fraction never ends in the lowest digit.** Without that, nothing could
//! be placed between `"a1"` and `"a10"` — every candidate would need a digit
//! below zero — and there would be arrangements of keys with a gap that cannot
//! be filled. The rule is about the fraction and not the whole key: `"a0"` is
//! the first key and ends in one, which is fine, because what goes below an
//! integer is another integer.
//!
//! ## What it does not do, and what covers that
//!
//! Two devices inserting at the same place produce the **same** key, because
//! both compute it from the same neighbours. That is deliberate: an `OrdKey`
//! orders entries and does not identify them. `CollectionEntry` is keyed by
//! `(collection, recording)` and carries the stamp that asserted it, so equal
//! keys break by the asserting device — a deterministic total order after
//! merge, and the same one on every machine. Encoding a device into the key
//! instead would make one list read differently depending on who typed it.
//!
//! ## What it costs, measured
//!
//! | | |
//! | --- | --- |
//! | 10,000 appends | longest key **4 digits** |
//! | 1,000 inserts at random places | longest **≤ 12** |
//! | 1,000 inserts at the *same* place | **169** |
//!
//! The third is not a defect and no encoding avoids it: a thousand successive
//! midpoints between a fixed pair are a thousand halvings, and a base-36 digit
//! buys 5.2 of them. It is the case `domain-model.md` means by compaction —
//! rewriting the keys of one collection locally and syncing the result as
//! ordinary field updates.

#![allow(
    dead_code,
    reason = "the encoding is decided before the facts that will carry it (#92) — `Collection` and `CollectionEntry` are #76, and this is what their `ord` field will be. Tested to the point where those can be written against it rather than around it"
)]

use std::fmt;

/// The digits, lowest first. Base 36 because it is dense enough that a key
/// stays short and plain enough that a person reading a log line can compare
/// two by eye.
const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
const BASE: u8 = 36;

/// The lowest digit, which a key may never end in — see the module docs.
const LOW: u8 = 0;

/// A position in an ordered collection.
///
/// Ordered by plain lexicographic comparison, which is what lets it be a
/// `BTreeMap` key and what keeps the fold free of a comparator.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct OrdKey(String);

impl OrdKey {
    /// The key for the first entry in an empty collection.
    pub fn first() -> Self {
        Self("a0".to_string())
    }

    /// A key strictly between `before` and `after`.
    ///
    /// `None` on either side means no neighbour that way — the start or the
    /// end of the collection. Both `None` is an empty collection.
    pub fn between(before: Option<&OrdKey>, after: Option<&OrdKey>) -> Self {
        match (before, after) {
            (None, None) => Self::first(),
            // Appending and prepending step the **integer**, which is what
            // makes them cheap: there is unbounded room either way, so neither
            // needs to touch a fraction at all.
            (Some(a), None) => Self(step(integer(&a.0), 1)),
            (None, Some(b)) => Self(step(integer(&b.0), -1)),
            (Some(a), Some(b)) => Self(mid(&a.0, &b.0)),
        }
    }

    /// How long the key is, for a compaction pass to decide it has grown.
    #[allow(
        dead_code,
        reason = "compaction is named in domain-model.md and nothing rewrites keys yet (#92)"
    )]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Display for OrdKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// How many digits follow this head.
///
/// `a`–`z` count up from one, `Z`–`A` count up from one going down, so a more
/// negative value has a head that sorts earlier.
fn width(head: u8) -> usize {
    if head.is_ascii_lowercase() {
        (head - b'a') as usize + 1
    } else {
        (b'Z' - head) as usize + 1
    }
}

/// The integer part of a key: its head and that many digits.
fn integer(key: &str) -> &str {
    let head = key.as_bytes()[0];
    &key[..1 + width(head)]
}

/// The fraction after the integer part, which may be empty.
fn fraction(key: &str) -> &str {
    &key[integer(key).len()..]
}

/// The integer one step up or down, widening the head when it overflows.
///
/// **The reason this encoding has a head at all.** A fraction runs out of room
/// above itself; an integer does not, and the head is what lets it grow
/// without breaking lexicographic order — a longer positive integer has a
/// later head, so it sorts after every shorter one.
fn step(int: &str, by: i8) -> String {
    let head = int.as_bytes()[0];
    let mut digits: Vec<u8> = int[1..].bytes().map(value).collect();

    let carried = if by > 0 {
        bump(&mut digits)
    } else {
        drop_one(&mut digits)
    };
    if !carried {
        return render(head, &digits);
    }

    // Overflowed this width. Widen by one and start at the far end of the new
    // range, which is adjacent to where we overflowed from.
    match (by > 0, head.is_ascii_lowercase()) {
        // z→…→ next head up, integer restarting at its lowest.
        (true, true) => render(head + 1, &vec![LOW; width(head + 1)]),
        // Climbing out of the negatives: 'A'…'Z' then 'a'.
        (true, false) if head == b'Z' => render(b'a', &[LOW]),
        (true, false) => render(head + 1, &vec![LOW; width(head + 1)]),
        // Descending below the lowest positive integer: into 'Z'.
        (false, true) if head == b'a' => render(b'Z', &[BASE - 1]),
        (false, true) => render(head - 1, &vec![BASE - 1; width(head - 1)]),
        (false, false) => render(head - 1, &vec![BASE - 1; width(head - 1)]),
    }
}

/// Add one, low digit last. `true` when it carried off the end.
fn bump(digits: &mut [u8]) -> bool {
    for d in digits.iter_mut().rev() {
        if *d + 1 < BASE {
            *d += 1;
            return false;
        }
        *d = LOW;
    }
    true
}

/// Subtract one. `true` when it borrowed off the end.
fn drop_one(digits: &mut [u8]) -> bool {
    for d in digits.iter_mut().rev() {
        if *d > LOW {
            *d -= 1;
            return false;
        }
        *d = BASE - 1;
    }
    true
}

/// A key strictly between two others.
///
/// Two cases. Their integers may already have room between them, in which case
/// the answer is an integer and there is no fraction to grow. Otherwise they
/// are adjacent, and the answer bisects the **fraction** — which is where
/// growth belongs, because it is bounded by the neighbour above.
fn mid(a: &str, b: &str) -> String {
    let (ia, ib) = (integer(a), integer(b));
    if ia != ib {
        let next = step(ia, 1);
        // A gap of more than one integer: take the next one, and it is
        // strictly between because `b` starts no earlier than the one after.
        if next.as_str() < ib {
            return next;
        }
        // Adjacent integers. `a`'s fraction has unbounded room above it, so a
        // key below `b` is `a`'s integer with a larger fraction — unless `b`
        // itself has a fraction, in which case bisect below that.
        if fraction(b).is_empty() {
            return format!("{ia}{}", grow(fraction(a), None));
        }
        return format!("{ib}{}", grow("", Some(fraction(b))));
    }
    format!("{ia}{}", grow(fraction(a), Some(fraction(b))))
}

/// A fraction strictly between two fractions of the same integer.
///
/// **Bisect when both ends are pinned, step when one is not** — the same
/// distinction the integer part makes, one level down, and it was got wrong
/// here first. `mid` reaches this with no upper bound whenever the two
/// neighbours are adjacent integers, which is the ordinary "insert just after
/// this row" case; bisecting there halves the room above on every insert, so
/// the key grows a digit every six. Stepping grows one every thirty-five.
fn grow(a: &str, b: Option<&str>) -> String {
    let (a, b) = (a.as_bytes(), b.map(str::as_bytes));
    let mut out: Vec<u8> = Vec::new();
    let mut at = 0usize;
    loop {
        let lo = a.get(at).map_or(LOW, |c| value(*c));
        let hi = match b {
            Some(b) if at < b.len() => value(b[at]),
            // Past the end of the upper bound there is none at this position:
            // anything longer that starts with it is already greater.
            _ => BASE,
        };
        if hi > lo + 1 {
            out.push(if b.is_some() {
                lo + (hi - lo) / 2
            } else {
                lo + 1
            });
            return digits(&out);
        }
        // No room here. Take the lower digit — a prefix of the upper bound
        // sorts before it — and look one place deeper.
        out.push(lo);
        at += 1;
    }
}

fn value(c: u8) -> u8 {
    DIGITS
        .iter()
        .position(|d| *d == c)
        .unwrap_or_else(|| panic!("an OrdKey holds only its own digits, not {:?}", c as char))
        as u8
}

fn digits(values: &[u8]) -> String {
    values.iter().map(|v| DIGITS[*v as usize] as char).collect()
}

fn render(head: u8, values: &[u8]) -> String {
    format!("{}{}", head as char, digits(values))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> OrdKey {
        OrdKey(s.to_string())
    }

    /// **The one property everything else rests on.**
    #[test]
    fn a_key_falls_strictly_between_its_neighbours() {
        let a = OrdKey::first();
        let c = OrdKey::between(Some(&a), None);
        let b = OrdKey::between(Some(&a), Some(&c));
        assert!(a < b && b < c, "{a} < {b} < {c}");
    }

    /// **Appending is the operation a playlist does most, and it must stay
    /// short.**
    ///
    /// The whole reason this encoding has an integer part. A bare fraction
    /// gave 167 digits for a thousand appends, and 30 after stepping instead
    /// of bisecting — a fraction in `[0, 1)` simply has finitely much room
    /// above any point in it.
    #[test]
    fn appending_a_thousand_times_stays_short() {
        let mut last = OrdKey::first();
        for n in 0..1_000 {
            let next = OrdKey::between(Some(&last), None);
            assert!(next > last, "append {n}: {next} did not follow {last}");
            last = next;
        }
        assert!(
            last.len() <= 4,
            "a thousand appends should not need {} digits: {last}",
            last.len()
        );
    }

    /// And prepending, which is the mirror and goes through the negative heads.
    #[test]
    fn prepending_a_thousand_times_stays_short() {
        let mut first = OrdKey::first();
        for n in 0..1_000 {
            let next = OrdKey::between(None, Some(&first));
            assert!(next < first, "prepend {n}: {next} did not precede {first}");
            first = next;
        }
        assert!(
            first.len() <= 4,
            "a thousand prepends should not need {} digits: {first}",
            first.len()
        );
    }

    /// **No arrangement of keys leaves a gap that cannot be filled.**
    ///
    /// Splitting the same interval over and over is the adversarial case, and
    /// it has to keep answering rather than run out. This is where growth is
    /// expected: both ends are pinned, so there is nowhere to go but longer.
    #[test]
    fn an_interval_can_be_split_forever() {
        let lo0 = OrdKey::first();
        let hi = OrdKey::between(Some(&lo0), None);
        let mut lo = lo0;
        for n in 0..200 {
            let mid = OrdKey::between(Some(&lo), Some(&hi));
            assert!(
                lo < mid && mid < hi,
                "round {n}: {lo} < {mid} < {hi} failed"
            );
            lo = mid;
        }
        // **And it steps rather than bisects, where there is no upper bound.**
        //
        // Both are strictly between, so betweenness cannot tell them apart —
        // the cost can. These splits are against a neighbour one integer away,
        // so the fraction has unbounded room above it: a step of one digit
        // buys 35 inserts per digit and bisecting buys six. Getting that wrong
        // is what made a thousand appends 167 digits, one level up.
        assert!(
            lo.len() < 12,
            "200 splits took {} digits, which is bisecting where it should step: {lo}",
            lo.len()
        );
    }

    /// And from the other side, which walks a different branch.
    #[test]
    fn an_interval_can_be_split_forever_from_above() {
        let lo = OrdKey::first();
        let mut hi = OrdKey::between(Some(&lo), None);
        for n in 0..200 {
            let mid = OrdKey::between(Some(&lo), Some(&hi));
            assert!(
                lo < mid && mid < hi,
                "round {n}: {lo} < {mid} < {hi} failed"
            );
            hi = mid;
        }
        // **And here it must bisect**, which is the mirror of the assertion
        // above and the reason the choice is a choice rather than a constant.
        //
        // Inserting just *below* the newest key each time crowds the fraction
        // downward. Bisecting halves what is left and costs a digit every six;
        // stepping up from the fixed lower bound returns almost the same key
        // every time, so it costs a digit every insert.
        assert!(
            hi.len() < 60,
            "200 splits from above took {} digits, which is stepping where it \
             should bisect: {hi}",
            hi.len()
        );
    }

    /// **The order a `BTreeMap` gives is the order the list is in.**
    ///
    /// Not an implementation detail: it is why the fold needs no comparator
    /// and why a collection can be read straight out of the map.
    #[test]
    fn lexicographic_order_is_the_list_order() {
        let mut list: Vec<(OrdKey, &str)> = Vec::new();
        let push = |list: &mut Vec<(OrdKey, &str)>, at: usize, what: &'static str| {
            let before = at.checked_sub(1).and_then(|i| list.get(i)).map(|e| &e.0);
            let after = list.get(at).map(|e| &e.0);
            let k = OrdKey::between(before, after);
            list.insert(at, (k, what));
        };
        push(&mut list, 0, "one");
        push(&mut list, 1, "three");
        push(&mut list, 1, "two");
        push(&mut list, 0, "zero");
        push(&mut list, 4, "four");

        let mut sorted = list.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            sorted.iter().map(|e| e.1).collect::<Vec<_>>(),
            ["zero", "one", "two", "three", "four"],
            "sorting the keys did not give the list back"
        );
    }

    /// **A thousand insertions at random places, and the keys still say what
    /// the list is.**
    ///
    /// The cases above each exercise one path; this one crosses them. A
    /// deterministic shuffle rather than a random one, so a failure is a
    /// failure somebody can reproduce.
    #[test]
    fn a_list_built_by_inserting_anywhere_reads_back_in_order() {
        let mut list: Vec<(OrdKey, usize)> = Vec::new();
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        for n in 0..1_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let at = if list.is_empty() {
                0
            } else {
                (seed % (list.len() as u64 + 1)) as usize
            };
            let before = at.checked_sub(1).and_then(|i| list.get(i)).map(|e| &e.0);
            let after = list.get(at).map(|e| &e.0);
            let k = OrdKey::between(before, after);
            if let Some(b) = before {
                assert!(&k > b, "insert {n} at {at}: {k} did not follow {b}");
            }
            if let Some(a) = after {
                assert!(&k < a, "insert {n} at {at}: {k} did not precede {a}");
            }
            list.insert(at, (k, n));
        }

        let mut sorted = list.clone();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(
            sorted.iter().map(|e| e.1).collect::<Vec<_>>(),
            list.iter().map(|e| e.1).collect::<Vec<_>>(),
            "the keys did not give back the list they were built for"
        );
        let longest = list.iter().map(|e| e.0.len()).max().unwrap_or(0);
        assert!(longest <= 12, "a key grew to {longest} digits: {list:?}");
    }

    /// **A key never ends in the lowest digit**, whatever it was asked for.
    ///
    /// Without it nothing could be placed between `a10` and `a1`, since every
    /// candidate would need a digit below zero.
    #[test]
    fn no_fraction_ends_in_the_lowest_digit() {
        let a = OrdKey::first();
        let b = OrdKey::between(Some(&a), None);
        let mut lo = a;
        for _ in 0..60 {
            let mid = OrdKey::between(Some(&lo), Some(&b));
            let f = fraction(&mid.0);
            assert!(
                f.is_empty() || !f.ends_with(DIGITS[LOW as usize] as char),
                "{mid} ends in the lowest digit, so nothing can go below it"
            );
            lo = mid;
        }
    }

    /// The first key leaves room on both sides, so an empty collection can be
    /// grown either way.
    #[test]
    fn the_first_key_leaves_room_on_both_sides() {
        let only = OrdKey::first();
        assert!(OrdKey::between(None, Some(&only)) < only, "no room before");
        assert!(OrdKey::between(Some(&only), None) > only, "no room after");
    }

    /// **Two devices inserting at the same place agree on the key**, which is
    /// deliberate: an `OrdKey` orders entries and does not identify them.
    #[test]
    fn the_same_insertion_point_gives_the_same_key_on_two_devices() {
        let a = OrdKey::first();
        let b = OrdKey::between(Some(&a), None);
        assert_eq!(
            OrdKey::between(Some(&a), Some(&b)),
            OrdKey::between(Some(&a), Some(&b)),
        );
    }

    /// **A gap left by a removal is filled with an integer, not a fraction.**
    ///
    /// The case a list reaches by deleting from the middle: the neighbours are
    /// several integers apart, and there is a whole key sitting between them
    /// already. Reaching for a fraction there would grow the key for no reason
    /// and leave the integer unused for ever.
    #[test]
    fn a_gap_between_distant_neighbours_is_filled_with_an_integer() {
        let mut keys = vec![OrdKey::first()];
        for _ in 0..4 {
            let last = keys.last().unwrap().clone();
            keys.push(OrdKey::between(Some(&last), None));
        }
        // a0 a1 a2 a3 a4 — now drop the middle three, as a removal would.
        let (lo, hi) = (keys[0].clone(), keys[4].clone());

        let filled = OrdKey::between(Some(&lo), Some(&hi));
        assert!(lo < filled && filled < hi, "{lo} < {filled} < {hi}");
        assert_eq!(
            filled.len(),
            lo.len(),
            "it grew a fraction where an integer was free: {filled}"
        );
        assert_eq!(filled, keys[1], "and it is the next integer along");
    }

    /// The head widens rather than the order breaking, in both directions.
    #[test]
    fn the_integer_widens_when_a_head_runs_out() {
        assert_eq!(step("az", 1), "b00", "the positive side did not widen");
        assert_eq!(step("a0", -1), "Zz", "it did not cross into the negatives");
        assert_eq!(step("Zz", 1), "a0", "and it did not climb back out");
        assert!(key("Zz") < key("a0") && key("az") < key("b00"));
    }
}

#[cfg(test)]
mod measured {
    //! What the keys actually cost, so a claim about "short" has a number.

    use super::*;

    #[test]
    #[ignore = "a measurement, not an assertion; run deliberately"]
    fn what_a_thousand_of_each_costs() {
        let mut last = OrdKey::first();
        let mut appended = Vec::new();
        for _ in 0..10_000 {
            last = OrdKey::between(Some(&last), None);
            appended.push(last.len());
        }
        eprintln!(
            "append   10,000 → longest {} digits, last {last}",
            appended.iter().max().unwrap()
        );

        let a = OrdKey::first();
        let b = OrdKey::between(Some(&a), None);
        let mut lo = a;
        for _ in 0..1_000 {
            lo = OrdKey::between(Some(&lo), Some(&b));
        }
        eprintln!("bisect    1,000 → {} digits, {lo}", lo.len());
    }
}
