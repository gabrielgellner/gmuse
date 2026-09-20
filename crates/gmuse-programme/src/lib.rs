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

use std::path::PathBuf;

use gmuse_ids::copy::CopyId;

/// cmus's "all / artist / album" narrowing. Applies to the library only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum Scope {
    #[default]
    All,
    /// Stay within the current **outer** key of the grouping tree.
    Outer,
    /// Stay within the current **inner** key.
    Inner,
}

impl Scope {
    pub fn cycle(self) -> Self {
        match self {
            Scope::All => Scope::Outer,
            Scope::Outer => Scope::Inner,
            Scope::Inner => Scope::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Scope::All => "all",
            Scope::Outer => "outer",
            Scope::Inner => "inner",
        }
    }

    /// The inverse of [`Scope::label`], and the home of the cmus spellings.
    ///
    /// **`artist` and `album` are accepted for ever** — they are what a saved
    /// session and a `config.toml` on disk say, and a rename must not stop a
    /// config file working (#234). They are aliases rather than synonyms: they
    /// meant "the outer key" and "the inner key" all along, which is only
    /// obvious once the grouping is something other than artist and album.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "all" => Scope::All,
            "outer" | "artist" => Scope::Outer,
            "inner" | "album" => Scope::Inner,
            _ => return None,
        })
    }
}

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

    /// A token that came back from a caller.
    ///
    /// **Not a constructor for a new entry** — only [`Programme`] mints those. A number
    /// this programme never handed out simply fails to match, which is the answer the
    /// refill protocol wants.
    pub fn from_u64(raw: u64) -> Self {
        Self(raw)
    }
}

/// One position in the programme, and what occupies it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub token: EntryToken,
    pub row: CopyId,
    /// **Put here by a person, not by the pool** — what `e` means.
    ///
    /// Under the two-tier model a queue was only ever what somebody chose,
    /// and "add to the queue" meant *after those, before the pool resumes*.
    /// One list has no seam between the two unless the entries say which
    /// they are, so an enqueue marks its rows and [`Programme::enqueue`]
    /// lands after the run of marked entries that follows what is sounding.
    /// Rows a play gesture or a reshuffle put in are not marked.
    #[serde(default)]
    pub by_hand: bool,
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Programme {
    entries: Vec<Entry>,
    mode: Mode,
    /// **How the order was made, so a view can draw the toggles.**
    ///
    /// Descriptive rather than operative: nothing consults these to decide what follows —
    /// `follows` reads the list. They are here because a view drawing a shuffle button has
    /// to know whether it is on, and the librarian is the actor that knows.
    ///
    /// §6 calls `scope` control state "because it determines what plays next". Under this
    /// design it determines what the *programme* is, once, when one is built — so the
    /// reason it lived in the engine no longer holds.
    scope: Scope,
    shuffled: bool,
    /// **The rows the programme was drawn from, when a view narrowed them.**
    ///
    /// A person who presses play inside `in=General` expects General to play,
    /// and the pool used to be whatever their view held. Now the librarian
    /// builds the pool and it needs telling — so this is the member set the
    /// view resolved to at the moment of the open, kept beside `scope` and
    /// `shuffled` because it is the same kind of thing: a standing rule about
    /// what a refill may draw from. `None` is the whole library.
    ///
    /// A set and not the view's text, because only the socket handler can
    /// resolve a view (it holds the fold and the parser) and the engine must
    /// not. Not persisted: after a restart a rescope draws from everything
    /// until the next play from a view says otherwise.
    #[serde(skip)]
    within: Option<std::sync::Arc<std::collections::HashSet<CopyId>>>,
    version: Version,
    /// The next token to hand out. **Never reset**, including by [`Programme::replace`],
    /// which is what makes a token from an older version fail to match rather than
    /// matching the wrong entry.
    minting: u64,
}

/// **Version 1, because 0 means "no version".**
///
/// A view asserts `version > 0` to mean "a version was read rather than defaulted", which is
/// the only way to tell an answer from an absent one over a wire that fills in zeros. An empty
/// programme is still a programme and has a version; nothing has one before it exists.
impl Default for Programme {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            mode: Mode::default(),
            scope: Scope::default(),
            shuffled: false,
            within: None,
            version: Version(1),
            minting: 0,
        }
    }
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

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn shuffled(&self) -> bool {
        self.shuffled
    }

    /// Record how an order was made. **Called by whoever made it**, not by a person
    /// toggling a switch — the toggle is a reorder, and this says what the reorder was.
    pub fn made_by(
        &mut self,
        scope: Scope,
        shuffled: bool,
        within: Option<std::sync::Arc<std::collections::HashSet<CopyId>>>,
    ) {
        self.scope = scope;
        self.shuffled = shuffled;
        self.within = within;
    }

    /// The rows a refill may draw from, if a view narrowed them — see the field.
    pub fn within(&self) -> Option<std::sync::Arc<std::collections::HashSet<CopyId>>> {
        self.within.clone()
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

    /// The **first** entry holding this row.
    ///
    /// First, because a programme may hold one recording twice and a caller naming a row
    /// has not said which occurrence it means. Anything that needs to be exact holds a
    /// token — which is what tokens are for.
    pub fn first_of(&self, row: CopyId) -> Option<Entry> {
        self.entries.iter().copied().find(|e| e.row == row)
    }

    /// A window starting **at** this entry rather than after it.
    ///
    /// What a play gesture wants: the track a person picked is the first thing to sound, so
    /// it is the first cue rather than something already playing.
    pub fn window_at(&self, token: EntryToken, n: usize) -> Vec<Entry> {
        let Some(first) = self.entry(token) else {
            return Vec::new();
        };
        let mut out = vec![first];
        self.extend_from(&mut out, token, n);
        out
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

    /// **What precedes this entry, under the mode in force** — the mirror of [`follows`].
    ///
    /// `RepeatCurrent` answers itself, as forwards does. `RepeatAll` wraps to the last
    /// entry, which is what makes `prev` from the first track land on the last.
    ///
    /// [`follows`]: Programme::follows
    pub fn precedes(&self, token: EntryToken) -> Option<Entry> {
        let at = self.position(token)?;
        match self.mode {
            Mode::RepeatCurrent => Some(self.entries[at]),
            Mode::RepeatAll => {
                let last = self.entries.len() - 1;
                Some(self.entries[if at == 0 { last } else { at - 1 }])
            }
            Mode::OnceThrough => match at {
                0 => None,
                _ => self.entries.get(at - 1).copied(),
            },
        }
    }

    /// The `n` entries before this one, nearest first.
    pub fn before(&self, token: EntryToken, n: usize) -> Vec<Entry> {
        if self.position(token).is_none() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut at = token;
        while out.len() < n {
            match self.precedes(at) {
                Some(prev) => {
                    at = prev.token;
                    out.push(prev);
                }
                None => break,
            }
        }
        out
    }

    /// **Reorder to put these rows in this order, keeping every token.**
    ///
    /// For a shuffle toggle: the caller computes an order of *rows* — shuffled, or the
    /// shelf's — and the entries follow it. Rows the order does not mention keep their
    /// relative position at the end, so an entry put in by hand is never dropped.
    pub fn reorder_by_row(&mut self, rows: &[CopyId]) {
        let mut rank: std::collections::HashMap<CopyId, usize> = std::collections::HashMap::new();
        for (at, row) in rows.iter().enumerate() {
            rank.entry(*row).or_insert(at);
        }
        let unknown = rows.len();
        // Stable, so two entries holding one row keep their order relative to each other.
        self.entries
            .sort_by_key(|e| *rank.get(&e.row).unwrap_or(&unknown));
        self.bump();
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

    /// **Forget what was queued by hand**, and leave the programme — what
    /// `:clear-queue` means under one list. Returns how many went.
    pub fn clear_queued(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| !e.by_hand);
        let gone = before - self.entries.len();
        if gone > 0 {
            self.bump();
        }
        gone
    }

    /// A fresh entry. **`minting` only ever goes up**, across a `replace` too.
    fn mint(&mut self, row: CopyId) -> Entry {
        let token = EntryToken(self.minting);
        self.minting += 1;
        Entry {
            token,
            row,
            by_hand: false,
        }
    }

    /// **Queue rows relative to what is sounding** — the `e` and `E` of a player.
    ///
    /// `anchor` is the entry that is playing, or `None` when nothing is; the
    /// insertion is after it. `Placement::Next` lands immediately after it;
    /// `Placement::Queued` lands after the run of by-hand entries that
    /// follows it — so pressing `e` three times plays those three in order,
    /// and then the programme resumes where it was; `Placement::End` is the
    /// end. Everything inserted is marked by hand.
    ///
    /// An anchor the programme no longer holds is treated as nothing playing,
    /// for the reason a page from a stale token starts at the head: the
    /// caller cannot tell, and refusing would lose the rows.
    pub fn enqueue(
        &mut self,
        anchor: Option<EntryToken>,
        rows: impl IntoIterator<Item = CopyId>,
        at: Placement,
    ) {
        let after = anchor
            .and_then(|t| self.entries.iter().position(|e| e.token == t))
            .map_or(0, |i| i + 1);
        let index = match at {
            Placement::Next => after,
            Placement::Queued => {
                let mut i = after;
                while i < self.entries.len() && self.entries[i].by_hand {
                    i += 1;
                }
                i
            }
            Placement::End => self.entries.len(),
        };
        let fresh: Vec<Entry> = rows
            .into_iter()
            .map(|row| Entry {
                by_hand: true,
                ..self.mint(row)
            })
            .collect();
        if fresh.is_empty() {
            return;
        }
        self.entries.splice(index..index, fresh);
        self.bump();
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

    /// **A partial order keeps what it does not mention, at the end.**
    ///
    /// `reorder_by_row`'s fallback, tested directly because no `Ask` exercises it: a
    /// shuffle derives its order from the entries, so every one is always mentioned. The
    /// fallback is what makes that safe to get wrong — and a guard nothing tests is a guard
    /// that is not there.
    #[test]
    fn reordering_by_a_partial_row_order_keeps_the_rest() {
        let mut p = Programme::of([row(1), row(2), row(3)]);
        let held: Vec<u64> = p.entries().iter().map(|e| e.token.as_u64()).collect();

        // Only two of the three named, reversed.
        p.reorder_by_row(&[row(3), row(1)]);

        let after: Vec<CopyId> = p.entries().iter().map(|e| e.row).collect();
        assert_eq!(p.len(), 3, "an unmentioned entry was dropped");
        assert_eq!(after, vec![row(3), row(1), row(2)], "the order is wrong");
        for token in held {
            assert!(
                p.entry(EntryToken::from_u64(token)).is_some(),
                "a token was lost to a partial reorder"
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

// ── What the librarian and the engine say to each other ─────────────────────

/// Where an enqueue lands, relative to what is sounding — see [`Programme::enqueue`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Placement {
    /// Immediately after what is playing.
    Next,
    /// After what was already queued by hand, before the programme resumes.
    /// The default, because it is what "add to the queue" has always meant.
    #[default]
    Queued,
    /// The end of the programme.
    End,
}

impl Placement {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "next" => Some(Self::Next),
            "queued" => Some(Self::Queued),
            "end" => Some(Self::End),
            _ => None,
        }
    }
}

/// What to do to the programme, or ask of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// **Start over on these rows**, and cue the first `window` of them.
    ///
    /// Every token the caller held stops matching, which is the honest answer for a
    /// genuinely different list.
    Play {
        rows: Vec<CopyId>,
        window: usize,
        base: Option<u64>,
    },
    /// **Build a programme from the library and cue from one row** — a play gesture.
    ///
    /// The caller names a row, a narrowing, and whether to shuffle; the librarian
    /// materialises the pool, orders it, and cues from `at`. **The pool never crosses**,
    /// which is the point of the ask existing: scope `All` is 7,271 rows and a caller has
    /// no business holding them.
    ///
    /// `at` absent cues from the head — "play this album" rather than "play this track".
    Open {
        at: Option<CopyId>,
        scope: Scope,
        shuffle: bool,
        /// The view's rows, resolved by whoever could — see `Programme::within`.
        within: Option<std::sync::Arc<std::collections::HashSet<CopyId>>>,
        /// Seeds the permutation. **The same seed shuffles the same pool the same way**, so
        /// a reshuffle is a new seed rather than a hidden `rand` call, and a test can assert
        /// an order.
        seed: u64,
        window: usize,
    },
    /// **Put these rows at the end** — what `queue.enqueue` is now.
    ///
    /// A programme is one list, so "play next" and "add to the queue" are both an insert at
    /// a position. `base` is #233's optimistic concurrency: two views reordering at once
    /// *is* a lost update, so a mutation carries the version it was computed against.
    Push {
        rows: Vec<CopyId>,
        base: Option<u64>,
    },
    /// Queue rows relative to what is sounding — [`Programme::enqueue`].
    Enqueue {
        rows: Vec<CopyId>,
        /// The sounding entry's token, from the engine; `None` when nothing plays.
        anchor: Option<u64>,
        at: Placement,
        base: Option<u64>,
    },
    /// **Take out the entry at this index** — what `queue.remove` is.
    ///
    /// By index because that is what the wire says (§7.4) and a view counts rows. **A token
    /// would be better** and `Ask::Remove` is it: an index races, which is exactly why
    /// `base` has to be checked. Changing the wire is a protocol decision, not a refactor.
    RemoveAt {
        index: usize,
        base: Option<u64>,
    },
    /// **Play this recording, and open a programme around it.**
    ///
    /// For the callers that name a recording rather than a programme position: a view saying
    /// "play this", and a session resume.
    ///
    /// **It opens a programme, and that is load-bearing rather than tidy.** `Ask::Load` used
    /// to load and touch nothing, which worked only because the old design had an *implicit*
    /// programme — the whole library, always, walked from a cursor. With an explicit one,
    /// loading without opening leaves the window empty and playback stops after one track.
    /// Two integration tests said so, which is what they are for.
    ///
    /// The narrowing and shuffle are whatever the programme already had, so pressing play on
    /// a track does not silently reset them.
    Start {
        recording: gmuse_ids::id::Id<gmuse_ids::entity::Recording>,
        /// The view the play came from, resolved — see `Programme::within`.
        within: Option<std::sync::Arc<std::collections::HashSet<CopyId>>>,
        window: usize,
    },
    /// **A window from the *end* of the programme, changing nothing.**
    ///
    /// What `prev` asks with nothing playing. `next::step` walked the pool backwards from
    /// "whichever end we are walking from" when there was no current track, and this is that
    /// rule said about a list.
    Tail {
        n: usize,
    },
    /// **A window from the top of the programme, changing nothing** — a resync.
    ///
    /// Distinct from `Open`, which rebuilds the pool. A resync means *the programme is
    /// right and my copy of it is not*, so nothing may move.
    Head {
        n: usize,
    },
    /// **The `n` cues after this token** — the refill.
    ///
    /// `version` is what the caller last saw. A mismatch is not by itself an error: the
    /// token is looked up regardless, because a reorder keeps tokens and the caller's
    /// position is still good. The version is reported back so a caller can tell a
    /// reorder from a replace.
    After {
        token: u64,
        n: usize,
    },
    /// **The `n` cues before this token** — what `prev` asks.
    ///
    /// The programme walked backwards. `next::prev` walked the *pool* backwards and never
    /// read the queue, which under one list is the same thing said more simply.
    Before {
        token: u64,
        n: usize,
    },
    /// **Shuffle on or off, keeping every token** — so the track playing keeps playing.
    ///
    /// Off restores the shelf's order: who made it, then the release, then the track.
    /// Entries put in by hand sort into place rather than being dropped, which is the only
    /// answer that does not lose an edit.
    Shuffle {
        on: bool,
        seed: u64,
    },
    /// **Narrow or widen the pool, anchored at an entry the caller holds.**
    ///
    /// A scope is relative to what is playing, and only the caller knows which entry that
    /// is — so it passes the token rather than the librarian guessing. Every old token
    /// stops matching, because this is a different set of rows.
    Rescope {
        at: Option<u64>,
        scope: Scope,
        shuffle: bool,
        within: Option<std::sync::Arc<std::collections::HashSet<CopyId>>>,
        seed: u64,
        window: usize,
    },
    /// The same rows in a new order, keeping every token. A reshuffle.
    Reorder {
        order: Vec<u64>,
    },
    /// What happens at the end of an entry.
    SetMode(Mode),
    Insert {
        at: usize,
        rows: Vec<CopyId>,
    },
    Remove {
        tokens: Vec<u64>,
    },
    Clear {
        base: Option<u64>,
        /// Only the by-hand entries — see [`Programme::clear_queued`].
        queued_only: bool,
    },
    /// **A page of the programme, for drawing it.**
    ///
    /// **A page, and not the whole thing, because the whole thing is the play order.** Under
    /// the two-tier model this returned "what you explicitly queued" — usually nothing, or
    /// three tracks. One list makes the programme the play order, and the play order after a
    /// play gesture at scope `all` is 7,271 entries: a pane drawing all of it draws your
    /// library, which is never the question being asked.
    ///
    /// `from` is where the page starts, and `None` means the head. A caller that knows what
    /// is sounding passes that token, which makes the default view "up next".
    Look {
        from: Option<u64>,
        limit: usize,
    },
}

impl Ask {
    /// The version this mutation was computed against, if it names one.
    ///
    /// **Exhaustive with no catch-all**, so a new mutating variant does not compile until
    /// somebody has decided whether it needs a guard. A `_ => None` would make "no guard"
    /// the silent default, which is the failure this whole file keeps naming.
    /// Whether applying this changes the programme, so a caller knows to resync.
    pub fn mutating(&self) -> bool {
        match self {
            Ask::Play { .. }
            | Ask::Open { .. }
            | Ask::Push { .. }
            | Ask::Enqueue { .. }
            | Ask::RemoveAt { .. }
            | Ask::Clear { .. }
            | Ask::Reorder { .. }
            | Ask::Shuffle { .. }
            | Ask::Rescope { .. }
            | Ask::SetMode(_)
            | Ask::Insert { .. }
            | Ask::Remove { .. } => true,
            Ask::Start { .. }
            | Ask::Tail { .. }
            | Ask::Head { .. }
            | Ask::After { .. }
            | Ask::Before { .. }
            | Ask::Look { .. } => false,
        }
    }

    /// **`pub` because the guard that reads it lives with the fold**, not here — the
    /// librarian checks it before applying anything, and this crate has nothing to apply to.
    pub fn base(&self) -> Option<u64> {
        match self {
            Ask::Play { base, .. }
            | Ask::Push { base, .. }
            | Ask::Enqueue { base, .. }
            | Ask::RemoveAt { base, .. }
            | Ask::Clear { base, .. } => *base,
            // Reads, and the asks whose whole job is to replace what a caller holds.
            Ask::Open { .. }
            | Ask::Start { .. }
            | Ask::Tail { .. }
            | Ask::Head { .. }
            | Ask::After { .. }
            | Ask::Before { .. }
            | Ask::Reorder { .. }
            | Ask::Shuffle { .. }
            | Ask::Rescope { .. }
            | Ask::SetMode(_)
            | Ask::Insert { .. }
            | Ask::Remove { .. }
            | Ask::Look { .. } => None,
        }
    }
}

/// Where to find one entry's audio, and what the tags say about its loudness.
#[derive(Debug, Clone, PartialEq)]
pub struct Cued {
    /// The programme entry. **Opaque**: hand it back, do not read it.
    pub token: u64,
    pub path: PathBuf,
    /// **Which recording is sounding**, for the `playback` event to name.
    ///
    /// The one library identity a cue carries, and it is unavoidable: a view identifies the
    /// playing track by recording — `gp` jumps to it, the info panel follows it — and the
    /// engine is the only actor that knows what is sounding. Not the row: two rows can hold
    /// one recording, and what a listener is hearing is the recording.
    pub recording: gmuse_ids::id::Id<gmuse_ids::entity::Recording>,
    /// **Which copy this is**, because a play is evidence about what was heard.
    ///
    /// The librarian chose it — a pin, or which device holds what — so the librarian is what
    /// can say which. The engine could not work it out from a path without hashing the file,
    /// which §4.4 forbids on that thread.
    pub encoding: gmuse_ids::id::Id<gmuse_ids::entity::Encoding>,
    /// **How long the library says it is**, which is not the same question as how long this
    /// file is (#77).
    ///
    /// It is the same number today and stops being one the moment another machine holds a
    /// longer copy of a rip that was cut short here — at which point "did I hear enough of
    /// it" should be asked of the song rather than of this device's copy.
    pub length: std::time::Duration,
    /// The ReplayGain tags, in dB, and nothing done with them.
    ///
    /// **How loud to play is the engine's question**, because it is the half that holds
    /// the person's gain preference; what the tags say is the library's. `Playable`'s own
    /// comment has drawn that line since #78, and `source_for` plays at unity today
    /// precisely because the cue carried no way to say this.
    pub track_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_db: Option<f32>,
    pub album_peak: Option<f32>,
}

/// One entry, for a surface that draws the programme rather than plays it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    pub token: u64,
    pub row: CopyId,
    /// Queued by a person — see `Entry::by_hand`.
    pub by_hand: bool,
}

/// **What is true of the programme, carried by every reply** — #259.
///
/// So a caller never has to *ask* for the version or the toggles: every answer refreshes
/// them. That is what lets the engine hold them as cached state instead of sending a `Look`
/// before each mode change and once per publish — and an ask the engine does not send is an
/// ask that cannot block it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Standing {
    pub version: u64,
    pub mode: Mode,
    /// How the order was made — what a view draws its toggles from.
    pub scope: Scope,
    pub shuffled: bool,
    /// How many entries the programme holds.
    pub total: usize,
}

/// What came back.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Cues to play, in order, and the version they were taken from.
    ///
    /// **Shorter than asked for is not an error.** A `OnceThrough` programme runs out, and
    /// a row whose audio this device does not hold is skipped rather than cueing a path
    /// that cannot be opened.
    Window { standing: Standing, cues: Vec<Cued> },
    /// **The token is not in this programme.** Ask for a window from the top.
    ///
    /// The rarer case than the design document assumed, because a reshuffle is a
    /// [`Ask::Reorder`] and keeps every token — so this means a genuine replacement.
    Stale { standing: Standing },
    /// One page of the programme. `standing.total` is how many there are in all.
    Look {
        standing: Standing,
        entries: Vec<Shown>,
        /// Where this page starts, counting from the head — so a view can say "showing 120 to
        /// 170 of 7,271" rather than leaving a person to guess where they are.
        at: usize,
        /// **The token the next page starts at**, or `None` at the end.
        ///
        /// A token rather than an offset, for the same reason a refill is addressed by one: an
        /// index shifts when somebody inserts above it, and a token does not.
        next: Option<u64>,
        /// **Whether the page begins at the entry asked for.**
        ///
        /// A token the programme no longer holds pages from the head rather than answering
        /// with nothing, so `at == 0` cannot say which of the two happened. A view anchored at
        /// what is sounding needs to know: when this is true the first entry *is* the one
        /// playing, and "up next" is everything after it. `false` when nothing was asked for
        /// too, since then nothing was anchored.
        anchored: bool,
    },
    /// A mutation landed.
    Settled { standing: Standing },
}

// ── Whether what sounded was a play ─────────────────────────────────────────

/// Past this, a track counts however long it is.
///
/// **Four minutes, so a 70-minute mix is not un-playable.** Half of one is 35 minutes, which
/// nobody sits through before it counts as heard.
const LONG_ENOUGH: std::time::Duration = std::time::Duration::from_secs(240);

/// Whether a track was played rather than skipped past.
///
/// Half of it, or four minutes, whichever comes first. A track of unknown
/// length has no half to take, so it has to earn the full four minutes —
/// otherwise a zero duration would count a play the instant it started.
pub fn counts_as_played(position: std::time::Duration, duration: std::time::Duration) -> bool {
    let threshold = if duration.is_zero() {
        LONG_ENOUGH
    } else {
        (duration / 2).min(LONG_ENOUGH)
    };
    position >= threshold
}
