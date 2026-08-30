//! Reading and writing `.m3u8` — issue #22.
//!
//! Not a format of our own. A playlist **is** an ordered list of paths, which
//! is exactly what M3U is, and M3U is what everything reads — VLC,
//! foobar2000, Plex, beets, cmus. XSPF is technically better and never got
//! adopted; PLS is a distant third. Inventing one would buy nothing and cost
//! every other program's ability to open the file.
//!
//! We write `.m3u8` — the `8` declares UTF-8, which removes the one genuine
//! ambiguity in an otherwise unspecified format — and read `.m3u`, `.m3u8` and
//! cmus's `.pl`, the last being path-per-line with no `#` lines and therefore
//! a strict subset.
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

use crate::store::Track;

/// Extensions we will open as a playlist.
pub const EXTENSIONS: &[&str] = &["m3u8", "m3u", "pl"];

/// What we write.
pub const EXTENSION: &str = "m3u8";

#[derive(Debug, thiserror::Error)]
pub enum M3uError {
    #[error("reading {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("writing {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

/// Render tracks as an `.m3u8` body.
///
/// `at` is where the file will live; paths are made relative to its directory.
pub fn render(at: &Path, tracks: &[&Track]) -> String {
    let base = at.parent().unwrap_or(Path::new(""));
    let mut out = String::from("#EXTM3U\n");
    for t in tracks {
        // Seconds, rounded — the format has no room for more, and nothing
        // reads it back anyway.
        let secs = t.duration.as_secs();
        let who = t.artist.as_deref().unwrap_or(t.file_under());
        let _ = writeln!(out, "#EXTINF:{secs},{who} - {}", t.display_title());
        let _ = writeln!(out, "{}", relative_to(base, &t.path).display());
    }
    out
}

/// Write a playlist, creating the directory if needed.
///
/// Temp-then-rename, so an interrupted write leaves the previous playlist
/// intact rather than a truncated one — the same rule the history follows.
pub fn write(at: &Path, tracks: &[&Track]) -> Result<(), M3uError> {
    let fail = |source| M3uError::Write {
        path: at.to_path_buf(),
        source,
    };
    if let Some(dir) = at.parent() {
        std::fs::create_dir_all(dir).map_err(fail)?;
    }
    let tmp = at.with_extension("tmp");
    std::fs::write(&tmp, render(at, tracks)).map_err(fail)?;
    std::fs::rename(&tmp, at).map_err(fail)
}

/// The paths a playlist names, in order.
///
/// Comments and blank lines are skipped, which is what makes `#EXTINF`
/// write-only and what lets a `.pl` — path-per-line, no `#` at all — be read
/// by the same code.
pub fn parse(at: &Path, text: &str) -> Vec<PathBuf> {
    let base = at.parent().unwrap_or(Path::new(""));
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| {
            let p = Path::new(l);
            if p.is_absolute() {
                p.to_path_buf()
            } else {
                // Relative to the playlist, not to the process's working
                // directory — a playlist opened from anywhere must resolve the
                // same way.
                normalise(&base.join(p))
            }
        })
        .collect()
}

pub fn read(at: &Path) -> Result<Vec<PathBuf>, M3uError> {
    let text = std::fs::read_to_string(at).map_err(|source| M3uError::Read {
        path: at.to_path_buf(),
        source,
    })?;
    Ok(parse(at, &text))
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

/// Resolve `.` and `..` textually.
///
/// Not `canonicalize`: that touches the filesystem and fails on a path that
/// does not exist, and a playlist naming a track on an unmounted drive should
/// still round-trip rather than vanish.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn track(path: &str, artist: Option<&str>, title: &str, secs: u64) -> Track {
        Track {
            path: PathBuf::from(path),
            artist: artist.map(str::to_string),
            title: Some(title.into()),
            duration: Duration::from_secs(secs),
            ..Default::default()
        }
    }

    #[test]
    fn a_playlist_is_extm3u_with_relative_paths() {
        let t = track(
            "/m/General/Sierra Ferrell/Long Time Coming/01 The Sea.mp3",
            Some("Sierra Ferrell"),
            "The Sea",
            211,
        );
        let out = render(Path::new("/m/.gmuse/playlists/road.m3u8"), &[&t]);
        assert_eq!(
            out,
            "#EXTM3U\n\
             #EXTINF:211,Sierra Ferrell - The Sea\n\
             ../../General/Sierra Ferrell/Long Time Coming/01 The Sea.mp3\n"
        );
    }

    /// Relative so that a library and its playlists move together — which is
    /// the whole reason for putting them under `data_dir`.
    #[test]
    fn a_written_playlist_reads_back_to_the_same_paths() {
        let tracks = [
            track("/m/General/a.mp3", Some("A"), "One", 100),
            track("/m/Dance/b.mp3", Some("B"), "Two", 200),
        ];
        let at = Path::new("/m/.gmuse/playlists/mix.m3u8");
        let text = render(at, &tracks.iter().collect::<Vec<_>>());
        assert_eq!(
            parse(at, &text),
            vec![
                PathBuf::from("/m/General/a.mp3"),
                PathBuf::from("/m/Dance/b.mp3")
            ]
        );

        // And the same list read from a moved library resolves under the new
        // root, because nothing absolute was written.
        assert_eq!(
            parse(Path::new("/elsewhere/.gmuse/playlists/mix.m3u8"), &text),
            vec![
                PathBuf::from("/elsewhere/General/a.mp3"),
                PathBuf::from("/elsewhere/Dance/b.mp3")
            ]
        );
    }

    /// `#EXTINF` is written for other programs and ignored on read: our own
    /// tags are better, and a metadata cache in a text file goes stale.
    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let at = Path::new("/m/p.m3u8");
        let text = "#EXTM3U\n\n#EXTINF:211,Someone - Something\n/abs/a.mp3\n\n# a note\nb.mp3\n";
        assert_eq!(
            parse(at, text),
            vec![PathBuf::from("/abs/a.mp3"), PathBuf::from("/m/b.mp3")]
        );
    }

    /// cmus's `.pl` is path-per-line with no `#` lines — a strict subset, so
    /// the same parser reads it for free.
    #[test]
    fn a_bare_path_per_line_file_reads() {
        let at = Path::new("/m/p.pl");
        assert_eq!(
            parse(at, "/abs/a.mp3\n/abs/b.mp3\n"),
            vec![PathBuf::from("/abs/a.mp3"), PathBuf::from("/abs/b.mp3")]
        );
    }

    #[test]
    fn dot_segments_resolve_without_touching_the_filesystem() {
        // `canonicalize` would fail here; a track on an unmounted drive must
        // still round-trip rather than vanish.
        assert_eq!(
            parse(Path::new("/m/.gmuse/p.m3u8"), "../General/../Dance/x.mp3\n"),
            vec![PathBuf::from("/m/Dance/x.mp3")]
        );
    }

    /// A path with nothing in common stays absolute rather than becoming a
    /// chain of `..` that climbs out of the filesystem.
    #[test]
    fn an_unrelated_path_stays_absolute() {
        let t = track("/other/volume/x.mp3", Some("X"), "X", 10);
        let out = render(Path::new("/m/.gmuse/p.m3u8"), &[&t]);
        assert!(out.contains("\n/other/volume/x.mp3\n"), "{out}");
    }

    #[test]
    fn a_track_with_no_artist_still_gets_a_label() {
        let t = track("/m/x.mp3", None, "Untitled", 5);
        let out = render(Path::new("/m/p.m3u8"), &[&t]);
        assert!(out.contains("#EXTINF:5,Unknown Artist - Untitled"), "{out}");
    }

    #[test]
    fn writing_is_atomic_and_round_trips_on_disk() {
        let d = std::env::temp_dir().join("gmuse-m3u-write");
        let _ = std::fs::remove_dir_all(&d);
        let at = d.join("playlists/mix.m3u8");
        let t = track("/m/a.mp3", Some("A"), "One", 100);

        write(&at, &[&t]).unwrap();
        assert!(at.exists(), "the directory was not created");
        assert!(!at.with_extension("tmp").exists(), "a temp file was left");
        assert_eq!(read(&at).unwrap(), vec![PathBuf::from("/m/a.mp3")]);
    }

    #[test]
    fn reading_something_missing_is_an_error_not_an_empty_playlist() {
        assert!(read(Path::new("/nope/nothing.m3u8")).is_err());
    }
}
