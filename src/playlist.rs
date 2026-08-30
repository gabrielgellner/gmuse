//! Views 3 and 4: the playlist and the play queue — DESIGN §4.
//!
//! Both hold `TrackId`s, so the same track can sit in the library, a playlist
//! and the queue at once without being copied.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::store::TrackId;

/// View 3. Ordered, duplicates allowed — a playlist that silently refused a
/// repeat would be wrong.
#[derive(Debug, Default, Clone)]
pub struct Playlist {
    entries: Vec<TrackId>,
}

// Mutation arrives with the command layer in M6 (`:add`, `y`, `e`, `D`); the
// types and their semantics are defined here because §4 marks them INVARIANT
// and the views in M4 already render them.
#[allow(dead_code)]
impl Playlist {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn entries(&self) -> &[TrackId] {
        &self.entries
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn push(&mut self, id: TrackId) {
        self.entries.push(id);
    }
    /// Remove the entry at `index`, if there is one.
    ///
    /// Mirrors `VecDeque::remove`, which `Queue` gets for free. `Vec::remove`
    /// panics out of range, so the bounds check is ours to make.
    pub fn remove(&mut self, index: usize) -> Option<TrackId> {
        if index < self.entries.len() {
            Some(self.entries.remove(index))
        } else {
            None
        }
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// View 4. Consumed as it plays: DESIGN §5 makes the queue win unconditionally
/// over the active source, and popping is what makes that a one-shot.
#[derive(Debug, Default, Clone)]
pub struct Queue {
    entries: VecDeque<TrackId>,
}

#[allow(dead_code)]
impl Queue {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn entries(&self) -> impl Iterator<Item = &TrackId> {
        self.entries.iter()
    }
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn push_back(&mut self, id: TrackId) {
        self.entries.push_back(id);
    }
    /// The next queued track, without consuming it.
    ///
    /// `next()` is pure, so it reports what the queue would give; popping is
    /// the caller's job once it commits to playing.
    pub fn front(&self) -> Option<TrackId> {
        self.entries.front().copied()
    }

    pub fn pop_front(&mut self) -> Option<TrackId> {
        self.entries.pop_front()
    }
    /// Remove the entry at `index`, if there is one.
    pub fn remove(&mut self, index: usize) -> Option<TrackId> {
        self.entries.remove(index)
    }
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

/// Named playlists on disk — issue #22.
///
/// A directory of `.m3u8` files under `data_dir`, so a playlist is a file
/// anything can read and dropping one in from outside makes it appear. That is
/// most of the argument for using a standard format rather than inventing one.
///
/// Deliberately not an index: the set *is* whatever the directory holds. An
/// index would be a second source of truth to keep in step, and the directory
/// is small enough to read every time it is asked for.
#[derive(Debug, Clone)]
pub struct Playlists {
    dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub name: String,
    pub path: PathBuf,
}

impl Playlists {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join("playlists"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where a name is written. Always `.m3u8` — the `8` says UTF-8.
    pub fn path_for(&self, name: &str) -> PathBuf {
        self.dir
            .join(format!("{}.{}", file_stem(name), crate::m3u::EXTENSION))
    }

    /// Every playlist in the directory, by name, sorted.
    ///
    /// Reads `.m3u`, `.m3u8` and cmus's `.pl`, so a folder someone already has
    /// works without being converted first.
    pub fn list(&self) -> Vec<Saved> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut out: Vec<Saved> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_ascii_lowercase())
                    .is_some_and(|e| crate::m3u::EXTENSIONS.contains(&e.as_str()))
            })
            .filter_map(|path| {
                let name = path.file_stem()?.to_string_lossy().into_owned();
                Some(Saved { name, path })
            })
            .collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    /// Find a playlist by name, whatever extension it happens to have.
    pub fn find(&self, name: &str) -> Option<Saved> {
        self.list().into_iter().find(|s| s.name == name)
    }
}

/// A name reaches a file name, so keep it to something a file name can hold.
///
/// Spaces stay — "late night" is a reasonable playlist name and a perfectly
/// good file name. Separators and dots do not: a name is one file in one
/// directory, never a path, and never a hidden file.
fn file_stem(name: &str) -> String {
    let cleaned: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '-',
            c if c.is_control() => ' ',
            c => c,
        })
        .collect();
    // Strip leading dots, dashes and spaces together, so `../../etc/passwd`
    // reduces to a readable `etc-passwd` rather than a thicket of leftovers —
    // and so nothing ends up hidden from its own listing.
    let cleaned = cleaned
        .trim()
        .trim_start_matches(['.', '-', ' '])
        .trim()
        .to_string();
    if cleaned.is_empty() {
        "untitled".to_string()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gmuse-playlists-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// The set *is* the directory, so a playlist dropped in from outside — by
    /// another program, or by hand — simply appears. That is most of the point
    /// of using a standard format.
    #[test]
    fn the_directory_is_the_index() {
        let data = scratch("index");
        let ps = Playlists::new(&data);
        assert!(ps.list().is_empty());

        std::fs::create_dir_all(ps.dir()).unwrap();
        std::fs::write(ps.dir().join("roadtrip.m3u8"), "#EXTM3U\n").unwrap();
        std::fs::write(ps.dir().join("late night.m3u"), "").unwrap();
        std::fs::write(ps.dir().join("from cmus.pl"), "").unwrap();
        // Not a playlist, whatever it contains.
        std::fs::write(ps.dir().join("notes.txt"), "/m/a.mp3\n").unwrap();

        let names: Vec<String> = ps.list().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["from cmus", "late night", "roadtrip"]);
    }

    /// Found by name whatever extension it happens to have, so a folder
    /// someone already keeps works without being converted first.
    #[test]
    fn a_playlist_is_found_by_name_not_by_extension() {
        let data = scratch("find");
        let ps = Playlists::new(&data);
        std::fs::create_dir_all(ps.dir()).unwrap();
        std::fs::write(ps.dir().join("mix.pl"), "").unwrap();

        assert_eq!(ps.find("mix").unwrap().path, ps.dir().join("mix.pl"));
        assert!(ps.find("nope").is_none());
        // But we always *write* .m3u8.
        assert_eq!(ps.path_for("mix"), ps.dir().join("mix.m3u8"));
    }

    /// A name reaches a file name. Spaces are fine; separators are not, and a
    /// leading dot would make the playlist hidden from its own listing.
    #[test]
    fn a_name_is_reduced_to_something_a_file_name_can_hold() {
        assert_eq!(file_stem("late night"), "late night");
        assert_eq!(file_stem("  spaced  "), "spaced");
        assert_eq!(file_stem("a/b"), "a-b");
        assert_eq!(file_stem(".hidden"), "hidden");
        assert_eq!(file_stem("../../etc/passwd"), "etc-passwd");
        assert_eq!(file_stem(""), "untitled");
        assert_eq!(file_stem("   "), "untitled");
    }

    /// Names are one file in one directory — never a path out of it.
    #[test]
    fn a_name_cannot_escape_the_playlist_directory() {
        let data = scratch("escape");
        let ps = Playlists::new(&data);
        for hostile in ["../../../etc/passwd", "/etc/passwd", "a/b/c", ".."] {
            let path = ps.path_for(hostile);
            // The property that matters: one file, in that directory, always.
            assert_eq!(path.parent(), Some(ps.dir()), "{hostile:?} escaped");
            assert_eq!(path.components().count(), ps.dir().components().count() + 1);
        }
    }

    #[test]
    fn playlist_allows_duplicates_and_keeps_order() {
        let mut p = Playlist::new();
        p.push(TrackId(3));
        p.push(TrackId(1));
        p.push(TrackId(3));
        assert_eq!(p.entries(), [TrackId(3), TrackId(1), TrackId(3)]);
        assert_eq!(p.remove(1), Some(TrackId(1)));
        assert_eq!(p.entries(), [TrackId(3), TrackId(3)]);
        assert_eq!(p.remove(99), None);
    }

    #[test]
    fn queue_is_consumed_from_the_front() {
        let mut q = Queue::new();
        q.push_back(TrackId(1));
        q.push_back(TrackId(2));
        assert_eq!(q.pop_front(), Some(TrackId(1)));
        assert_eq!(q.len(), 1);
        assert_eq!(q.pop_front(), Some(TrackId(2)));
        assert_eq!(q.pop_front(), None);
        assert!(q.is_empty());
    }
}
