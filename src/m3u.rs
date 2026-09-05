//! Writing `.m3u8` — issue #22, narrowed by #118.
//!
//! Not a format of our own. A playlist **is** an ordered list of paths, which
//! is exactly what M3U is, and M3U is what everything reads — VLC,
//! foobar2000, Plex, beets, cmus. XSPF is technically better and never got
//! adopted; PLS is a distant third. Inventing one would buy nothing and cost
//! every other program's ability to open the file.
//!
//! We write `.m3u8` — the `8` declares UTF-8, which removes the one genuine
//! ambiguity in an otherwise unspecified format.
//!
//! ## Export only, and the reading half is deleted
//!
//! This used to be the playlist's **storage**, which made a path authoritative
//! for what a list contained: save here, open on another machine, resolve
//! nothing. A playlist is an ordered collection of recordings now (#118), and
//! m3u is how one leaves the building — for a DJ program, a car stereo,
//! anything that reads the standard format.
//!
//! Nothing reads one back. That is enforced by there being no `read`: an
//! exported file going stale costs nothing, and cannot re-key a list by
//! location.
//!
//! ```text
//! #EXTM3U
//! #EXTINF:211,Sierra Ferrell - The Sea
//! ../General/Sierra Ferrell/Long Time Coming/01 The Sea.mp3
//! ```
//!
//! **`#EXTINF` is write-only.** We emit it because other programs use it, and
//! ignore it on read because our own tags are better and a metadata cache in a
//! text file goes stale the moment anything is retagged.
//!
//! **Paths are relative to the playlist file** where they can be, so a library
//! and its playlists move together and survive being copied to another
//! machine — which is the whole reason for putting them under `data_dir`.

use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum M3uError {
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// One line of an export: where the file is, and what to say about it.
///
/// **The one place the player is allowed to hold a location** (#86). A row
/// carries no path by design — that omission is what stopped `history.toml`
/// being keyed by one — but an `.m3u` is a path format by definition, and an
/// export naming paths is not a leak. So the locator is asked of
/// [`Listing::playable`](crate::model::rows::Listing::playable) at the moment
/// of writing and is never kept.
///
/// Assembled by the caller rather than taken as a row id, because the two
/// halves come from different questions: what to *say* is a row, and where the
/// file *is* is a holding.
pub struct Entry<'a> {
    pub locator: &'a Path,
    /// The track's own credit where it has one, else who it files under.
    pub who: &'a str,
    pub title: &'a str,
    pub duration: std::time::Duration,
}

/// Render entries as an `.m3u8` body.
///
/// `at` is where the file will live; paths are made relative to its directory.
pub fn render(at: &Path, entries: &[Entry<'_>]) -> String {
    let base = at.parent().unwrap_or(Path::new(""));
    let mut out = String::from("#EXTM3U\n");
    for e in entries {
        // Seconds, rounded — the format has no room for more, and nothing
        // reads it back anyway.
        let secs = e.duration.as_secs();
        let _ = writeln!(out, "#EXTINF:{secs},{} - {}", e.who, e.title);
        let _ = writeln!(out, "{}", relative_to(base, e.locator).display());
    }
    out
}

/// Write a playlist, creating the directory if needed.
///
/// Temp-then-rename, so an interrupted write leaves the previous playlist
/// intact rather than a truncated one — the same rule the history follows.
pub fn write(at: &Path, entries: &[Entry<'_>]) -> Result<(), M3uError> {
    let fail = |source| M3uError::Write {
        path: at.to_path_buf(),
        source,
    };
    if let Some(dir) = at.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let tmp = at.with_extension("tmp");
    std::fs::write(&tmp, render(at, entries)).map_err(fail)?;
    std::fs::rename(&tmp, at).map_err(fail)
}

/// `path` expressed relative to `base`, or absolute when that is not possible.
///
/// Falls back to absolute across a drive or root boundary rather than
/// producing a chain of `..` that leaves the filesystem — a playlist that
/// cannot be made relative is still a working playlist.
fn relative_to(base: &Path, path: &Path) -> PathBuf {
    let b: Vec<Component<'_>> = base.components().collect();
    let p: Vec<Component<'_>> = path.components().collect();
    let shared = b.iter().zip(&p).take_while(|(x, y)| x == y).count();

    // Sharing nothing but the root means the two are on unrelated branches —
    // a different volume, say. A chain of `..` back to the root would still
    // resolve, but it is fragile and unreadable, and an absolute path is the
    // honest answer.
    let only_root =
        shared == 0 || (shared == 1 && matches!(b[0], Component::RootDir | Component::Prefix(_)));
    if only_root && path.is_absolute() {
        return path.to_path_buf();
    }

    let mut out = PathBuf::new();
    for _ in shared..b.len() {
        out.push("..");
    }
    for c in &p[shared..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        path.to_path_buf()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// One entry, as the player assembles it from a row and a holding.
    fn entry<'a>(path: &'a str, who: &'a str, title: &'a str, secs: u64) -> Entry<'a> {
        Entry {
            locator: Path::new(path),
            who,
            title,
            duration: Duration::from_secs(secs),
        }
    }

    #[test]
    fn a_playlist_is_extm3u_with_relative_paths() {
        let t = entry(
            "/m/General/Sierra Ferrell/Long Time Coming/01 The Sea.mp3",
            "Sierra Ferrell",
            "The Sea",
            211,
        );
        let out = render(Path::new("/m/.gmuse/playlists/road.m3u8"), &[t]);
        assert_eq!(
            out,
            "#EXTM3U\n\
             #EXTINF:211,Sierra Ferrell - The Sea\n\
             ../../General/Sierra Ferrell/Long Time Coming/01 The Sea.mp3\n"
        );
    }

    #[test]
    fn a_relative_path_is_written_without_touching_the_filesystem() {
        // `canonicalize` would fail here; a track on an unmounted drive must
        // still be written rather than vanish.
        assert_eq!(
            // `render` passes the playlist's *parent*, which is what a path in
            // the file is relative to.
            relative_to(Path::new("/m/.gmuse"), Path::new("/m/Dance/x.mp3")),
            PathBuf::from("../Dance/x.mp3")
        );
    }

    /// A path with nothing in common stays absolute rather than becoming a
    /// chain of `..` that climbs out of the filesystem.
    #[test]
    fn an_unrelated_path_stays_absolute() {
        let t = entry("/other/volume/x.mp3", "X", "X", 10);
        let out = render(Path::new("/m/.gmuse/p.m3u8"), &[t]);
        assert!(out.contains("\n/other/volume/x.mp3\n"), "{out}");
    }

    /// **The fallback moved, and this test moved with it.**
    ///
    /// It used to build a `Track` with no artist and assert that `#EXTINF`
    /// said `Unknown Artist`. An entry takes `who` already resolved, because
    /// the caller has the row and the row has the answer: `file_under` is
    /// never empty — the librarian substitutes `UNKNOWN_ARTIST` — and the
    /// call site reads `row.artist.unwrap_or(row.file_under)`.
    ///
    /// So what is left to prove here is the narrower thing this file is still
    /// responsible for: whatever it is given ends up in the line.
    #[test]
    fn the_credit_it_is_given_is_the_credit_it_writes() {
        let t = entry(
            "/m/x.mp3",
            crate::model::seam::UNKNOWN_ARTIST,
            "Untitled",
            5,
        );
        let out = render(Path::new("/m/p.m3u8"), &[t]);
        assert!(out.contains("#EXTINF:5,Unknown Artist - Untitled"), "{out}");
    }

    #[test]
    fn writing_is_atomic_and_round_trips_on_disk() {
        let d = std::env::temp_dir().join("gmuse-m3u-write");
        let _ = std::fs::remove_dir_all(&d);
        let at = d.join("playlists/mix.m3u8");
        let t = entry("/m/a.mp3", "A", "One", 100);

        write(&at, &[t]).unwrap();
        assert!(at.exists(), "the directory was not created");
        assert!(!at.with_extension("tmp").exists(), "a temp file was left");
        // Read back as text, not through a parser: there isn't one any more,
        // and that is the point (#118) — nothing here reads an m3u as
        // authority for what a list contains.
        let written = std::fs::read_to_string(&at).unwrap();
        assert!(written.contains("/m/a.mp3"), "{written}");
    }
}
