//! **The ordered list of what will play, and the rule for what follows** — #257.
//!
//! `docs/design/playback-programme.md` is the decision. The short of it: the librarian
//! owns an ordered, editable, versioned **programme**, and the audio engine holds a
//! **window** of two or three entries and no ordering policy at all.
//!
//! ## A mode is a rule answered here, not state the engine holds
//!
//! Every playback mode collapses into one function — `follows` — and the engine cannot
//! tell which is in force:
//!
//! | mode             | `follows(token)`                         |
//! | ---------------- | ---------------------------------------- |
//! | `OnceThrough`    | the next entry, or **nothing** at the end |
//! | `RepeatAll`      | the next entry, or **the first**          |
//! | `RepeatCurrent`  | **the same entry**                        |
//!
//! Which is why `RepeatCurrent` is survivable. A one-item circular programme would
//! *replace* the programme and lose your place; this leaves the programme untouched and
//! only changes what a refill answers, so turning it off resumes in order for free.
//!
//! ## Why an entry has its own identity
//!
//! A programme can hold the same recording twice — a disc that plays one recording in two
//! places, a compilation carrying two versions. `shelf copies` and `shelf slots` exist
//! because that shape is real. So `follows(row)` would be ambiguous given `[A, B, A, C]`,
//! and the stateless cursor the refill protocol depends on could not work at all.
//!
//! **An `EntryToken` names a position's occupant, not the recording.** It is minted here
//! and opaque to everything else, which is also what lets `gmuse-engine` report which
//! entry finished while naming no library type.
//!
//! ## Why a reshuffle is a reorder
//!
//! The same rows in a new order keep their tokens ([`Programme::reorder`]), so the entry
//! that is sounding stays valid and the new order simply follows it. **The common case
//! needs no resync** — a person toggling shuffle does not expect the current track to
//! stop. [`Programme::replace`] is the other case, a genuinely different set of rows, and
//! that one does invalidate every token the engine holds.

use gmuse_ids::copy::CopyId;

/// Which entry, as against which recording.
///
/// **Opaque, and minted only by a [`Programme`].** Monotonic for the programme's whole
/// life and never reused, so a token from an older version cannot silently match a
/// different entry — it fails to match at all, which is the answer the refill protocol
/// wants (`StaleVersion`, and a fresh window).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct EntryToken(u64);

impl EntryToken {
    /// The number on the wire. **Nothing may derive meaning from it** beyond equality —
    /// it is not an index, and arithmetic on it is a bug.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// One position in the programme, and what occupies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub token: EntryToken,
    pub row: CopyId,
}

/// What happens when an entry ends.
///
/// **`RepeatCurrent` is not `deck.loop`.** A loop's unit is a time range inside a
/// sounding track (`DeckLoop { start: When, beats: u16 }`), and "repeat bars 33–40"
/// cannot be an entry in a list — so a loop stays with the audio engine, where the RT
/// path is. The two coincide at exactly one setting and belong on opposite sides of the
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Mode {
    /// Play to the end and stop.
    #[default]
    OnceThrough,
    /// Wrap to the first entry. **A property of the sequence**, which is why it lives
    /// here rather than as a flag anywhere else.
    RepeatAll,
    /// Answer every refill with the entry that is already playing.
    RepeatCurrent,
}

impl Mode {
    /// For a status line and a format string.
    pub fn label(self) -> &'static str {
        match self {
            Mode::OnceThrough => "once",
            Mode::RepeatAll => "repeat-all",
            Mode::RepeatCurrent => "repeat-one",
        }
    }
}

/// A monotonically increasing count of mutations.
///
/// **Carried in a refill request so a stale position is detected rather than guessed
/// at.** Not `projection::Position`, which is a per-device map of stamps: that is a
/// vector clock for sync identity, and §8.7 says in as many words that is not what goes
/// on this wire.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct Version(u64);

impl Version {
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// The ordered list of what will play.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Programme {
    entries: Vec<Entry>,
    mode: Mode,
    version: Version,
    /// The next token to hand out. **Never reset**, including by [`Programme::replace`],
    /// which is what makes a token from an older version fail to match rather than
    /// matching the wrong entry.
    minting: u64,
}

impl Programme {
    /// An empty programme, mode `OnceThrough`.
    pub fn new() -> Self {
        Self::default()
    }

    /// A programme over these rows, in this order.
    pub fn of(rows: impl IntoIterator<Item = CopyId>) -> Self {
        let mut it = Self::new();
        it.replace(rows);
        it
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    pub fn version(&self) -> Version {
        self.version
    }

    /// Change what happens at the end of an entry.
    ///
    /// **Bumps the version, and touches no entry.** That is the whole point: the engine's
    /// window is replaced from the new answer, and the programme a person is looking at is
    /// exactly as it was.
    pub fn set_mode(&mut self, mode: Mode) {
        if self.mode != mode {
            self.mode = mode;
            self.bump();
        }
    }

    /// Where this token sits, if it still does.
    pub fn position(&self, token: EntryToken) -> Option<usize> {
        self.entries.iter().position(|e| e.token == token)
    }

    pub fn entry(&self, token: EntryToken) -> Option<Entry> {
        self.position(token).map(|at| self.entries[at])
    }

    /// **What follows this entry, under the mode in force.** The one function every
    /// playback mode collapses into.
    ///
    /// `None` means playback stops: the end of a `OnceThrough` programme, an empty one,
    /// or a token this programme no longer holds — and the last of those is the caller's
    /// cue to send a fresh window rather than to guess.
    pub fn follows(&self, token: EntryToken) -> Option<Entry> {
        let at = self.position(token)?;
        match self.mode {
            Mode::RepeatCurrent => Some(self.entries[at]),
            Mode::RepeatAll => {
                // `position` succeeded, so `entries` is non-empty and the wrap is safe.
                Some(self.entries[(at + 1) % self.entries.len()])
            }
            Mode::OnceThrough => self.entries.get(at + 1).copied(),
        }
    }

    /// The first `n` entries, for a programme that has not started playing.
    pub fn head(&self, n: usize) -> Vec<Entry> {
        let Some(first) = self.entries.first().copied() else {
            return Vec::new();
        };
        let mut out = vec![first];
        self.extend_from(&mut out, first.token, n);
        out
    }

    /// The `n` entries after this one — **the refill answer**.
    ///
    /// Empty when the token is unknown, which is the stale-position case, or when a
    /// `OnceThrough` programme has run out.
    pub fn after(&self, token: EntryToken, n: usize) -> Vec<Entry> {
        if self.position(token).is_none() {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.extend_from(&mut out, token, n);
        out
    }

    /// Walk `follows` until `out` holds `n`, or until it says stop.
    ///
    /// **The bound is `n`, not the programme's length**, because `RepeatAll` and
    /// `RepeatCurrent` are deliberately infinite — a window of three over a one-entry
    /// repeating programme is three copies of it, and that is correct.
    fn extend_from(&self, out: &mut Vec<Entry>, from: EntryToken, n: usize) {
        let mut at = from;
        while out.len() < n {
            match self.follows(at) {
                Some(next) => {
                    at = next.token;
                    out.push(next);
                }
                None => break,
            }
        }
    }

    /// **The same rows in a new order, keeping every token** — a reshuffle.
    ///
    /// Returns `false` and changes nothing unless `order` is a permutation of the tokens
    /// already held. That refusal is deliberate: a reorder that silently dropped an entry
    /// would invalidate a window without saying so, and the caller wanted [`replace`].
    ///
    /// [`replace`]: Programme::replace
    pub fn reorder(&mut self, order: &[EntryToken]) -> bool {
        if order.len() != self.entries.len() {
            return false;
        }
        let mut moved = Vec::with_capacity(order.len());
        for token in order {
            match self.entry(*token) {
                Some(entry) if !moved.iter().any(|e: &Entry| e.token == *token) => {
                    moved.push(entry);
                }
                // Unknown, or named twice.
                _ => return false,
            }
        }
        self.entries = moved;
        self.bump();
        true
    }

    /// **A different set of rows, and every old token stops matching.**
    ///
    /// The stale-position case the refill protocol exists for: an engine holding a token
    /// from before gets an empty `after`, and the answer is a fresh window rather than a
    /// guess.
    pub fn replace(&mut self, rows: impl IntoIterator<Item = CopyId>) {
        self.entries = rows.into_iter().map(|row| self.mint(row)).collect();
        self.bump();
    }

    /// Put these rows in at `at`, clamped to the end.
    pub fn insert(&mut self, at: usize, rows: impl IntoIterator<Item = CopyId>) {
        let at = at.min(self.entries.len());
        let fresh: Vec<Entry> = rows.into_iter().map(|row| self.mint(row)).collect();
        if fresh.is_empty() {
            return;
        }
        self.entries.splice(at..at, fresh);
        self.bump();
    }

    pub fn push(&mut self, rows: impl IntoIterator<Item = CopyId>) {
        self.insert(usize::MAX, rows);
    }

    /// Take these entries out. Unknown tokens are ignored.
    pub fn remove(&mut self, tokens: &[EntryToken]) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| !tokens.contains(&e.token));
        let gone = before - self.entries.len();
        if gone > 0 {
            self.bump();
        }
        gone
    }

    pub fn clear(&mut self) {
        if !self.entries.is_empty() {
            self.entries.clear();
            self.bump();
        }
    }

    /// A fresh entry. **`minting` only ever goes up**, across a `replace` too.
    fn mint(&mut self, row: CopyId) -> Entry {
        let token = EntryToken(self.minting);
        self.minting += 1;
        Entry { token, row }
    }

    fn bump(&mut self) {
        self.version = Version(self.version.0 + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmuse_ids::copy::row_of_test as row;

    fn tokens(entries: &[Entry]) -> Vec<u64> {
        entries.iter().map(|e| e.token.as_u64()).collect()
    }

    /// **The same recording twice, and `follows` tells the two apart** — the case that
    /// makes a token necessary rather than tidy.
    ///
    /// `[A, B, A, C]` is a real programme: a disc that plays one recording in two places,
    /// a compilation carrying two versions. A cursor keyed by the *row* could not answer
    /// "what follows this A", so the refill protocol would have to hold an index and
    /// drift. Keyed by entry it is exact.
    #[test]
    fn one_recording_held_twice_is_two_entries() {
        let p = Programme::of([row(1), row(2), row(1), row(3)]);

        // The instrument: both entries really do name the same row, or this proves
        // nothing about ambiguity at all.
        let first = p.entries()[0];
        let third = p.entries()[2];
        assert_eq!(first.row, third.row, "the fixture did not repeat a row");
        assert_ne!(first.token, third.token, "one row minted one token");

        assert_eq!(p.follows(first.token).map(|e| e.row), Some(row(2)));
        assert_eq!(p.follows(third.token).map(|e| e.row), Some(row(3)));
    }

    /// **Each mode's answer at the end, which is where they differ.**
    #[test]
    fn the_three_modes_answer_differently_at_the_end() {
        let mut p = Programme::of([row(1), row(2)]);
        let last = p.entries()[1].token;

        p.set_mode(Mode::OnceThrough);
        assert_eq!(p.follows(last), None, "once-through ran past the end");

        p.set_mode(Mode::RepeatAll);
        assert_eq!(
            p.follows(last).map(|e| e.row),
            Some(row(1)),
            "repeat-all did not wrap"
        );

        p.set_mode(Mode::RepeatCurrent);
        assert_eq!(
            p.follows(last).map(|e| e.token),
            Some(last),
            "repeat-current did not answer itself"
        );
    }

    /// **Repeat-current leaves the programme alone, so turning it off resumes in order.**
    ///
    /// This is the whole argument for a rule over a one-item circular programme, and it is
    /// the assertion that would fail if `set_mode` ever started rewriting entries.
    #[test]
    fn repeat_current_does_not_disturb_the_programme() {
        let mut p = Programme::of([row(1), row(2), row(3)]);
        let before = p.entries().to_vec();
        let second = p.entries()[1].token;

        p.set_mode(Mode::RepeatCurrent);
        assert_eq!(p.follows(second).map(|e| e.token), Some(second));
        assert_eq!(
            p.entries(),
            before.as_slice(),
            "the programme was rewritten"
        );

        p.set_mode(Mode::OnceThrough);
        assert_eq!(
            p.follows(second).map(|e| e.row),
            Some(row(3)),
            "the place was lost"
        );
    }

    /// **A window under repeat-current is the same entry repeated**, not one entry.
    ///
    /// `extend_from` is bounded by `n` rather than by the programme's length precisely so
    /// this works; bounding it by length would hand the engine a window of one and make it
    /// ask again at every track end.
    #[test]
    fn a_repeating_window_is_as_long_as_asked_for() {
        let mut p = Programme::of([row(1)]);

        p.set_mode(Mode::RepeatCurrent);
        let one = p.entries()[0].token;
        assert_eq!(p.after(one, 3).len(), 3);

        p.set_mode(Mode::RepeatAll);
        assert_eq!(
            p.after(one, 3).len(),
            3,
            "one entry repeating is still a stream"
        );

        p.set_mode(Mode::OnceThrough);
        assert!(p.after(one, 3).is_empty(), "once-through invented entries");
    }

    /// **An empty repeating programme terminates**, which a `while` over `follows` need
    /// not do by itself.
    #[test]
    fn an_empty_repeating_programme_does_not_hang() {
        let mut p = Programme::new();
        p.set_mode(Mode::RepeatAll);
        assert!(p.head(5).is_empty());
        assert!(p.after(EntryToken(0), 5).is_empty());
    }

    /// **A reshuffle keeps your place**, because the same rows in a new order keep their
    /// tokens.
    ///
    /// The behaviour this buys: toggling shuffle does not stop the track that is playing.
    /// The engine's held token still resolves, and the new order simply follows it.
    #[test]
    fn a_reorder_keeps_every_token_valid() {
        let mut p = Programme::of([row(1), row(2), row(3)]);
        let playing = p.entries()[0].token;
        let held = tokens(p.entries());

        let shuffled: Vec<EntryToken> = [held[1], held[0], held[2]]
            .into_iter()
            .map(EntryToken)
            .collect();
        assert!(p.reorder(&shuffled));

        assert_eq!(
            p.entry(playing).map(|e| e.row),
            Some(row(1)),
            "the token went stale"
        );
        assert_eq!(
            p.follows(playing).map(|e| e.row),
            Some(row(3)),
            "the new order was not followed"
        );
    }

    /// **A reorder that is not a permutation changes nothing**, rather than silently
    /// dropping an entry and invalidating a window without saying so.
    #[test]
    fn a_reorder_refuses_anything_but_a_permutation() {
        let mut p = Programme::of([row(1), row(2), row(3)]);
        let held = tokens(p.entries());
        let at = p.version();

        let short: Vec<EntryToken> = held[..2].iter().copied().map(EntryToken).collect();
        assert!(!p.reorder(&short), "a short order was accepted");

        let doubled: Vec<EntryToken> = vec![held[0], held[0], held[1]]
            .into_iter()
            .map(EntryToken)
            .collect();
        assert!(!p.reorder(&doubled), "a repeated token was accepted");

        assert_eq!(
            tokens(p.entries()),
            held,
            "a refused reorder still moved things"
        );
        assert_eq!(p.version(), at, "a refused reorder bumped the version");
    }

    /// **A replace invalidates every token, and never reuses one.**
    ///
    /// Reusing would be worse than invalidating: a stale token would match a *different*
    /// entry, and the engine would play the wrong thing rather than asking for a fresh
    /// window.
    #[test]
    fn a_replace_mints_tokens_that_never_collide_with_the_old() {
        let mut p = Programme::of([row(1), row(2)]);
        let old = tokens(p.entries());

        p.replace([row(1), row(2)]);
        let new = tokens(p.entries());

        assert!(
            !old.is_empty() && !new.is_empty(),
            "nothing was minted either time"
        );
        for token in &old {
            assert!(!new.contains(token), "token {token} was handed out twice");
            assert!(
                p.entry(EntryToken(*token)).is_none(),
                "an old token still resolves"
            );
            assert!(
                p.after(EntryToken(*token), 2).is_empty(),
                "an old token still refills"
            );
        }
    }

    /// **Every mutation bumps the version, and a no-op does not.**
    ///
    /// The version is what a refill request carries, so a bump that did not happen is a
    /// stale window nobody notices, and one that happens for nothing is a needless
    /// resync.
    #[test]
    fn the_version_tracks_real_change_only() {
        let mut p = Programme::new();
        let start = p.version().as_u64();

        p.push([row(1), row(2)]);
        p.insert(1, [row(3)]);
        p.set_mode(Mode::RepeatAll);
        let held = tokens(p.entries());
        p.remove(&[EntryToken(held[0])]);
        assert_eq!(p.version().as_u64(), start + 4, "a mutation did not bump");

        let steady = p.version();
        p.set_mode(Mode::RepeatAll);
        p.remove(&[EntryToken(9_999)]);
        p.insert(0, std::iter::empty());
        p.clear();
        p.clear();
        assert_eq!(
            p.version().as_u64(),
            steady.as_u64() + 1,
            "only the first clear was real change"
        );
    }

    /// **`head` includes the first entry**, and `after` excludes the one it is given —
    /// the two halves of a window, which must not overlap or the engine plays a track
    /// twice.
    #[test]
    fn head_starts_the_window_and_after_continues_it() {
        let p = Programme::of([row(1), row(2), row(3)]);
        let opening = p.head(2);
        assert_eq!(opening.len(), 2);
        assert_eq!(opening[0].row, row(1));

        let more = p.after(opening[1].token, 1);
        assert_eq!(more.len(), 1);
        assert_eq!(
            more[0].row,
            row(3),
            "after() repeated an entry it was given"
        );
    }
}
