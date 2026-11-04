//! Where a file belongs on disk, and whether it can go there — issue #132.
//!
//! `storage-layout.md` is the design. One readable tree, rendered from the
//! library:
//!
//! ```text
//! {file_under}/{album}/{disc-}{track} {title}.{ext}
//! ```
//!
//! **Structural facts only** — who made it, and what release it is on. Never
//! genre, era, catalog or tempo: a tree has one dimension and description has
//! many, so a folder holding a description must pick one and lie about the
//! rest. Those are views.
//!
//! ## Rendered from the library, never from the tags
//!
//! Not a preference. Several files here carry an album name truncated to 30
//! bytes by a long-dead pass through ID3v1, and the folder holds the full one
//! (#143) — a renderer reading tags would replace good names with stumps.
//!
//! ## It proposes and refuses; it does not mangle
//!
//! A component that is illegal, too long, or wanted by two different copies
//! goes to a review list and the file stays where it is. That is safe because
//! **the path is not the binding**: identity is the hash of the audio (#35,
//! #141), so a file sitting in the wrong place indefinitely costs nothing.
//! Silently truncating a name is how a library grows files nobody can find,
//! and this library already has some — Fiona Apple's album folder is cut
//! mid-word at 255 bytes.
//!
//! ## Composed on write — #151
//!
//! Every name this renders is **NFC**, because the two devices here disagree:
//! measured across the library, all 48 non-ASCII names are composed on APFS
//! and decomposed on the SMB share. Both filesystems happen to be
//! form-*insensitive* on lookup, so nothing is unreachable today — but `ext4`
//! and `btrfs` are not, and there the same album is genuinely two directories.
//!
//! This does not rewrite what is on disk. It fixes what the renderer
//! *proposes*, and the ordinary move carries it. The share will decompose
//! again on write, which is fine: the NAS is a copy (§8), not a source of
//! truth.

//! ## Spelled plainly — #167
//!
//! The same reasoning one step further. MusicBrainz catalogues the sleeve, and
//! a sleeve is typeset: `Doin’ Time`, `Froggie Went a‐Courtin’`, `Live
//! 1975–85`. A path is not typeset — it is typed, globbed and tab-completed —
//! and a U+2010 in a name that looks exactly like it has a hyphen is a name
//! nobody can reach. So [`TYPOGRAPHIC`] writes each such mark as the ASCII it
//! stands for.
//!
//! **Marks only.** Accents are spelling and are kept whole. The library keeps
//! the sleeve's own text either way; this is how a filename is written, which
//! is the same standing as replacing a colon.
//!
//! ## Two albums, one title — #322
//!
//! `{album}` is the release group's title, so pressings land together; two
//! *albums* with one title under one credit are told apart by kind, then year,
//! and refused where neither separates them. [`Shelves`] has the rule.
//!
//! ## Two pressings, one file name — #375
//!
//! The same rule one level down. Pressings of one album share its folder
//! until their file names collide; then those pressings, and only those, are
//! told apart by their own title and then their year (`Shelves::press`).
//!
//! ## No ellipsis, and no leading dot — 2026-10-25
//!
//! An ellipsis is dropped from a name and the space around it closes up; a
//! name never starts with a dot, which would hide it. `storage-layout.md` §5
//! has the measurement and what it reverses.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use crate::model::entity::{Encoding, Release, ReleaseGroup};
use crate::model::id::Id;
use crate::model::reference::CreditOn;
use crate::model::seam::Librarian;
use crate::model::spine::ReleaseType;

/// The longest a single path component may be, in bytes.
///
/// Every filesystem here stops at 255, and it is **bytes** rather than
/// characters — a name of accented text reaches it sooner than it reads.
const COMPONENT_LIMIT: usize = 255;

/// Characters no component may contain.
///
/// `/` because it is the separator, and the rest because this tree is read
/// over SMB from machines that refuse them.
const FORBIDDEN: [char; 9] = ['/', '\\', ':', '*', '?', '"', '<', '>', '|'];

/// What a shortened component ends with, so a reader can see it was cut.
const ELLIPSIS: &str = "…";

/// Names Windows will not open whatever the extension.
const RESERVED: [&str; 6] = ["CON", "PRN", "AUX", "NUL", "COM1", "LPT1"];

/// What is wrong with a path this library would otherwise have proposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refused {
    /// Nothing names the artist or the title, so there is nowhere to put it.
    Nameless,
    /// A component exceeds what a filesystem will hold.
    TooLong(String),
    /// A name Windows reserves.
    Reserved(String),
    /// Two copies want one path. **Both are refused**, because picking a
    /// winner would move one file on top of another's name.
    Contested(PathBuf),
    /// Two albums want one folder and nothing the library holds tells them
    /// apart — #322. Refused for the reason `Contested` is, one level up: their
    /// files would not collide, so nothing else would notice them mixing.
    Shared(PathBuf),
    /// Two pressings of one album would collide file by file and nothing the
    /// library holds about them tells them apart — #375.
    Pressed(PathBuf),
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refused::Nameless => write!(f, "nothing names it"),
            Refused::TooLong(part) => {
                write!(f, "{} bytes is over the limit: {part}", part.len())
            }
            Refused::Reserved(part) => write!(f, "a reserved name: {part}"),
            Refused::Contested(at) => write!(f, "two copies want {}", at.display()),
            Refused::Pressed(at) => {
                write!(
                    f,
                    "two pressings collide in {} and nothing tells them apart",
                    at.display()
                )
            }
            Refused::Shared(at) => {
                write!(
                    f,
                    "two albums want {} and nothing tells them apart",
                    at.display()
                )
            }
        }
    }
}

/// The ASCII a typographic mark is a variant of — #167.
///
/// **A curly quote is a typesetting choice, not a character.** MusicBrainz
/// catalogues `Doin’ Time` with U+2019 and `Froggie Went a‐Courtin’` with
/// U+2010, and both are right about the printed sleeve. A path is not printed
/// — it is typed, globbed, and completed by a shell, and none of those produce
/// U+2010. A hyphen nobody's keyboard makes, in a name that looks exactly like
/// one that has a hyphen, is a name that cannot be reached.
///
/// So the library keeps the sleeve's spelling and the tree spells it plainly.
/// The library is still the ledger; this is only how it writes a filename,
/// which is the same standing as `component` replacing a colon.
///
/// **Only marks, never letters.** `Sinéad`, `Björk` and `Beyoncé` keep every
/// accent: a diacritic is part of how a name is spelled and dropping it is a
/// misspelling, where `’` and `'` are two renderings of one apostrophe.
///
/// `…` is absent for a different reason. The harm this rule addresses is
/// **impersonation** — `‐` is indistinguishable from `-` and is not one — and
/// an ellipsis impersonates nothing; it is legible as itself. Folding it would
/// also destroy the very title the module's `fit` documentation is about:
/// `When the Pawn…` becomes `When the Pawn...`, and the trailing dots are then
/// stripped because Windows cannot open a name that ends in one, leaving
/// `When the Pawn`. The mark stays.
///
/// And only where the ASCII form is allowed here, which is why `“ ” ″` are
/// absent. `"` is in [`FORBIDDEN`], so folding them would land four real
/// titles — `“En Gallop”`, `No More “I Love You’s”` — on `_`. Turning a
/// character every filesystem here accepts into one none of them do is
/// destruction wearing the costume of a normalisation, so the quotes stay
/// curly.
const TYPOGRAPHIC: [(char, &str); 11] = [
    // Apostrophes and the prime that stands in for one.
    ('\u{2018}', "'"), // ‘ left single quote
    ('\u{2019}', "'"), // ’ right single quote
    ('\u{201B}', "'"), // ‛ reversed
    ('\u{2032}', "'"), // ′ prime — `40′`
    // Every dash that is a hyphen wearing better clothes.
    ('\u{2010}', "-"), // ‐ hyphen
    ('\u{2011}', "-"), // ‑ non-breaking hyphen
    ('\u{2012}', "-"), // ‒ figure dash
    ('\u{2013}', "-"), // – en dash
    ('\u{2014}', "-"), // — em dash
    ('\u{2212}', "-"), // − minus sign
    // A space that does not look like one is worse than the others: it is
    // invisible, and a name with one cannot be typed at all.
    ('\u{00A0}', " "), // no-break space
];

/// One component, with the characters a filesystem refuses taken out.
///
/// **Replaced rather than dropped**, so a title does not silently lose a word
/// boundary. A trailing dot or space is stripped because Windows cannot open a
/// name that has one — and only there, since `Manhattan Research, Inc.` is a
/// name and the dot is meaning rather than noise.
///
/// Typographic marks are folded first, per [`TYPOGRAPHIC`], and before `fit`
/// measures — `’` is three bytes and `'` is one, so folding after would spend
/// a budget on characters the name no longer has.
fn component(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if FORBIDDEN.contains(&c) || c.is_control() {
                "_".into()
            } else if let Some((_, ascii)) = TYPOGRAPHIC.iter().find(|(from, _)| *from == c) {
                (*ascii).to_owned()
            } else {
                c.to_string()
            }
        })
        .collect();
    // **Composed, and before `fit` measures it** — NFD spends extra bytes on
    // the combining marks, so an accented name would be shortened against a
    // budget it does not really exceed.
    //
    // **And no name starts with a dot** (2026-10-25): a leading `.` hides a
    // folder on every filesystem here, and Plex does not scan a hidden one —
    // Britney Spears' `...Baby One More Time` was the one album the rendering
    // held that Plex never showed.
    without_ellipses(&cleaned)
        .trim()
        .trim_start_matches(['.', ' '])
        .trim_end_matches(['.', ' '])
        .nfc()
        .collect()
}

/// **An ellipsis is dropped from a name**, and the space around it closes up
/// to one — 2026-10-25.
///
/// `…`, `...` and `. . .` are typesetting, like #167's curly quotes, and a
/// path is typed rather than printed: `I Am... Sasha Fierce` is `I Am Sasha
/// Fierce` on disk, `Oops!...I Did It Again` is `Oops! I Did It Again`, and a
/// leading one goes entirely. The library keeps the sleeve's spelling; this is
/// only how a file is named. Three or more dots, so `Vol. 2` and `Jr.` stand.
///
/// Not the `…` [`fit`] writes on a name it shortened: that one is added after,
/// and means "this was cut".
fn without_ellipses(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let end = ellipsis_at(&chars, i);
        if end == i {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        let mut next = end;
        while chars.get(next) == Some(&' ') {
            next += 1;
        }
        let kept = out.trim_end_matches(' ').len();
        out.truncate(kept);
        if !out.is_empty() && next < chars.len() {
            out.push(' ');
        }
        i = next;
    }
    out
}

/// Where an ellipsis starting at `at` ends, or `at` itself if none starts there.
fn ellipsis_at(chars: &[char], at: usize) -> usize {
    if chars[at] == '\u{2026}' {
        return at + 1;
    }
    let mut dots = 0;
    let mut end = at;
    while chars.get(end) == Some(&'.') {
        dots += 1;
        end += 1;
        // `. . .` counts too: a single space between dots.
        if chars.get(end) == Some(&' ') && chars.get(end + 1) == Some(&'.') {
            end += 1;
        }
    }
    if dots >= 3 { end } else { at }
}

/// Shorten a component to fit, at a word boundary, and say so — #150.
///
/// **Deliberate and visible, which is the whole difference.** Refusing was
/// right while nothing shortened: a name silently cut mid-word is one nobody
/// can search for, and this library already had one — Fiona Apple's album
/// folder ended `…to Batter When`, four words into a sentence.
///
/// But the refusal cannot be the final answer, because the title is not a
/// mistake. *When the Pawn…* is 444 characters of poem and held the Guinness
/// record for the longest album title; the library is right to hold it whole
/// and no filesystem will. So the **path** gives way, and the `…` says it did.
///
/// Cut at a space so the result is a phrase rather than a fragment, and never
/// mid-character.
fn fit(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_string();
    }
    let room = budget.saturating_sub(ELLIPSIS.len());
    let mut end = room.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let cut = text[..end].trim_end();
    let at = cut.rfind(' ').unwrap_or(cut.len());
    // A single word longer than the budget has no boundary to cut at, so it is
    // cut where it must be.
    let kept = if at == 0 { cut } else { &cut[..at] };
    format!("{}{ELLIPSIS}", kept.trim_end())
}

/// A path as the filesystem would match it, for comparing two of them.
///
/// **Composed and case-folded**, and both halves are load-bearing. APFS and
/// SMB fold case, so two artists differing only in capitals are one directory.
/// They also fold composition — and they disagree about which form to hand
/// back, so a name written here as `Sinéad` is read back off the share
/// decomposed. Comparing the bytes says those are two places; the filesystem
/// says they are one, and the filesystem is right.
fn folded(at: &Path) -> String {
    at.to_string_lossy()
        .nfc()
        .collect::<String>()
        .to_lowercase()
}

/// Whether a component can be a directory or a file name.
fn check(part: &str) -> Result<(), Refused> {
    if part.is_empty() {
        return Err(Refused::Nameless);
    }
    if part.len() > COMPONENT_LIMIT {
        return Err(Refused::TooLong(part.to_string()));
    }
    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
    if RESERVED.contains(&stem.as_str()) {
        return Err(Refused::Reserved(part.to_string()));
    }
    Ok(())
}

/// What an album's folder is called where its title alone does not say which
/// album it is — #322.
///
/// **`{album}` is the release group's title**, so two pressings of one album
/// land together, which is meant. Two *albums* with one title under one credit
/// landed together too, which is not: three Enya releases called *Amarantine*
/// in one folder, and five performances of Purcell's *Dido and Aeneas* — every
/// one credited `Henry Purcell; …`. Their file names differed, so the
/// `Contested` check never fired and the albums mixed without a word, under
/// one `cover.jpg` and one album gain.
///
/// **Qualified by what the library holds about the album**, which keeps it
/// inside the structural rule: what kind of release it is, then the year it
/// first appeared. Only rivals get a qualifier, and only the facts that
/// separate them, so a library with one *Amarantine* never sees a year in the
/// name:
///
/// - **The kind, where the rivals' kinds differ.** An album keeps its plain
///   name, and the single named after it says `(Single)`.
/// - **The year, among those the kind left together** — `Dido and Aeneas
///   (1986)`. For classical this is the ordinary case rather than an edge: one
///   composer and one title, many times over, is what a classical shelf is.
///
/// **Whatever is still together is refused**, both sides, as two copies
/// wanting one path are: picking a winner is a guess, and a folder of two
/// albums is the failure this exists to stop.
///
/// **Rivals are found across the whole library**, never only the copies being
/// placed — an import of one *Dido* has to know the library already holds
/// another. Only albums some copy is filed on count, because a group nothing is
/// filed on has no folder to want.
#[derive(Debug, Default)]
struct Shelves {
    /// A group whose folder is not its plain title, and what it is instead.
    named: BTreeMap<Id<ReleaseGroup>, String>,
    /// A group nothing tells apart from another wanting its folder.
    shared: BTreeMap<Id<ReleaseGroup>, PathBuf>,
    /// A pressing whose files would collide with another pressing's, and the
    /// folder it takes instead — #375.
    pressed: BTreeMap<Id<Release>, String>,
    /// A pressing nothing tells apart from one it collides with.
    shared_pressings: BTreeMap<Id<Release>, PathBuf>,
}

/// One album wanting a folder.
struct Rival {
    group: Id<ReleaseGroup>,
    under: String,
    album: String,
    kind: ReleaseType,
    year: Option<i16>,
}

impl Shelves {
    fn of(l: &Librarian) -> Self {
        let filed: BTreeSet<Id<ReleaseGroup>> = l
            .every_copy()
            .into_iter()
            .filter_map(|copy| l.group_of(l.placed_at(copy)))
            .collect();

        let mut wanted: BTreeMap<String, Vec<Rival>> = BTreeMap::new();
        for group in filed {
            let Some(g) = l.spine.groups.get(&group) else {
                continue;
            };
            // An album with no credit of its own files each track under the
            // track's, so it has no one folder to contest.
            let under = l.reference.files_under(CreditOn::ReleaseGroup(group));
            let album = component(&g.title);
            if under.is_empty() || album.is_empty() {
                continue;
            }
            let under = fit(&component(&under), COMPONENT_LIMIT);
            wanted
                .entry(folded(
                    &Path::new(&under).join(fit(&album, COMPONENT_LIMIT)),
                ))
                .or_default()
                .push(Rival {
                    group,
                    under,
                    album,
                    kind: g.kind,
                    year: g.first_released.map(|d| d.year),
                });
        }

        let mut out = Shelves::default();
        let mut taken: BTreeMap<String, Vec<(Holder, PathBuf)>> = BTreeMap::new();
        let mut folders: BTreeMap<Id<ReleaseGroup>, (String, String)> = BTreeMap::new();
        for rivals in wanted.values() {
            let marks = if rivals.len() > 1 {
                qualifiers(rivals)
            } else {
                vec![Vec::new()]
            };
            for (rival, marks) in rivals.iter().zip(marks) {
                let name = if marks.is_empty() {
                    rival.album.clone()
                } else {
                    let name = format!("{} ({})", rival.album, marks.join(", "));
                    out.named.insert(rival.group, name.clone());
                    name
                };
                let at = Path::new(&rival.under).join(fit(&name, COMPONENT_LIMIT));
                taken
                    .entry(folded(&at))
                    .or_default()
                    .push((Holder::Album(rival.group), at));
                folders.insert(rival.group, (rival.under.clone(), name));
            }
        }
        out.press(l, &folders, &mut taken);
        // **Checked after naming, and against every album**: a qualified name
        // can land on another album's plain one, and two rivals of one kind
        // and one year are still one folder.
        for claims in taken.into_values().filter(|c| c.len() > 1) {
            for (holder, at) in claims {
                match holder {
                    Holder::Album(group) => out.shared.insert(group, at),
                    Holder::Pressing(release) => out.shared_pressings.insert(release, at),
                };
            }
        }
        out
    }

    /// **Two pressings of one album whose files collide get a folder each** —
    /// #375, and #322's rule one level down.
    ///
    /// `{album}` is the group's title so that pressings land together, and
    /// usually they can: a deluxe edition's extra tracks have names of their
    /// own. Where they cannot — the 1988 *Enya* and its 1992 reissue *The
    /// Celts*, one tracklist under two titles — every track was `Contested`
    /// and both sides stayed where they were, which left the album in two
    /// folders anyway, with whichever track's title happened to differ moved
    /// in beside the other pressing.
    ///
    /// So only the pressings whose names collide are qualified, and only by
    /// what separates them: **the pressing's own title** where it is not the
    /// album's — `The Celts (Enya)` — then **the year it was issued**. One
    /// that needs neither keeps the album's folder. Whatever still collides is
    /// refused both sides, as before.
    fn press(
        &mut self,
        l: &Librarian,
        folders: &BTreeMap<Id<ReleaseGroup>, (String, String)>,
        taken: &mut BTreeMap<String, Vec<(Holder, PathBuf)>>,
    ) {
        // Which pressings claim each file name inside an album's folder —
        // **among copies held here**, as `layout_of` counts a contest: a copy
        // held only on another device, or retired, has no file to collide.
        let mut names: BTreeMap<(Id<ReleaseGroup>, String), BTreeSet<Id<Release>>> =
            BTreeMap::new();
        for copy in l.every_copy() {
            if l.where_held(copy).is_none() {
                continue;
            }
            let slot = l.placed_at(copy);
            let (Some(group), Some(release)) = (l.group_of(slot), l.release_of(slot)) else {
                continue;
            };
            if !folders.contains_key(&group) {
                continue;
            }
            let Some((name, _)) = leaf(l, copy) else {
                continue;
            };
            names
                .entry((group, folded(Path::new(&name))))
                .or_default()
                .insert(release);
        }
        let mut colliding: BTreeMap<Id<ReleaseGroup>, BTreeSet<Collision>> = BTreeMap::new();
        for ((group, _), releases) in names {
            let releases: Vec<_> = releases.into_iter().collect();
            for (i, a) in releases.iter().enumerate() {
                for b in &releases[i + 1..] {
                    colliding.entry(group).or_default().insert((*a, *b));
                }
            }
        }

        for (group, pairs) in colliding {
            let (under, base) = &folders[&group];
            let album = l
                .spine
                .groups
                .get(&group)
                .map(|g| component(&g.title))
                .unwrap_or_default();
            let involved: Vec<Id<Release>> = pairs
                .iter()
                .flat_map(|(a, b)| [*a, *b])
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let pressings: Vec<(String, Option<i16>)> = involved
                .iter()
                .map(|r| {
                    let r = l.spine.releases.get(r);
                    (
                        r.map(|r| component(&r.title)).unwrap_or_default(),
                        r.and_then(|r| r.released).map(|d| d.year),
                    )
                })
                .collect();
            let marks = pressing_marks(&album, &pressings);
            let mut plain: BTreeSet<Id<Release>> = BTreeSet::new();
            for (release, marks) in involved.iter().zip(marks) {
                if marks.is_empty() {
                    plain.insert(*release);
                    continue;
                }
                let name = format!("{base} ({})", marks.join(", "));
                let at = Path::new(under).join(fit(&name, COMPONENT_LIMIT));
                taken
                    .entry(folded(&at))
                    .or_default()
                    .push((Holder::Pressing(*release), at));
                self.pressed.insert(*release, name);
            }
            // Two pressings left in the album's own folder that still collide
            // are what this could not separate. Those marked alike land on one
            // qualified folder and are caught by `taken`.
            for (a, b) in &pairs {
                if plain.contains(a) && plain.contains(b) {
                    let at = Path::new(under).join(fit(base, COMPONENT_LIMIT));
                    self.shared_pressings.insert(*a, at.clone());
                    self.shared_pressings.insert(*b, at);
                }
            }
        }
    }
}

/// Two pressings of one album that want one file name.
type Collision = (Id<Release>, Id<Release>);

/// Who wants a folder: an album, or one pressing of it given its own.
#[derive(Debug, Clone, Copy)]
enum Holder {
    Album(Id<ReleaseGroup>),
    Pressing(Id<Release>),
}

/// What separates each of several pressings whose files collide — the
/// pressing's own title where it is not the album's, then the year it was
/// issued among those the title left together.
fn pressing_marks(album: &str, pressings: &[(String, Option<i16>)]) -> Vec<Vec<String>> {
    let fold = |t: &str| t.nfc().collect::<String>().to_lowercase();
    let mut marks: Vec<Vec<String>> = pressings
        .iter()
        .map(|(title, _)| {
            if title.is_empty() || fold(title) == fold(album) {
                Vec::new()
            } else {
                vec![title.clone()]
            }
        })
        .collect();
    let mut together: BTreeMap<Vec<String>, Vec<usize>> = BTreeMap::new();
    for (i, m) in marks.iter().enumerate() {
        together
            .entry(m.iter().map(|t| fold(t)).collect())
            .or_default()
            .push(i);
    }
    for still in together.into_values().filter(|s| s.len() > 1) {
        let years: BTreeSet<i16> = still.iter().filter_map(|&i| pressings[i].1).collect();
        if years.len() < 2 {
            continue;
        }
        for i in still {
            if let Some(year) = pressings[i].1 {
                marks[i].push(year.to_string());
            }
        }
    }
    marks
}

/// What separates each of several albums wanting one folder — the kind where
/// their kinds differ, then the year among those the kind left together.
fn qualifiers(rivals: &[Rival]) -> Vec<Vec<String>> {
    let mut marks: Vec<Vec<String>> = vec![Vec::new(); rivals.len()];
    if rivals.iter().any(|r| r.kind != rivals[0].kind) {
        for (i, rival) in rivals.iter().enumerate() {
            if let Some(kind) = kind_label(rival.kind) {
                marks[i].push(kind.to_owned());
            }
        }
    }
    let mut together: BTreeMap<Vec<String>, Vec<usize>> = BTreeMap::new();
    for (i, m) in marks.iter().enumerate() {
        together.entry(m.clone()).or_default().push(i);
    }
    for still in together.into_values().filter(|s| s.len() > 1) {
        let years: BTreeSet<i16> = still.iter().filter_map(|&i| rivals[i].year).collect();
        if years.len() < 2 {
            continue;
        }
        for i in still {
            if let Some(year) = rivals[i].year {
                marks[i].push(year.to_string());
            }
        }
    }
    marks
}

/// How a folder names a kind of release, and `None` for an album, which is
/// what a folder is unless it says otherwise.
fn kind_label(kind: ReleaseType) -> Option<&'static str> {
    match kind {
        ReleaseType::Album => None,
        ReleaseType::Single => Some("Single"),
        ReleaseType::Ep => Some("EP"),
        ReleaseType::Broadcast => Some("Broadcast"),
        ReleaseType::Other => Some("Other"),
    }
}

/// The file name a copy renders to — the part of its path that does not
/// depend on which folder it lands in — and whether it has no title.
fn leaf(l: &Librarian, copy: Id<Encoding>) -> Option<(String, bool)> {
    let format = l.encoding(copy)?.format;
    let row = l.describe(l.row_of_copy(copy)?)?;
    let title = component(if row.title.is_empty() { "" } else { row.title });

    // The disc prefix only where the release has more than one (#134).
    let discs = l
        .release_of(l.placed_at(copy))
        .and_then(|r| l.release_media(r))
        .unwrap_or(1);
    let lead = match (row.track, row.disc, discs > 1) {
        (Some(track), Some(disc), true) => format!("{disc}-{track:02} "),
        (Some(track), _, _) => format!("{track:02} "),
        (None, _, _) => String::new(),
    };

    // The extension and the track prefix are part of the name's budget.
    let ext = format.extension();
    let room = COMPONENT_LIMIT.saturating_sub(lead.len() + 1 + ext.len());
    Some((
        format!("{lead}{}.{ext}", fit(&title, room)),
        title.is_empty(),
    ))
}

/// Where this copy belongs, relative to the library root.
///
/// `None` for a copy the library cannot describe — it has no row, so there is
/// nothing to render from.
fn proposed(
    l: &Librarian,
    shelves: &Shelves,
    copy: Id<Encoding>,
) -> Option<Result<PathBuf, Refused>> {
    // **A row is an appearance** (#155) and this renders a *file*, so the file
    // names its row rather than being one.
    let row = l.describe(l.row_of_copy(copy)?)?;

    let group = l.group_of(l.placed_at(copy));
    if let Some(at) = group.and_then(|g| shelves.shared.get(&g)) {
        return Some(Err(Refused::Shared(at.clone())));
    }
    let release = l.release_of(l.placed_at(copy));
    if let Some(at) = release.and_then(|r| shelves.shared_pressings.get(&r)) {
        return Some(Err(Refused::Pressed(at.clone())));
    }

    let under = fit(&component(row.file_under), COMPONENT_LIMIT);
    // **`album_if_named`, not `album`.** The latter stands in `No Album` so a
    // tree node always has a label; rendering that would make a directory
    // called "No Album" the answer to "what release is this on".
    let album = release
        .and_then(|r| shelves.pressed.get(&r))
        .or_else(|| group.and_then(|g| shelves.named.get(&g)))
        .map(String::as_str)
        .or(row.album_if_named)
        .map(component)
        .filter(|a| !a.is_empty())
        .map(|a| fit(&a, COMPONENT_LIMIT));
    let (name, untitled) = leaf(l, copy)?;
    let mut parts = vec![under];
    parts.extend(album);
    parts.push(name);

    for part in &parts {
        if let Err(refused) = check(part) {
            return Some(Err(refused));
        }
    }
    // A title of nothing leaves a name that is only an extension.
    if untitled {
        return Some(Err(Refused::Nameless));
    }
    Some(Ok(parts.iter().collect()))
}

/// Everything a re-render would do, decided before anything is touched.
#[derive(Debug, Default)]
pub struct Plan {
    pub settled: usize,
    pub elsewhere: usize,
    pub moves: Vec<(Id<Encoding>, PathBuf, PathBuf)>,
    pub refused: Vec<(Id<Encoding>, Refused)>,
}

/// Plan a re-render of everything this device holds under `root`.
///
/// **Contested paths are refused on both sides.** Two copies wanting one name
/// is not a tie to be broken — moving one on top of the other's name is how a
/// file disappears — so both stay put and both are reported.
pub fn plan(l: &Librarian, root: &Path) -> Plan {
    plan_of(l, root, &l.every_copy())
}

/// Where each of a set of copies belongs, relative to whatever root it lands
/// under — the half of a plan that does not care where the file is now.
#[derive(Debug, Default)]
pub struct Layout {
    /// Held on this device but with no row to render from, or not held here.
    pub elsewhere: usize,
    pub placed: Vec<(Id<Encoding>, PathBuf)>,
    pub refused: Vec<(Id<Encoding>, Refused)>,
}

/// Lay out a set of copies, refusing a path two of them want.
///
/// **One notion of "contested" for a re-render and a rendering** (#332). A
/// rendering writes the same tree into a different root, so two copies that
/// would collide in the library collide there too, and a second check written
/// for the rendering would be a second answer to one question.
pub fn layout_of(l: &Librarian, copies: &[Id<Encoding>]) -> Layout {
    let mut wanted: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = Layout::default();
    let mut proposals = Vec::new();
    let shelves = Shelves::of(l);

    for copy in copies.iter().copied() {
        if l.where_held(copy).is_none() {
            out.elsewhere += 1;
            continue;
        }
        match proposed(l, &shelves, copy) {
            None => out.elsewhere += 1,
            Some(Err(refused)) => out.refused.push((copy, refused)),
            Some(Ok(at)) => {
                // **Case-insensitively**, because APFS and SMB fold case: two
                // artists differing only in capitals are one directory, and
                // finding that out during a move is finding out too late.
                *wanted.entry(folded(&at)).or_default() += 1;
                proposals.push((copy, at));
            }
        }
    }

    // Second pass, now that every wanted path is known.
    for (copy, at) in proposals {
        if wanted.get(&folded(&at)).copied().unwrap_or(0) > 1 {
            out.refused.push((copy, Refused::Contested(at)));
        } else {
            out.placed.push((copy, at));
        }
    }
    out
}

/// The same, over a chosen set — what an import plans for the files it just
/// took in, rather than for the whole library.
pub fn plan_of(l: &Librarian, root: &Path, copies: &[Id<Encoding>]) -> Plan {
    let layout = layout_of(l, copies);
    let mut out = Plan {
        elsewhere: layout.elsewhere,
        refused: layout.refused,
        ..Plan::default()
    };

    let mut settled = 0;
    let mut moves = Vec::new();
    for (copy, to) in layout.placed {
        let from = l.where_held(copy).map(PathBuf::from).unwrap_or_default();
        // Folded, not byte-equal: a file already in the right place on a share
        // that decomposed its name would otherwise be proposed for a move on
        // every run, and the move is a rename onto itself.
        if folded(&from) == folded(&root.join(&to)) {
            settled += 1;
        } else {
            moves.push((copy, from, to));
        }
    }
    out.settled = settled;
    out.moves = moves;
    out
}

/// What carrying a plan out actually did.
#[derive(Debug, Default)]
pub struct Moved {
    pub moved: usize,
    /// Cover art, rip logs and the like, carried along with their album.
    pub assets: usize,
    /// **Files the operating system left, removed rather than carried** — #239.
    ///
    /// A thumbnail cache has no destination worth having, and the file browser writes
    /// another the next time it looks. Counted because a deletion is reported even
    /// when what it deleted was nobody's.
    pub discarded: usize,
    /// A move that could not be made, and why. The file stays where it is.
    pub failed: Vec<(PathBuf, String)>,
    /// Directories left empty by a move and taken away.
    pub emptied: usize,
}

/// Carry out a plan: move each file to where the library says it belongs.
///
/// **Nothing is overwritten, ever.** A destination that already exists stops
/// that one move and is reported — even though the plan checked for collisions
/// among the files it knows about, because the tree may hold something the
/// library does not.
///
/// **A rename, not a copy.** Within one root it is atomic and does not touch a
/// byte of the file, which is what makes this safe to do to 6,000 files: the
/// audio cannot change, so identity cannot change, so nothing can be lost even
/// if the pass dies halfway. What a half-finished run leaves is some files
/// moved and some not, which the next scan reconciles and the next plan
/// finishes.
pub fn carry_out(plan: &Plan, root: &Path, write: bool) -> Moved {
    let mut out = Moved::default();
    // Where each source folder's audio went, so what is left in it can follow.
    let mut followed: BTreeMap<PathBuf, PathBuf> = BTreeMap::new();
    for (_, from, to) in &plan.moves {
        let at = root.join(to);
        if at.exists() {
            out.failed
                .push((from.clone(), format!("{} is already there", at.display())));
            continue;
        }
        if !write {
            out.moved += 1;
            continue;
        }
        if let Some(parent) = at.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            out.failed.push((from.clone(), e.to_string()));
            continue;
        }
        match std::fs::rename(from, &at) {
            Ok(()) => {
                out.moved += 1;
                if let (Some(was), Some(now)) = (from.parent(), at.parent()) {
                    followed.insert(was.to_path_buf(), now.to_path_buf());
                }
            }
            Err(e) => out.failed.push((from.clone(), e.to_string())),
        }
    }

    // **What sits beside an album goes with it** (#149). Cover art, a rip log
    // and a cue sheet belong to the *release*, and moving the audio out from
    // under them left 13 orphaned images in this library's old tree.
    //
    // After the audio, and only from a folder none is left in: a directory
    // still holding music has not moved anywhere yet.
    if write {
        for (was, now) in &followed {
            let carried = carry_assets(was, now, root);
            out.assets += carried.moved;
            out.discarded += carried.discarded;
        }
    }
    for was in followed.keys() {
        out.emptied += prune(Some(was), root);
    }
    out
}

/// **Carry retired bytes out of the library** — `storage-layout.md` §6, #159.
///
/// Step two of how a file leaves: the tombstone is written first and this
/// moves the bytes to `.departed/`, keeping the path they had beneath it so a
/// restore is a move back and not a puzzle.
///
/// **Never a delete.** §6 makes the two steps separate precisely because the
/// first is reversible and the second is not, and there is no second step
/// here — a purge is its own operation and does not exist yet.
///
/// A file already under `.departed` is left alone rather than refused: that is
/// the album somebody retired by hand before `remove` existed, and its
/// tombstone is the only thing it is missing.
pub fn retire(root: &Path, files: &[PathBuf], write: bool) -> Moved {
    let mut out = Moved::default();
    let departed = root.join(crate::operations::scan::DEPARTED);
    let mut emptied: std::collections::BTreeSet<PathBuf> = Default::default();
    for from in files {
        if from.starts_with(&departed) {
            continue;
        }
        let Ok(under) = from.strip_prefix(root) else {
            out.failed.push((
                from.clone(),
                "outside the library, so there is nowhere beneath `.departed` for it".into(),
            ));
            continue;
        };
        let to = departed.join(under);
        if to.exists() {
            out.failed
                .push((from.clone(), format!("{} is already there", to.display())));
            continue;
        }
        if !write {
            out.moved += 1;
            continue;
        }
        if let Some(parent) = to.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            out.failed.push((from.clone(), e.to_string()));
            continue;
        }
        match std::fs::rename(from, &to) {
            Ok(()) => {
                out.moved += 1;
                if let Some(was) = from.parent() {
                    emptied.insert(was.to_path_buf());
                }
            }
            Err(e) => out.failed.push((from.clone(), e.to_string())),
        }
    }
    if write {
        // The cover goes with the record, as it does on a render (#149).
        for was in &emptied {
            if let Ok(under) = was.strip_prefix(root) {
                let carried = carry_assets(was, &departed.join(under), root);
                out.assets += carried.moved;
                out.discarded += carried.discarded;
            }
        }
        for was in &emptied {
            out.emptied += prune(Some(was), root);
        }
    }
    out
}

/// **What belongs to a release rather than to whoever owns the folder** — #239.
///
/// `carry_assets` moved *everything* left beside the audio, which is right for a
/// release folder inside the library and wrong for anywhere else. `shelf import`
/// places from arbitrary paths, so "the folder the audio has gone from" can be
/// `~/Downloads` — and it carried a `config.toml` out of one three times in a single
/// session before anybody noticed, because the report only counts audio.
///
/// **The list is measured, not imagined.** Every non-audio file in the live library,
/// by extension: 137 jpg, 33 jpeg, 14 txt, 14 log, 11 bmp, 8 png, 8 asd, 2 m3u8,
/// 1 gif — against 4 ini, 2 db, and the odd `toml`, `lock` and `jsonl` that are
/// plainly not a record's. `storage-layout.md` §9 names the first three kinds by
/// hand ("cover art, `rip.log` and a cue sheet"); this is that list with the shapes
/// the disk actually holds.
///
/// `asd` is Ableton's per-track analysis, which travels with its track and is cheap
/// to regenerate if it does not. `ini` and `db` are `desktop.ini` and `Thumbs.db`,
/// which belong to whatever browsed the folder.
///
/// **`txt` is the one arguable entry**, and the measurement decides it: fourteen of
/// them in this library are rip logs and track lists. A stray note carried out of a
/// download folder is a small surprise, and the report now says how many followed.
fn is_asset(path: &Path) -> bool {
    /// Extensions a release's own files have, lowercased.
    const ASSETS: &[&str] = &[
        // Cover art, which is 189 of the library's 214 non-audio files.
        "jpg", "jpeg", "png", "gif", "bmp", "webp", "tif", "tiff", // What a rip leaves behind.
        "log", "txt", "cue", "nfo", "accurip", "md5", "sfv",
        // A playlist over one release, and Ableton's per-track analysis.
        "m3u", "m3u8", "asd",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| ASSETS.contains(&e.as_str()))
}

/// Move a release's own files out of a folder its audio has gone from, and drop what
/// the operating system left there.
///
/// **Only inside the library**, which is the discriminator #239 deferred and the
/// answer to what it deferred it for. A `render` move takes a release from one place
/// in the tree to another and both folders are the library's — that is #149's case,
/// where the cover has to follow. An `import` places from wherever it was pointed,
/// and `~/Downloads` is not a release folder: nothing there is the library's to move
/// or to delete, so an import takes the audio and nothing else.
///
/// **Only when no audio remains**, because a folder still holding music is a folder
/// that has not moved: two albums under one directory would otherwise have the first
/// one's move drag the second one's cover away.
///
/// **And only what [`is_asset`] admits.** What `scan::is_junk` admits is *removed*
/// instead — a thumbnail cache has no destination worth having, and carrying one
/// would spread it.
fn carry_assets(was: &Path, now: &Path, root: &Path) -> Carried {
    let mut out = Carried::default();
    // Not the library's folder, so not the library's files. The audio has already
    // gone; everything else stays exactly as it was found.
    if !was.starts_with(root) {
        return out;
    }
    let Ok(entries) = std::fs::read_dir(was) else {
        return out;
    };
    let left: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    if left.iter().any(|p| crate::operations::scan::is_audio(p)) {
        return out;
    }
    for file in &left {
        if crate::operations::scan::is_junk(file) {
            if std::fs::remove_file(file).is_ok() {
                out.discarded += 1;
            }
            continue;
        }
        if !is_asset(file) {
            continue;
        }
        let Some(name) = file.file_name() else {
            continue;
        };
        let to = now.join(name);
        if !to.exists() && std::fs::rename(file, &to).is_ok() {
            out.moved += 1;
        }
    }
    out
}

/// What followed a release, and what was thrown away behind it.
#[derive(Debug, Default, Clone, Copy)]
struct Carried {
    moved: usize,
    discarded: usize,
}

/// Take away directories a move left empty, up to but never including `root`.
///
/// **Only empty ones**, so nothing can be lost: `remove_dir` refuses a
/// directory with anything in it, and that refusal is the whole safety
/// argument rather than a check written here.
fn prune(from: Option<&Path>, root: &Path) -> usize {
    let mut gone = 0;
    let mut at = from.map(Path::to_path_buf);
    while let Some(dir) = at {
        if dir == root || !dir.starts_with(root) || std::fs::remove_dir(&dir).is_err() {
            break;
        }
        gone += 1;
        at = dir.parent().map(Path::to_path_buf);
    }
    gone
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Tagged;
    use crate::operations::scan::Holdings;

    fn library(files: &[Tagged]) -> Librarian {
        let mut held = Holdings::new();
        for file in files {
            held.upsert(file.clone());
        }
        let mut l = crate::fixture::from_holdings(&held);
        l.here(crate::fixture::device());
        l
    }

    fn only(l: &Librarian) -> Result<PathBuf, Refused> {
        // **Storage is copy-shaped** (#155): it renders a file, not a row.
        let copy = l.every_copy()[0];
        proposed(l, &Shelves::of(l), copy).expect("a fixture row renders")
    }

    /// `Sinéad`, decomposed: an `e` followed by U+0301 COMBINING ACUTE.
    const NFD: &str = "Sine\u{301}ad O'Connor";
    /// The same name, composed: a single U+00E9.
    const NFC: &str = "Sin\u{e9}ad O'Connor";

    #[test]
    fn a_decomposed_name_renders_composed() {
        // The share hands names back decomposed, so this is what a tag read
        // off the NAS actually carries.
        let l = library(&[Tagged {
            path: "/m/x.flac".into(),
            artist: Some(NFD.into()),
            album: Some("The Lion and the Cobra".into()),
            title: Some("Troy".into()),
            tracknumber: Some(1),
            ..Default::default()
        }]);
        let at = only(&l).expect("it renders");
        let under = at.components().next().expect("a first component");
        assert_eq!(
            under.as_os_str().to_string_lossy(),
            NFC,
            "the renderer proposes NFC whatever form the library carries"
        );
    }

    #[test]
    fn the_two_forms_of_one_name_are_one_place() {
        assert_eq!(folded(&PathBuf::from(NFD)), folded(&PathBuf::from(NFC)));
        assert_eq!(
            folded(&PathBuf::from("Bjork")),
            folded(&PathBuf::from("BJORK")),
            "case still folds — the composed half must not have replaced it"
        );
        assert_ne!(
            folded(&PathBuf::from("Bj\u{f6}rk")),
            folded(&PathBuf::from("Bjork")),
            "an accent is not noise: folding it away would merge distinct artists"
        );
    }

    #[test]
    fn a_file_already_placed_under_a_decomposed_name_is_settled_not_moved() {
        // The bug this would have caught: `from` comes off a share that
        // decomposed the name, `to` renders composed, the bytes differ, and
        // every accented file is proposed for a move on every run — a rename
        // onto itself, forever.
        let l = library(&[Tagged {
            path: format!("/m/{NFD}/The Lion and the Cobra/01 Troy.flac").into(),
            artist: Some(NFC.into()),
            album: Some("The Lion and the Cobra".into()),
            title: Some("Troy".into()),
            tracknumber: Some(1),
            ..Default::default()
        }]);
        let plan = plan(&l, Path::new("/m"));
        assert_eq!(plan.moves.len(), 0, "nothing to move: it is already there");
        assert_eq!(plan.settled, 1);
    }

    /// Two copies of one name in two Unicode forms, with no album between them —
    /// so the only place they can meet is the file name.
    fn in_two_forms(album: Option<&str>) -> Librarian {
        library(&[
            Tagged {
                path: "/m/a.flac".into(),
                artist: Some(NFD.into()),
                album: album.map(Into::into),
                title: Some("Troy".into()),
                tracknumber: Some(1),
                ..Default::default()
            },
            Tagged {
                path: "/m/b.flac".into(),
                artist: Some(NFC.into()),
                album: album.map(Into::into),
                title: Some("Troy".into()),
                tracknumber: Some(1),
                ..Default::default()
            },
        ])
    }

    #[test]
    fn two_recordings_whose_names_differ_only_in_form_contest_one_path() {
        // The filesystem is form-insensitive, so these land in one directory.
        // A byte-comparing check reports no contest and finds out during the
        // move, which is too late.
        //
        // **No album**, since #322: with one, the two credits are two albums
        // wanting one folder and are refused a level up, before the file names
        // are ever compared — the next test.
        let plan = plan(&in_two_forms(None), Path::new("/m"));
        assert_eq!(
            plan.moves.len(),
            0,
            "neither moves while the contest stands"
        );
        assert_eq!(
            plan.refused.len(),
            2,
            "both are reported: {:?}",
            plan.refused
        );
        assert!(
            plan.refused
                .iter()
                .all(|(_, r)| matches!(r, Refused::Contested(_))),
            "both are refused as contested, not silently merged: {:?}",
            plan.refused
        );
    }

    /// **The same, one level up** — #322. Two spellings of one artist mint two
    /// credits and so two albums, and `component` composes both names, so the
    /// folder they want is one folder byte for byte.
    #[test]
    fn two_albums_whose_credits_differ_only_in_form_want_one_folder() {
        let l = in_two_forms(Some("The Lion and the Cobra"));
        assert_eq!(
            albums(&l),
            2,
            "two credits have to be two albums for this to test anything"
        );
        let plan = plan(&l, Path::new("/m"));
        assert_eq!(plan.moves.len(), 0);
        assert_eq!(
            plan.refused.len(),
            2,
            "both are reported: {:?}",
            plan.refused
        );
        assert!(
            plan.refused
                .iter()
                .all(|(_, r)| matches!(r, Refused::Shared(_))),
            "{:?}",
            plan.refused
        );
    }

    /// **An ellipsis is dropped from a name, and none starts with a dot** —
    /// 2026-10-25. The cases are the library's own.
    #[test]
    fn an_ellipsis_is_dropped_and_no_name_starts_with_a_dot() {
        let cases = [
            ("I Am... Sasha Fierce", "I Am Sasha Fierce"),
            ("...Baby One More Time", "Baby One More Time"),
            ("\u{2026}Baby One More Time", "Baby One More Time"),
            ("Oops!...I Did It Again", "Oops! I Did It Again"),
            ("Wait . . . What", "Wait What"),
            ("When the Pawn\u{2026}", "When the Pawn"),
            (".38 Special", "38 Special"),
        ];
        for (title, on_disk) in cases {
            assert_eq!(component(title), on_disk, "{title:?}");
        }
        // What must stand: two dots are not an ellipsis, and a name's own dot is
        // meaning — only a trailing one goes, as it always has, for Windows.
        assert_eq!(component("Vol. 2"), "Vol. 2");
        assert_eq!(component("Live.. Again"), "Live.. Again");
        assert_eq!(
            component("Manhattan Research, Inc."),
            "Manhattan Research, Inc"
        );
        assert!(
            cases.iter().all(|(title, _)| title.contains("...")
                || title.contains('\u{2026}')
                || title.contains(". .")
                || title.starts_with('.')),
            "instrument: every case has something to drop"
        );
        // `fit`'s own `…` means "this was cut", and is added after.
        let long = "word ".repeat(80);
        assert!(fit(&component(&long), 40).ends_with(ELLIPSIS));
    }

    #[test]
    fn composing_happens_before_the_length_budget() {
        // NFD spends two extra bytes per accent. A name that fits composed
        // must not be shortened for exceeding the cap decomposed.
        let long = "É".repeat(120); // 240 bytes NFD, 240 NFC — but 120 chars
        let out = component(&long);
        assert!(
            !out.contains(ELLIPSIS),
            "component does not shorten; fit does"
        );
        assert_eq!(out.chars().count(), 120);
        assert!(
            out.len() < component(&long.nfd().collect::<String>()).len() + 1,
            "the output is composed regardless of the input's form"
        );
    }

    /// **A move puts the file where the plan said, and takes the empty
    /// folder with it.**
    #[test]
    fn carrying_out_a_plan_moves_the_file_and_prunes_what_it_left() {
        let root = std::env::temp_dir().join("gmuse-storage-move");
        let _ = std::fs::remove_dir_all(&root);
        let was = root.join("General").join("Singles");
        std::fs::create_dir_all(&was).unwrap();
        let at = was.join("whatever.mp3");
        std::fs::write(&at, b"audio").unwrap();

        let plan = Plan {
            moves: vec![(
                Id::from_uuid(uuid::Uuid::from_u128(1)),
                at.clone(),
                PathBuf::from("An Artist/An Album/01 A Track.mp3"),
            )],
            ..Default::default()
        };
        let done = carry_out(&plan, &root, true);

        assert_eq!((done.moved, done.failed.len()), (1, 0));
        assert!(root.join("An Artist/An Album/01 A Track.mp3").exists());
        assert!(!at.exists(), "the file is still at its old path");
        assert!(!was.exists(), "the emptied folder was left behind");
        assert!(root.exists(), "the root was pruned away");
    }

    /// **What sits beside an album goes with it** — #149.
    ///
    /// Cover art belongs to the release, and moving the audio out from under
    /// it left 13 orphaned images in this library's old tree.
    #[test]
    fn a_cover_follows_the_album_it_belongs_to() {
        let root = std::env::temp_dir().join("gmuse-storage-assets");
        let _ = std::fs::remove_dir_all(&root);
        let was = root.join("General").join("An Artist").join("An Album");
        std::fs::create_dir_all(&was).unwrap();
        std::fs::write(was.join("01 A Track.mp3"), b"audio").unwrap();
        std::fs::write(was.join("cover.jpg"), b"art").unwrap();
        std::fs::write(was.join("rip.log"), b"log").unwrap();

        let plan = Plan {
            moves: vec![(
                Id::from_uuid(uuid::Uuid::from_u128(1)),
                was.join("01 A Track.mp3"),
                PathBuf::from("An Artist/An Album/01 A Track.mp3"),
            )],
            ..Default::default()
        };
        let done = carry_out(&plan, &root, true);

        assert_eq!((done.moved, done.assets), (1, 2));
        assert!(root.join("An Artist/An Album/cover.jpg").exists());
        assert!(root.join("An Artist/An Album/rip.log").exists());
        assert!(!was.exists(), "the emptied folder stayed behind");
    }

    /// **An import takes the audio and nothing else** — #239, and the rule the issue
    /// deferred.
    ///
    /// `shelf import --write` places from wherever it was pointed, so "the folder the
    /// audio has gone from" can be `~/Downloads` — and this once carried everything in
    /// it. Found the hard way: a scratch `config.toml` was quietly moved into an album
    /// folder three times in one session, and because a missing config makes
    /// `config::load` fall back to defaults *silently*, two verifications were pointed
    /// at the wrong library before the cause turned up.
    ///
    /// An allowlist of asset kinds fixed the worst of it. What fixes the rest is
    /// asking whose folder it is: a release folder is in the library, and nothing
    /// outside it is this program's to move or to delete.
    #[test]
    fn an_import_takes_the_audio_and_leaves_the_folder_as_it_found_it() {
        let root = std::env::temp_dir().join("gmuse-storage-strangers");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // A staging directory, outside the library, holding one track and a lot that
        // is nobody's business.
        let was = std::env::temp_dir().join("gmuse-storage-strangers-src");
        let _ = std::fs::remove_dir_all(&was);
        std::fs::create_dir_all(&was).unwrap();
        std::fs::write(was.join("01 A Track.mp3"), b"audio").unwrap();
        let beside = ["cover.jpg", "config.toml", "notes.pdf", "Thumbs.db"];
        for name in beside {
            std::fs::write(was.join(name), b"not the library's").unwrap();
        }

        let plan = Plan {
            moves: vec![(
                Id::from_uuid(uuid::Uuid::from_u128(1)),
                was.join("01 A Track.mp3"),
                PathBuf::from("An Artist/An Album/01 A Track.mp3"),
            )],
            ..Default::default()
        };
        let done = carry_out(&plan, &root, true);

        assert_eq!(
            (done.moved, done.assets, done.discarded),
            (1, 0, 0),
            "the audio moved and nothing else was touched"
        );
        for name in beside {
            assert!(
                was.join(name).exists(),
                "{name} was taken out of a folder that is not the library's"
            );
        }
        let _ = std::fs::remove_dir_all(&was);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **Inside the library, the cover follows and the junk is dropped** — #149 and
    /// #239 together.
    ///
    /// A `render` move takes a release from one place in the tree to another and both
    /// folders are the library's, so the cover has to follow — that is the whole of
    /// #149. What `desktop.ini` and `Thumbs.db` have instead is no destination worth
    /// having: a file browser writes another the next time it looks, and carrying one
    /// would only spread it.
    ///
    /// **The narrow exception to "a measurement marks a file and never moves it".**
    /// Nothing here is a copy of anything, which is why the rule that keeps a
    /// low-bitrate rip in place does not reach it.
    #[test]
    fn a_move_inside_the_library_carries_the_cover_and_drops_the_junk() {
        let root = std::env::temp_dir().join("gmuse-storage-junk");
        let _ = std::fs::remove_dir_all(&root);
        let was = root.join("General").join("An Artist").join("An Album");
        std::fs::create_dir_all(&was).unwrap();
        std::fs::write(was.join("01 A Track.mp3"), b"audio").unwrap();
        std::fs::write(was.join("cover.jpg"), b"art").unwrap();
        std::fs::write(was.join("rip.log"), b"log").unwrap();
        let junk = ["desktop.ini", "Thumbs.db", ".DS_Store", "._cover.jpg"];
        for name in junk {
            std::fs::write(was.join(name), b"the system wrote this").unwrap();
        }

        let plan = Plan {
            moves: vec![(
                Id::from_uuid(uuid::Uuid::from_u128(1)),
                was.join("01 A Track.mp3"),
                PathBuf::from("An Artist/An Album/01 A Track.mp3"),
            )],
            ..Default::default()
        };
        let done = carry_out(&plan, &root, true);
        let now = root.join("An Artist").join("An Album");

        assert_eq!(
            (done.moved, done.assets, done.discarded),
            (1, 2, 4),
            "one track, two of the release's own files, four the system left"
        );
        assert!(now.join("cover.jpg").exists(), "the cover did not follow");
        assert!(now.join("rip.log").exists(), "the log did not follow");
        for name in junk {
            assert!(
                !now.join(name).exists(),
                "{name} was carried into the album's new folder"
            );
        }
        assert!(
            !was.exists(),
            "the emptied folder stayed behind, so the junk was left in it"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// **A folder still holding music has not moved.** Two albums under one
    /// directory must not have the first one's move drag the second's cover.
    #[test]
    fn a_cover_stays_where_audio_remains() {
        let root = std::env::temp_dir().join("gmuse-storage-assets-stay");
        let _ = std::fs::remove_dir_all(&root);
        let was = root.join("Mixed");
        std::fs::create_dir_all(&was).unwrap();
        std::fs::write(was.join("moving.mp3"), b"audio").unwrap();
        std::fs::write(was.join("staying.mp3"), b"other").unwrap();
        std::fs::write(was.join("cover.jpg"), b"art").unwrap();

        let plan = Plan {
            moves: vec![(
                Id::from_uuid(uuid::Uuid::from_u128(1)),
                was.join("moving.mp3"),
                PathBuf::from("An Artist/An Album/01 A Track.mp3"),
            )],
            ..Default::default()
        };
        let done = carry_out(&plan, &root, true);

        assert_eq!(done.assets, 0, "a cover left a folder that still has music");
        assert!(was.join("cover.jpg").exists());
        assert!(was.join("staying.mp3").exists());
    }

    /// **Nothing is overwritten**, even by a plan that thought the path free —
    /// the tree may hold something the library has never seen.
    #[test]
    fn a_move_onto_something_that_exists_is_refused() {
        let root = std::env::temp_dir().join("gmuse-storage-clash");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("An Artist/An Album")).unwrap();
        let occupied = root.join("An Artist/An Album/01 A Track.mp3");
        std::fs::write(&occupied, b"someone else").unwrap();
        let from = root.join("elsewhere.mp3");
        std::fs::write(&from, b"mine").unwrap();

        let plan = Plan {
            moves: vec![(
                Id::from_uuid(uuid::Uuid::from_u128(1)),
                from.clone(),
                PathBuf::from("An Artist/An Album/01 A Track.mp3"),
            )],
            ..Default::default()
        };
        let done = carry_out(&plan, &root, true);

        assert_eq!(done.moved, 0);
        assert_eq!(done.failed.len(), 1);
        assert_eq!(std::fs::read(&occupied).unwrap(), b"someone else");
        assert!(from.exists(), "the file that could not move was lost");
    }

    /// A dry run says what it would do and touches nothing.
    #[test]
    fn a_plan_not_written_moves_nothing() {
        let root = std::env::temp_dir().join("gmuse-storage-dry");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let from = root.join("here.mp3");
        std::fs::write(&from, b"audio").unwrap();

        let plan = Plan {
            moves: vec![(
                Id::from_uuid(uuid::Uuid::from_u128(1)),
                from.clone(),
                PathBuf::from("An Artist/An Album/01 A Track.mp3"),
            )],
            ..Default::default()
        };
        let done = carry_out(&plan, &root, false);
        assert_eq!(done.moved, 1, "a dry run still says what it would do");
        assert!(from.exists());
        assert!(!root.join("An Artist").exists());
    }

    /// **The ordinary path, and every level of it comes from the library.**
    #[test]
    fn a_file_renders_under_its_credit_and_its_release() {
        let l = library(&[Tagged {
            path: "/m/wherever/x.flac".into(),
            artist: Some("Stephan Bodzin".into()),
            albumartist: Some("Stephan Bodzin & Marc Romboy".into()),
            album: Some("Luna".into()),
            title: Some("Triton".into()),
            tracknumber: Some(1),
            ..Default::default()
        }]);
        assert_eq!(
            only(&l),
            Ok(PathBuf::from(
                "Stephan Bodzin & Marc Romboy/Luna/01 Triton.flac"
            )),
            "the album credit files it, not the track's — a guest must not split an album"
        );
    }

    /// **No release, no album level.** A track on no album is ordinary, and
    /// there is no `Singles/` and no stand-in pretending otherwise.
    #[test]
    fn a_file_on_no_release_gets_no_album_level() {
        let l = library(&[Tagged {
            path: "/m/x.mp3".into(),
            artist: Some("Jeremie Albino".into()),
            title: Some("Rollin'".into()),
            ..Default::default()
        }]);
        assert_eq!(only(&l), Ok(PathBuf::from("Jeremie Albino/Rollin'.mp3")));
    }

    /// A separator in a title is not a directory. Replaced rather than
    /// dropped, so the words either side stay apart.
    #[test]
    fn a_forbidden_character_becomes_an_underscore() {
        let l = library(&[Tagged {
            path: "/m/x.mp3".into(),
            artist: Some("AC/DC".into()),
            album: Some("Who Made Who".into()),
            title: Some("D.T.: Part 1?".into()),
            tracknumber: Some(3),
            ..Default::default()
        }]);
        assert_eq!(
            only(&l),
            Ok(PathBuf::from("AC_DC/Who Made Who/03 D.T._ Part 1_.mp3"))
        );
    }

    /// **A name over the limit is shortened at a word boundary, and says so.**
    ///
    /// *When the Pawn…* is 444 characters of poem and held the Guinness record
    /// for the longest album title. The library is right to hold it whole and
    /// no filesystem will, so the path gives way — deliberately, at a space,
    /// with an ellipsis, rather than the mid-word stump this library already
    /// had.
    #[test]
    fn a_component_over_the_limit_is_shortened_at_a_word() {
        let l = library(&[Tagged {
            path: "/m/x.mp3".into(),
            artist: Some("Fiona Apple".into()),
            album: Some("When the Pawn Hits the Conflicts ".repeat(20)),
            title: Some("Fast As You Can".into()),
            tracknumber: Some(2),
            ..Default::default()
        }]);
        let at = only(&l).expect("a long name is not a refusal");
        let album = at.components().nth(1).expect("an album level");
        let album = album.as_os_str().to_string_lossy();
        assert!(album.len() <= COMPONENT_LIMIT, "{} bytes", album.len());
        assert!(
            album.ends_with(ELLIPSIS),
            "nothing says it was cut: {album}"
        );
        assert!(
            !album.trim_end_matches(ELLIPSIS).ends_with(' '),
            "cut left a trailing space: {album}"
        );
    }

    /// The cut lands on a space, so what is left reads as a phrase.
    #[test]
    fn shortening_cuts_at_a_word_and_never_mid_character() {
        assert_eq!(fit("short", 255), "short");
        assert_eq!(fit("one two three four", 12), "one two…");

        // **Never mid-character.** A budget landing inside a multi-byte char
        // would otherwise produce something that is not a string.
        let accented = "é".repeat(40);
        let cut = fit(&accented, 21);
        assert!(cut.len() <= 21, "{} bytes", cut.len());
        assert!(cut.ends_with(ELLIPSIS));

        // A single word longer than the budget has no boundary to cut at.
        let word = "a".repeat(300);
        assert!(fit(&word, 20).len() <= 20);
    }

    /// **The name's budget includes what goes around it**, or a title that
    /// just fits produces a filename that does not.
    #[test]
    fn a_track_number_and_an_extension_come_out_of_the_budget() {
        let l = library(&[Tagged {
            path: "/m/x.mp3".into(),
            artist: Some("An Artist".into()),
            album: Some("An Album".into()),
            title: Some("Word ".repeat(80)),
            tracknumber: Some(2),
            ..Default::default()
        }]);
        let at = only(&l).expect("renders");
        let name = at.file_name().expect("a name").to_string_lossy();
        assert!(name.len() <= COMPONENT_LIMIT, "{} bytes", name.len());
        assert!(name.ends_with(".mp3"), "{name}");
        assert!(name.starts_with("02 "), "{name}");
    }

    /// Windows refuses these whatever the extension, and this tree is read
    /// over SMB.
    #[test]
    fn a_reserved_name_is_refused() {
        let l = library(&[Tagged {
            path: "/m/x.mp3".into(),
            artist: Some("CON".into()),
            album: Some("An Album".into()),
            title: Some("A Track".into()),
            tracknumber: Some(1),
            ..Default::default()
        }]);
        assert_eq!(only(&l), Err(Refused::Reserved("CON".into())));
    }

    /// **A trailing dot goes and an internal one stays.** Windows cannot open
    /// `Inc./`, but `Manhattan Research, Inc.` is a name and the dot inside
    /// `D.T.` is meaning.
    #[test]
    fn a_trailing_dot_is_stripped_and_the_rest_are_kept() {
        assert_eq!(
            component("Manhattan Research, Inc."),
            "Manhattan Research, Inc"
        );
        assert_eq!(component("O.C.M.S."), "O.C.M.S");
        assert_eq!(component("  spaced  "), "spaced");
        assert_eq!(component("D.T.: Part 1"), "D.T._ Part 1");
    }

    /// **A path is typed, not printed** — #167.
    ///
    /// MusicBrainz catalogues the sleeve, and the sleeve is typeset: `Doin’
    /// Time` with U+2019, `Froggie Went a‐Courtin’` with U+2010. Neither is on
    /// anybody's keyboard, and a U+2010 in a name that looks exactly like a
    /// hyphen is a name no glob reaches. The library keeps the sleeve's
    /// spelling; the tree spells it plainly.
    #[test]
    fn a_typographic_mark_is_written_as_the_ascii_it_stands_for() {
        assert_eq!(component("Doin’ Time"), "Doin' Time");
        assert_eq!(
            component("Froggie Went a‐Courtin’"),
            "Froggie Went a-Courtin'"
        );
        assert_eq!(
            component("Feelin’ Single — Seein’ Double"),
            "Feelin' Single - Seein' Double"
        );
        assert_eq!(component("Live 1975–85"), "Live 1975-85");
        assert_eq!(component("40′"), "40'");
        // **Reversed 2026-10-25.** This kept `…`, because folding it to `...`
        // would meet the trailing-dot rule and vanish. It now vanishes on
        // purpose: an ellipsis is dropped from a name, leading or trailing or
        // between words (`an_ellipsis_is_dropped_and_no_name_starts_with_a_dot`).
        assert_eq!(component("When the Pawn…"), "When the Pawn");
        assert_eq!(
            component("Hard\u{a0}Times"),
            "Hard Times",
            "an invisible space cannot be typed at all"
        );
    }

    /// **A diacritic is spelling, and a curly quote is typesetting.**
    ///
    /// `’` and `'` are two renderings of one apostrophe, so choosing the
    /// typeable one loses nothing. `é` and `e` are different letters, and a
    /// tree that folded them would be misspelling its artists — which is the
    /// line the rule stops at.
    #[test]
    fn an_accent_is_not_a_typographic_mark() {
        assert_eq!(component("Sinéad O’Connor"), "Sinéad O'Connor");
        assert_eq!(component("Björk"), "Björk");
        assert_eq!(component("Beyoncé"), "Beyoncé");
        assert_eq!(component("Motörhead"), "Motörhead");
    }

    /// **Nothing is folded into a character this tree refuses.**
    ///
    /// `"` is in `FORBIDDEN` because the shares here will not open it, so
    /// folding `“ ” ″` would put four real titles on `_` — `“En Gallop”`, `No
    /// More “I Love You’s”`, `Waiting for Magic (Total remix 7″)`. Turning a
    /// character every filesystem here accepts into one none of them do is
    /// destruction dressed as normalisation.
    #[test]
    fn a_quote_stays_curly_because_the_straight_one_is_refused() {
        assert_eq!(component("“En Gallop”"), "“En Gallop”");
        assert_eq!(
            component("No More “I Love You’s”"),
            "No More “I Love You's”"
        );
        assert_eq!(
            component("Waiting for Magic (Total remix 7″)"),
            "Waiting for Magic (Total remix 7″)"
        );
        // And the straight one still goes, so the two are not silently equal.
        assert_eq!(component("Say \"Yes\""), "Say _Yes_");
    }

    /// **Folding happens before the budget is measured.**
    ///
    /// `’` is three bytes and `'` is one. Measuring first would shorten a name
    /// against bytes it is about to stop having, and `fit` cuts at a word — so
    /// the error is a whole word, not a character.
    #[test]
    fn the_budget_counts_the_bytes_the_name_will_have() {
        let curly = "’".repeat(90);
        assert_eq!(
            component(&curly).len(),
            90,
            "270 bytes of typeset apostrophe is 90 bytes of apostrophe"
        );
    }

    /// **Two copies wanting one path are both refused**, because moving one
    /// onto the other's name is how a file disappears. Neither moves and both
    /// are reported.
    #[test]
    fn two_copies_that_want_one_path_both_stay_put() {
        let same = |at: &str| Tagged {
            path: at.into(),
            artist: Some("An Artist".into()),
            album: Some("An Album".into()),
            title: Some("A Track".into()),
            tracknumber: Some(1),
            ..Default::default()
        };
        let l = library(&[same("/m/one/a.mp3"), same("/m/two/b.mp3")]);
        let plan = plan(&l, Path::new("/m"));
        assert_eq!(plan.moves.len(), 0, "one of them moved anyway");
        assert_eq!(plan.refused.len(), 2, "both should be reported");
        assert!(
            plan.refused
                .iter()
                .all(|(_, why)| matches!(why, Refused::Contested(_)))
        );
    }

    /// A file already where it belongs is not a move.
    /// **Retiring keeps the path beneath `.departed`** — §6, so a restore is
    /// a move back and not a puzzle. And it never deletes: §6 makes the two
    /// steps separate precisely because the first is reversible.
    #[test]
    fn retiring_carries_the_bytes_and_the_path_they_had() {
        let root = std::env::temp_dir().join("gmuse-retire");
        let _ = std::fs::remove_dir_all(&root);
        let album = root.join("Early James/Strange Time to Be Alive");
        std::fs::create_dir_all(&album).unwrap();
        let track = album.join("01 Racing to a Red Light.mp3");
        std::fs::write(&track, b"bytes").unwrap();
        std::fs::write(album.join("cover.jpg"), b"art").unwrap();

        let dry = retire(&root, std::slice::from_ref(&track), false);
        assert_eq!(dry.moved, 1);
        assert!(track.exists(), "a dry run moved a file");

        let done = retire(&root, std::slice::from_ref(&track), true);
        assert_eq!(done.moved, 1);
        assert!(!track.exists());
        let now = root
            .join(".departed/Early James/Strange Time to Be Alive/01 Racing to a Red Light.mp3");
        assert!(now.exists(), "the path beneath `.departed` was not kept");
        assert_eq!(std::fs::read(&now).unwrap(), b"bytes", "the bytes changed");
        assert!(
            root.join(".departed/Early James/Strange Time to Be Alive/cover.jpg")
                .exists(),
            "the cover went with the record, as it does on a render (#149)"
        );
        assert!(!album.exists(), "the folder the audio left was not pruned");
    }

    /// A file already under `.departed` is the album somebody retired by hand
    /// before `remove` existed. Its tombstone is the only thing it is missing,
    /// so there is nothing to move and nothing to refuse.
    #[test]
    fn a_file_already_departed_is_left_where_it_is() {
        let root = std::env::temp_dir().join("gmuse-retire-again");
        let _ = std::fs::remove_dir_all(&root);
        let at = root.join(".departed/A/B/01 t.mp3");
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, b"bytes").unwrap();

        let done = retire(&root, std::slice::from_ref(&at), true);
        assert_eq!(done.moved, 0);
        assert!(done.failed.is_empty(), "{:?}", done.failed);
        assert!(at.exists());
    }

    #[test]
    fn a_file_in_the_right_place_is_settled() {
        let l = library(&[Tagged {
            path: "/m/An Artist/An Album/01 A Track.mp3".into(),
            artist: Some("An Artist".into()),
            album: Some("An Album".into()),
            title: Some("A Track".into()),
            tracknumber: Some(1),
            ..Default::default()
        }]);
        let plan = plan(&l, Path::new("/m"));
        assert_eq!((plan.settled, plan.moves.len()), (1, 0));
    }

    /// And one in the wrong place says exactly where it would go.
    #[test]
    fn a_file_in_the_wrong_place_is_a_move() {
        let l = library(&[Tagged {
            path: "/m/Dance/Singles/whatever.mp3".into(),
            artist: Some("An Artist".into()),
            album: Some("An Album".into()),
            title: Some("A Track".into()),
            tracknumber: Some(1),
            ..Default::default()
        }]);
        let plan = plan(&l, Path::new("/m"));
        assert_eq!(plan.settled, 0);
        assert_eq!(
            plan.moves
                .first()
                .map(|(_, from, to)| (from.clone(), to.clone())),
            Some((
                PathBuf::from("/m/Dance/Singles/whatever.mp3"),
                PathBuf::from("An Artist/An Album/01 A Track.mp3")
            ))
        );
    }

    /// One track of an album #322 is about: a composer's credit, a work's
    /// title, and the album named by its own MusicBrainz group.
    fn dido(path: &str, group: u128, first: Option<i32>, track: u32, title: &str) -> Tagged {
        Tagged {
            path: path.into(),
            artist: Some("Henry Purcell".into()),
            albumartist: Some("Henry Purcell".into()),
            album: Some("Dido and Aeneas".into()),
            title: Some(title.into()),
            tracknumber: Some(track),
            mb_release_group: Some(uuid::Uuid::from_u128(group)),
            first_released: first,
            ..Default::default()
        }
    }

    /// Where each copy goes, sorted, so a test can say the set.
    fn placed(l: &Librarian, copies: &[Id<Encoding>]) -> Vec<PathBuf> {
        let mut at: Vec<PathBuf> = layout_of(l, copies)
            .placed
            .into_iter()
            .map(|(_, at)| at)
            .collect();
        at.sort();
        at
    }

    /// How many albums the library's copies are filed on — counted from the
    /// copies' slots, not from anything the layout decided.
    fn albums(l: &Librarian) -> usize {
        l.every_copy()
            .into_iter()
            .filter_map(|c| l.group_of(l.placed_at(c)))
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// **Two albums with one title under one credit get a folder each** — #322.
    ///
    /// Their file names differ, so `Contested` never fired and both landed in
    /// `Henry Purcell/Dido and Aeneas/` without a word. The year they first
    /// appeared is what tells two performances of one work apart.
    #[test]
    fn two_albums_with_one_title_are_told_apart_by_their_years() {
        let l = library(&[
            dido(
                "/m/a.ogg",
                0xd1d0,
                Some(1986),
                15,
                "When I am laid in earth",
            ),
            dido("/m/b.mp3", 0xd1d1, Some(1994), 27, "Thy hand, Belinda"),
        ]);
        assert_eq!(
            albums(&l),
            2,
            "the fixture has to be two albums for this to test anything"
        );
        assert_eq!(
            placed(&l, &l.every_copy()),
            vec![
                PathBuf::from(
                    "Henry Purcell/Dido and Aeneas (1986)/15 When I am laid in earth.ogg"
                ),
                PathBuf::from("Henry Purcell/Dido and Aeneas (1994)/27 Thy hand, Belinda.mp3"),
            ]
        );
    }

    /// **Only what separates them, and only where something must.** The single
    /// says what it is and the album it is named after keeps its plain name —
    /// so does an album with no rival, which never sees a year.
    #[test]
    fn a_single_named_after_its_album_says_so_and_the_album_keeps_its_name() {
        let single = Tagged {
            release_kind: ReleaseType::Single,
            ..dido("/m/s.mp3", 0x5146, Some(1994), 1, "When I am laid in earth")
        };
        let l = library(&[
            dido("/m/a.mp3", 0xa1b0, Some(1994), 2, "Shake the cloud"),
            single,
            Tagged {
                album: Some("The Fairy-Queen".into()),
                ..dido("/m/f.mp3", 0xfa1e, Some(1990), 1, "Overture")
            },
        ]);
        assert_eq!(albums(&l), 3);
        assert_eq!(
            placed(&l, &l.every_copy()),
            vec![
                PathBuf::from("Henry Purcell/Dido and Aeneas/02 Shake the cloud.mp3"),
                PathBuf::from(
                    "Henry Purcell/Dido and Aeneas (Single)/01 When I am laid in earth.mp3"
                ),
                PathBuf::from("Henry Purcell/The Fairy-Queen/01 Overture.mp3"),
            ]
        );
    }

    /// **Rivals are found case-insensitively**, because APFS and SMB fold case:
    /// two albums whose titles differ only in capitals want one folder there,
    /// whatever their bytes say. Told apart by their years where they have
    /// them, and refused where they do not.
    #[test]
    fn two_albums_whose_titles_differ_only_in_case_are_rivals() {
        let shouted = |first| {
            library(&[
                dido("/m/a.mp3", 0xd1d0, first, 1, "Overture"),
                Tagged {
                    album: Some("Dido And Aeneas".into()),
                    ..dido(
                        "/m/b.mp3",
                        0xd1d1,
                        first.map(|y| y + 8),
                        2,
                        "Shake the cloud",
                    )
                },
            ])
        };
        let dated = shouted(Some(1986));
        assert_eq!(albums(&dated), 2);
        assert_eq!(
            placed(&dated, &dated.every_copy()),
            vec![
                PathBuf::from("Henry Purcell/Dido And Aeneas (1994)/02 Shake the cloud.mp3"),
                PathBuf::from("Henry Purcell/Dido and Aeneas (1986)/01 Overture.mp3"),
            ]
        );

        let undated = shouted(None);
        let layout = layout_of(&undated, &undated.every_copy());
        assert_eq!(
            layout.refused.len(),
            2,
            "both are reported: {:?}",
            layout.placed
        );
        assert!(
            layout
                .refused
                .iter()
                .all(|(_, r)| matches!(r, Refused::Shared(_))),
            "{:?}",
            layout.refused
        );
    }

    /// **What nothing tells apart is refused, both sides** — the same answer
    /// `Contested` gives one level down. A folder of two albums is the failure,
    /// and a guess at which keeps the name is a guess.
    #[test]
    fn two_albums_nothing_tells_apart_are_refused_both() {
        let l = library(&[
            dido("/m/a.mp3", 0xd1d0, Some(1994), 1, "Overture"),
            dido("/m/b.mp3", 0xd1d1, Some(1994), 2, "Shake the cloud"),
        ]);
        assert_eq!(albums(&l), 2);
        let layout = layout_of(&l, &l.every_copy());
        assert!(
            layout.placed.is_empty(),
            "neither is placed: {:?}",
            layout.placed
        );
        assert_eq!(layout.refused.len(), 2, "both are reported, not just one");
        assert!(
            layout
                .refused
                .iter()
                .all(|(_, r)| *r == Refused::Shared("Henry Purcell/Dido and Aeneas".into())),
            "{:?}",
            layout.refused
        );
    }

    /// **Two pressings of one album still land together**, which `{album}`
    /// being the group's title is for. Only *albums* are rivals.
    #[test]
    fn two_pressings_of_one_album_still_share_its_folder() {
        let l = library(&[
            Tagged {
                mb_release: Some(uuid::Uuid::from_u128(0x1986)),
                ..dido("/m/a.mp3", 0xd1d0, Some(1986), 1, "Overture")
            },
            Tagged {
                mb_release: Some(uuid::Uuid::from_u128(0x2001)),
                ..dido("/m/b.mp3", 0xd1d0, Some(1986), 2, "Shake the cloud")
            },
        ]);
        let pressings: BTreeSet<_> = l
            .every_copy()
            .into_iter()
            .filter_map(|c| l.release_of(l.placed_at(c)))
            .collect();
        assert_eq!(pressings.len(), 2, "the fixture has to be two pressings");
        assert_eq!(
            placed(&l, &l.every_copy()),
            vec![
                PathBuf::from("Henry Purcell/Dido and Aeneas/01 Overture.mp3"),
                PathBuf::from("Henry Purcell/Dido and Aeneas/02 Shake the cloud.mp3"),
            ]
        );
    }

    /// One track of the album #375 is about: two pressings in one group, the
    /// same tracklist under the pressing's own title.
    fn celts(path: &str, release: u128, album: &str, year: i32, track: u32, title: &str) -> Tagged {
        Tagged {
            path: path.into(),
            artist: Some("Enya".into()),
            albumartist: Some("Enya".into()),
            album: Some(album.into()),
            title: Some(title.into()),
            tracknumber: Some(track),
            date: Some(year),
            mb_release_group: Some(uuid::Uuid::from_u128(0xce175)),
            mb_release: Some(uuid::Uuid::from_u128(release)),
            ..Default::default()
        }
    }

    /// How many pressings the library's copies sit on — counted from their
    /// slots, not from anything the layout decided.
    fn pressings(l: &Librarian) -> usize {
        l.every_copy()
            .into_iter()
            .filter_map(|c| l.release_of(l.placed_at(c)))
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// **Two pressings whose files collide get a folder each** — #375. One
    /// tracklist under two titles made every track `Contested`, and both sides
    /// stayed where they were: the album in two folders anyway.
    #[test]
    fn two_pressings_whose_files_collide_are_told_apart_by_their_titles() {
        let l = library(&[
            celts("/m/a.mp3", 0x1992, "The Celts", 1992, 1, "The Celts"),
            celts("/m/b.mp3", 0x1988, "Enya", 1988, 1, "The Celts"),
        ]);
        assert_eq!(albums(&l), 1, "the fixture has to be one album");
        assert_eq!(pressings(&l), 2, "and two pressings of it");
        let album = l.spine.groups.values().next().unwrap().title.clone();
        let other = if album == "Enya" { "The Celts" } else { "Enya" };
        assert_eq!(
            placed(&l, &l.every_copy()),
            vec![
                PathBuf::from(format!("Enya/{album}/01 The Celts.mp3")),
                PathBuf::from(format!("Enya/{album} ({other})/01 The Celts.mp3")),
            ]
        );
    }

    /// **The year, where the titles are one** — and both are qualified,
    /// because neither is the album's more than the other.
    #[test]
    fn two_pressings_with_one_title_are_told_apart_by_their_years() {
        let l = library(&[
            celts("/m/a.mp3", 0x1992, "The Celts", 1992, 1, "The Celts"),
            celts("/m/b.mp3", 0x2009, "The Celts", 2009, 1, "The Celts"),
        ]);
        assert_eq!(pressings(&l), 2, "the fixture has to be two pressings");
        assert_eq!(
            placed(&l, &l.every_copy()),
            vec![
                PathBuf::from("Enya/The Celts (1992)/01 The Celts.mp3"),
                PathBuf::from("Enya/The Celts (2009)/01 The Celts.mp3"),
            ]
        );
    }

    /// **And where nothing separates them, both stay put**, as two copies
    /// wanting one path always have.
    #[test]
    fn two_pressings_nothing_tells_apart_are_refused_both() {
        let l = library(&[
            celts("/m/a.mp3", 0x1992, "The Celts", 1992, 1, "The Celts"),
            celts("/m/b.mp3", 0x1993, "The Celts", 1992, 1, "The Celts"),
        ]);
        assert_eq!(pressings(&l), 2, "the fixture has to be two pressings");
        let layout = layout_of(&l, &l.every_copy());
        assert_eq!(layout.placed, vec![]);
        assert_eq!(layout.refused.len(), 2);
        assert!(
            layout
                .refused
                .iter()
                .all(|(_, r)| *r == Refused::Pressed("Enya/The Celts".into())),
            "{:?}",
            layout.refused
        );
    }

    /// **Only the pressings that collide.** A third whose names are its own
    /// stays in the album's folder beside the one that kept it.
    #[test]
    fn a_pressing_that_collides_with_nothing_keeps_the_albums_folder() {
        let l = library(&[
            celts("/m/a.mp3", 0x1992, "The Celts", 1992, 1, "The Celts"),
            celts("/m/b.mp3", 0x1988, "The Celts", 1988, 1, "The Celts"),
            celts("/m/c.mp3", 0x2009, "The Celts", 2009, 16, "Bonus"),
        ]);
        assert_eq!(pressings(&l), 3, "the fixture has to be three pressings");
        assert_eq!(
            placed(&l, &l.every_copy()),
            vec![
                PathBuf::from("Enya/The Celts/16 Bonus.mp3"),
                PathBuf::from("Enya/The Celts (1988)/01 The Celts.mp3"),
                PathBuf::from("Enya/The Celts (1992)/01 The Celts.mp3"),
            ]
        );
    }

    /// **A pressing held nowhere here collides with nothing.** Its copies have
    /// no file on this device, so qualifying the one that does would move it
    /// out of its album's folder for a collision that cannot happen — what six
    /// James Blake and The Bug files did on the live library before this was
    /// counted the way `layout_of` counts a contest.
    #[test]
    fn a_pressing_held_elsewhere_does_not_move_the_one_held_here() {
        let here = celts("/m/a.mp3", 0x1992, "The Celts", 1992, 1, "The Celts");
        let away = celts("/m/b.mp3", 0x1988, "The Celts", 1988, 1, "The Celts");
        let mut both = Holdings::new();
        both.upsert(here.clone());
        both.upsert(away.clone());
        let mut held = Holdings::new();
        held.upsert(here);
        let mut log = crate::model::instance::Instance::new(crate::fixture::device());
        log.ingest(100, &both.tags());
        let mut l = crate::bridge::librarian(&held, log.records(), crate::fixture::device(), 100);
        l.here(crate::fixture::device());

        assert_eq!(pressings(&l), 2, "both pressings have to be catalogued");
        let unheld = l
            .every_copy()
            .into_iter()
            .filter(|&c| l.where_held(c).is_none())
            .count();
        assert_eq!(unheld, 1, "and one of them held nowhere here");
        assert_eq!(
            placed(&l, &l.every_copy()),
            vec![PathBuf::from("Enya/The Celts/01 The Celts.mp3")]
        );
    }

    /// **The rival can already be in the library.** An import lays out only
    /// the files it took in, and the album it collides with is not one of them
    /// — which is the *Dido* import exactly: four performances arriving beside
    /// one already shelved.
    #[test]
    fn an_import_is_told_apart_from_an_album_already_held() {
        let l = library(&[
            dido("/m/held.ogg", 0xd1d0, Some(1986), 16, "With drooping wings"),
            dido("/m/new.mp3", 0xd1d1, Some(1995), 39, "With drooping wings"),
        ]);
        let arriving: Vec<Id<Encoding>> = l
            .every_copy()
            .into_iter()
            .filter(|&c| l.where_held(c).is_some_and(|at| at.ends_with("new.mp3")))
            .collect();
        assert_eq!(arriving.len(), 1, "the import is one file of the two");
        assert_eq!(
            placed(&l, &arriving),
            vec![PathBuf::from(
                "Henry Purcell/Dido and Aeneas (1995)/39 With drooping wings.mp3"
            )]
        );
    }
}
