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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use unicode_normalization::UnicodeNormalization;

use crate::model::entity::Encoding;
use crate::model::id::Id;
use crate::model::seam::Librarian;

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
    cleaned.trim().trim_end_matches(['.', ' ']).nfc().collect()
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

/// Where this copy belongs, relative to the library root.
///
/// `None` for a copy the library cannot describe — it has no row, so there is
/// nothing to render from.
pub fn proposed(l: &Librarian, copy: Id<Encoding>) -> Option<Result<PathBuf, Refused>> {
    // **A row is an appearance** (#155) and this renders a *file*, so the file
    // names its row rather than being one.
    let format = l.encoding(copy)?.format;
    let row = l.describe(l.row_of_copy(copy)?)?;

    let under = fit(&component(row.file_under), COMPONENT_LIMIT);
    // **`album_if_named`, not `album`.** The latter stands in `No Album` so a
    // tree node always has a label; rendering that would make a directory
    // called "No Album" the answer to "what release is this on".
    let album = row
        .album_if_named
        .map(component)
        .filter(|a| !a.is_empty())
        .map(|a| fit(&a, COMPONENT_LIMIT));
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
    let name = format!("{lead}{}.{ext}", fit(&title, room));
    let mut parts = vec![under];
    parts.extend(album);
    parts.push(name);

    for part in &parts {
        if let Err(refused) = check(part) {
            return Some(Err(refused));
        }
    }
    // A title of nothing leaves a name that is only an extension.
    if title.is_empty() {
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

    for copy in copies.iter().copied() {
        if l.where_held(copy).is_none() {
            out.elsewhere += 1;
            continue;
        }
        match proposed(l, copy) {
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
        proposed(l, copy).expect("a fixture row renders")
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

    #[test]
    fn two_recordings_whose_names_differ_only_in_form_contest_one_path() {
        // The filesystem is form-insensitive, so these land in one directory.
        // A byte-comparing check reports no contest and finds out during the
        // move, which is too late.
        let l = library(&[
            Tagged {
                path: "/m/a.flac".into(),
                artist: Some(NFD.into()),
                album: Some("The Lion and the Cobra".into()),
                title: Some("Troy".into()),
                tracknumber: Some(1),
                ..Default::default()
            },
            Tagged {
                path: "/m/b.flac".into(),
                artist: Some(NFC.into()),
                album: Some("The Lion and the Cobra".into()),
                title: Some("Troy".into()),
                tracknumber: Some(1),
                ..Default::default()
            },
        ]);
        let plan = plan(&l, Path::new("/m"));
        assert_eq!(
            plan.moves.len(),
            0,
            "neither moves while the contest stands"
        );
        assert!(
            plan.refused
                .iter()
                .all(|(_, r)| matches!(r, Refused::Contested(_))),
            "both are refused as contested, not silently merged: {:?}",
            plan.refused
        );
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
        assert_eq!(
            component("When the Pawn…"),
            "When the Pawn…",
            "an ellipsis impersonates nothing, and folding it would meet the \
             trailing-dot rule and be stripped away entirely"
        );
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
}
