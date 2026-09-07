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
//! ## What is not checked yet
//!
//! **Unicode normalisation.** §5 says NFC on write, and macOS hands out
//! decomposed names while Linux does not, so the same album can be two
//! directories. Doing it needs `unicode-normalization`, and a dependency needs
//! a Decision Log entry — so it is stated here as absent rather than half-done.
//! Everything below compares raw bytes, which is right for collisions and
//! silent about composition.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::model::entity::Encoding;
use crate::model::id::Id;
use crate::model::rows::Listing;
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

/// One component, with the characters a filesystem refuses taken out.
///
/// **Replaced rather than dropped**, so a title does not silently lose a word
/// boundary. A trailing dot or space is stripped because Windows cannot open a
/// name that has one — and only there, since `Manhattan Research, Inc.` is a
/// name and the dot is meaning rather than noise.
fn component(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if FORBIDDEN.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    cleaned.trim().trim_end_matches(['.', ' ']).to_string()
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
    let row = Listing::listed(l, copy)?;
    let format = l.encoding(copy)?.format;

    let under = component(row.file_under);
    // **`album_if_named`, not `album`.** The latter stands in `No Album` so a
    // tree node always has a label; rendering that would make a directory
    // called "No Album" the answer to "what release is this on".
    let album = row.album_if_named.map(component).filter(|a| !a.is_empty());
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

    let name = format!("{lead}{title}.{}", format.extension());
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
    plan_of(l, root, &Listing::rows(l))
}

/// The same, over a chosen set — what an import plans for the files it just
/// took in, rather than for the whole library.
pub fn plan_of(l: &Librarian, root: &Path, copies: &[Id<Encoding>]) -> Plan {
    let mut wanted: BTreeMap<String, Vec<(Id<Encoding>, PathBuf)>> = BTreeMap::new();
    let mut out = Plan::default();

    for copy in copies.iter().copied() {
        let Some(here) = l.where_held(copy) else {
            out.elsewhere += 1;
            continue;
        };
        match proposed(l, copy) {
            None => out.elsewhere += 1,
            Some(Err(refused)) => out.refused.push((copy, refused)),
            Some(Ok(at)) => {
                // **Case-insensitively**, because APFS and SMB fold case: two
                // artists differing only in capitals are one directory, and
                // finding that out during a move is finding out too late.
                let key = at.to_string_lossy().to_lowercase();
                let from = PathBuf::from(here);
                wanted.entry(key).or_default().push((copy, from));
                out.moves.push((copy, PathBuf::new(), at));
            }
        }
    }

    // Second pass, now that every wanted path is known.
    let mut settled = 0;
    let mut moves = Vec::new();
    for (copy, _, to) in std::mem::take(&mut out.moves) {
        let key = to.to_string_lossy().to_lowercase();
        let holders = wanted.get(&key).map(Vec::len).unwrap_or(0);
        if holders > 1 {
            out.refused.push((copy, Refused::Contested(to)));
            continue;
        }
        let from = l.where_held(copy).map(PathBuf::from).unwrap_or_default();
        if from == root.join(&to) {
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
            out.assets += carry_assets(was, now);
        }
    }
    for was in followed.keys() {
        out.emptied += prune(Some(was), root);
    }
    out
}

/// Move whatever is left in a folder the audio has gone from.
///
/// **Only when no audio remains**, because a folder still holding music is a
/// folder that has not moved: two albums under one directory would otherwise
/// have the first one's move drag the second one's cover away.
fn carry_assets(was: &Path, now: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(was) else {
        return 0;
    };
    let left: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    if left.iter().any(|p| crate::scan::is_audio(p)) {
        return 0;
    }
    let mut moved = 0;
    for asset in left {
        let Some(name) = asset.file_name() else {
            continue;
        };
        let to = now.join(name);
        if !to.exists() && std::fs::rename(&asset, &to).is_ok() {
            moved += 1;
        }
    }
    moved
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
    use crate::scan::Holdings;

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
        let copy = Listing::rows(l)[0];
        proposed(l, copy).expect("a fixture row renders")
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

    /// **A name over the limit is refused, never truncated.**
    ///
    /// This library already holds the failure: Fiona Apple's album folder is
    /// cut mid-word, and the fragment is not something anybody would search
    /// for. The file stays put and the row goes to a person.
    #[test]
    fn a_component_over_the_limit_is_refused_rather_than_cut() {
        let l = library(&[Tagged {
            path: "/m/x.mp3".into(),
            artist: Some("Fiona Apple".into()),
            album: Some("When the Pawn ".repeat(30)),
            title: Some("Fast As You Can".into()),
            tracknumber: Some(2),
            ..Default::default()
        }]);
        assert!(
            matches!(only(&l), Err(Refused::TooLong(_))),
            "a 420-byte album name was accepted"
        );
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
