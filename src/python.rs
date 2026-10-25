//! **L3 for Python** — #197.
//!
//! `access-layers.md` §6. The tools already reach the library through a
//! boundary: a work list from `shelf query`, a proposal file for `shelf
//! apply`. What that boundary lacks is a **contract anyone can fail against**
//! — a tool writes JSONL by hand and finds out whether the fields were the
//! ones `apply` reads three hours later, on a file somebody spent an afternoon
//! producing. Two spellings of one album id cost eight albums their genre.
//!
//! So this is an adapter and holds no rules:
//!
//! - `query` is `filter.rs`, so a spec the `:` line accepts, Python accepts —
//!   by construction rather than by two parsers agreeing today.
//! - `propose` deserialises into the same `Proposal` the apply pass reads, so a
//!   malformed one fails **where it is built**, with the field named.
//!
//! What stays in Python is everything it is better at and that is not library
//! state: HTTP and rate limits, MusicBrainz and Discogs parsing, `bronze`, tag
//! writing, the review files, and the judgements that produce them.

use pyo3::exceptions::{PyIOError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyList;

use crate::model::entity::Device;
use crate::model::id::Id;

/// A read handle on the library. **Opens no music file.**
///
/// It folds the log — or loads the snapshot and stats the logs to find it is
/// current, which costs 17.5µs and 9.5ms. A file that arrived reaches a row
/// when `shelf import` takes it in, and a file that moved when `shelf scan`
/// says so; neither is this object's business (#195).
#[pyclass]
pub struct Library {
    projection: crate::projection::Projected,
    /// What reading a file already told us, so `encoding_at` is a `stat` for
    /// everything that has not changed. Loaded once; not written back, because
    /// a tool asking which bytes it just read should not be rewriting a cache
    /// the library's own passes maintain.
    //
    // A `Mutex` rather than a `RefCell`: pyo3 requires a `#[pyclass]` to be
    // `Sync`, because Python may hand the same object to another thread.
    reads: std::sync::Mutex<crate::model::ingest::Reads>,
    device: Id<Device>,
    roots: Vec<std::path::PathBuf>,
    data_dir: std::path::PathBuf,
}

#[pymethods]
impl Library {
    /// Open the library the config points at.
    #[staticmethod]
    #[pyo3(signature = (data_dir = None))]
    fn open(data_dir: Option<std::path::PathBuf>) -> PyResult<Self> {
        let loaded = crate::config::load(&crate::config::default_path());
        let data_dir = data_dir.unwrap_or(loaded.data_dir.clone());
        let machine = crate::events::machine_id(&crate::config::default_state_dir());
        let device = Id::derived(machine.as_bytes());

        // The same read path the CLI takes, and for the same reason: the
        // print decides, so a tool that is caught up never opens the log.
        let projection = crate::librarian::reading(&data_dir, device, crate::history::now_ms());
        Ok(Library {
            projection,
            reads: std::sync::Mutex::new(crate::cache::load_measurements(
                &crate::cache::measurements_path(),
            )),
            device,
            roots: loaded.library.clone(),
            data_dir,
        })
    }

    /// **The rows a filter leaves**, as dicts — the same shape `shelf query`
    /// prints, because it is the same function.
    ///
    /// A spec this refuses is a spec the `:` line refuses, and the error names
    /// what it choked on rather than returning nothing.
    #[pyo3(signature = (filter = None, unidentified = false, every_copy = false))]
    fn query(
        &self,
        py: Python<'_>,
        filter: Option<&str>,
        unidentified: bool,
        every_copy: bool,
    ) -> PyResult<Py<PyList>> {
        let parsed = filter
            .map(crate::query::filter::Expr::parse)
            .transpose()
            .map_err(|e| PyValueError::new_err(format!("filter: {e}")))?;

        // **The librarian, built once here** — `rows` takes it rather than building one per
        // call (#261). A Python pass makes one query per process, so this is the same work in
        // a place that says so.
        let library = crate::model::seam::Librarian::from_catalogue(self.projection.catalogue());
        let rows = crate::query::rows(
            self.projection.catalogue(),
            &library,
            self.device,
            crate::history::now_ms(),
            if unidentified {
                crate::query::Only::Unidentified
            } else {
                crate::query::Only::Everything
            },
            if every_copy {
                crate::query::Copies::Every
            } else {
                crate::query::Copies::Shown
            },
            parsed.as_ref(),
        );

        // Through serde, so the dict a tool sees and the JSON the CLI prints
        // cannot drift: one `Serialize`, two renderings.
        let out = PyList::empty(py);
        for row in rows {
            let json = serde_json::to_string(&row)
                .map_err(|e| PyValueError::new_err(format!("row: {e}")))?;
            out.append(py.import("json")?.call_method1("loads", (json,))?)?;
        }
        Ok(out.into())
    }

    /// **Write proposals, having checked every one of them first** — the half
    /// a file handoff cannot do.
    ///
    /// Each is deserialised into the `Proposal` the apply pass reads. A
    /// malformed one raises **here**, naming the row and what was wrong with
    /// it, rather than being discovered by `shelf apply` after a long pass.
    fn propose(&self, items: Vec<String>, path: std::path::PathBuf) -> PyResult<usize> {
        let mut lines = String::new();
        for (n, raw) in items.iter().enumerate() {
            serde_json::from_str::<crate::model::proposal::Proposal>(raw)
                .map_err(|e| PyValueError::new_err(format!("proposal {}: {e}", n + 1)))?;
            lines.push_str(raw.trim());
            lines.push('\n');
        }
        std::fs::write(&path, lines).map_err(|e| PyIOError::new_err(e.to_string()))?;
        Ok(items.len())
    }

    /// **The copy each group's rendering is made from** — #333.
    ///
    /// Each group is the encoding ids that compete for one place — for a
    /// rendering, the copies of one recording that would land in one folder.
    /// The best of them *this device holds*, by `canonical_key`, the order a
    /// view uses (#325), so a rendering and the player rank copies alike.
    ///
    /// A dict per group, in the order given: `encoding`, its `locator`, the
    /// `tied` copies only an id separated from it and `tied_at`, where those
    /// are — or an empty dict where this device holds none of them.
    fn choose(&self, py: Python<'_>, groups: Vec<Vec<String>>) -> PyResult<Py<PyList>> {
        let mut library =
            crate::model::seam::Librarian::from_catalogue(self.projection.catalogue());
        library.here(self.device);
        let out = PyList::empty(py);
        for group in &groups {
            let candidates = group
                .iter()
                .map(|raw| {
                    raw.parse::<Id<crate::model::entity::Encoding>>()
                        .map_err(|err| PyValueError::new_err(format!("encoding {raw}: {err}")))
                })
                .collect::<PyResult<Vec<_>>>()?;
            let row = pyo3::types::PyDict::new(py);
            if let Some(held) = library.encodings.best_held(&candidates, self.device) {
                row.set_item("encoding", held.best.id.to_string())?;
                row.set_item("locator", library.where_held(held.best.id))?;
                // **Whether a rendering may transcode it** (#334): lossless
                // only, and the format's to say, not the extension's.
                row.set_item("lossless", held.best.format.is_lossless())?;
                // What a transcode is checked against: an encode that came
                // back short is a broken file, not a smaller one.
                row.set_item("length_ms", held.best.length.as_millis() as u64)?;
                let tied: Vec<String> = held.tied.iter().map(ToString::to_string).collect();
                row.set_item("tied", tied)?;
                // Where each one is, because a tie is reported to a person,
                // and an encoding id is not something a person can open.
                let tied_at: Vec<Option<String>> =
                    held.tied.iter().map(|e| library.where_held(*e)).collect();
                row.set_item("tied_at", tied_at)?;
            }
            out.append(row)?;
        }
        Ok(out.into())
    }

    /// **What the library says about each of these copies** — #335.
    ///
    /// The values a rendering writes into its tags: the library's statement,
    /// read from the projection and never from the file. A dict per encoding
    /// id, in the order given; a field the library has no value for is
    /// **absent**, because a rendering clears what the library does not say
    /// rather than leaving a stale tag to contradict it. `{}` for a copy with
    /// no row.
    ///
    /// Of the copy's *appearance*: the album, track and disc are the slot the
    /// copy is placed in, so one recording on an album and on a compilation
    /// states two different things.
    fn statement(&self, py: Python<'_>, encodings: Vec<String>) -> PyResult<Py<PyList>> {
        let catalogue = self.projection.catalogue();
        let mut library = crate::model::seam::Librarian::from_catalogue(catalogue);
        library.here(self.device);
        let genre = Id::<crate::model::entity::Facet>::of_key(crate::model::facet::GENRE);
        let out = PyList::empty(py);
        for raw in &encodings {
            let copy = raw
                .parse::<Id<crate::model::entity::Encoding>>()
                .map_err(|err| PyValueError::new_err(format!("encoding {raw}: {err}")))?;
            let row = pyo3::types::PyDict::new(py);
            let listed = library.row_of_copy(copy).and_then(|r| library.describe(r));
            if let Some(listed) = listed {
                let some = |k: &str, v: Option<&str>| -> PyResult<()> {
                    match v.filter(|s| !s.is_empty()) {
                        Some(s) => row.set_item(k, s),
                        None => Ok(()),
                    }
                };
                // `_if_named`, not the stand-ins a tree draws for a nameless row:
                // a tag reading "No Album" would be the library inventing one.
                some("title", listed.title_if_named)?;
                some("artist", listed.artist)?;
                some("album", listed.album_if_named)?;
                some("album_artist", listed.album_artist)?;
                some("label", listed.label)?;
                if let Some(track) = listed.track {
                    row.set_item("track", track)?;
                }
                if let Some(disc) = listed.disc {
                    row.set_item("disc", disc)?;
                }
                let release = library.release_of(library.placed_at(copy));
                if let Some(discs) = release.and_then(|r| library.release_media(r)) {
                    row.set_item("discs", discs)?;
                }
                if let Some(year) = listed.year {
                    row.set_item("year", year)?;
                }
                if let Some(genres) = listed.concepts.get(&genre).filter(|g| !g.is_empty()) {
                    row.set_item("genres", genres.clone())?;
                }
                if listed
                    .release_secondary
                    .contains(&crate::model::spine::ReleaseSecondary::Compilation)
                {
                    row.set_item("compilation", true)?;
                }
                let recording = library
                    .encoding(copy)
                    .and_then(|e| e.recording)
                    .map(|r| r.id());
                // **The rating the library holds now** — #338's echo rule: a rating
                // read back from Plex is recorded only where it differs from this.
                // Not a tag: the writer knows its fields, and this is not one.
                if let Some(stars) = recording.and_then(|r| catalogue.ratings.get(&r)) {
                    row.set_item("stars", *stars)?;
                }
                if let Some(crate::model::basis::Basis::MusicBrainz(id)) =
                    recording.and_then(|r| catalogue.identified.get(&r))
                {
                    row.set_item("musicbrainz_recording", id.to_string())?;
                }
                if let Some(mbid) = release
                    .and_then(|r| catalogue.releases.get(&r))
                    .and_then(|p| p.musicbrainz)
                {
                    row.set_item("musicbrainz_release", mbid.to_string())?;
                }
                // **The source's loudness**, which a transcode keeps: ReplayGain
                // is about how loud the music is, and an MP3 of a FLAC is the
                // same music within a fraction of a decibel.
                if let Some(levels) = library.encoding(copy).map(|e| e.levels) {
                    let gain = |k: &str, v: Option<f32>| -> PyResult<()> {
                        match v {
                            Some(x) => row.set_item(k, x),
                            None => Ok(()),
                        }
                    };
                    gain("track_gain_db", levels.track_db())?;
                    gain("track_peak", levels.track_peak())?;
                    gain("album_gain_db", levels.album_db())?;
                    gain("album_peak", levels.album_peak())?;
                }
            }
            out.append(row)?;
        }
        Ok(out.into())
    }

    /// **Where each of these copies belongs in a rendered tree** — #332.
    ///
    /// `storage-layout.md` §2's path, relative, for each encoding id: a dict
    /// with `path`, or with `refused` saying why there is none, or with
    /// `elsewhere` for a copy this device does not hold. A path two of the
    /// copies want is refused for both, exactly as a re-render refuses it,
    /// because it is the same function.
    ///
    /// `file_under` is an assembled credit line, not a tag (#200), which is why
    /// a tool cannot derive this from the files and asks.
    fn layout(&self, py: Python<'_>, encodings: Vec<String>) -> PyResult<Py<PyList>> {
        let copies = encodings
            .iter()
            .map(|e| {
                e.parse::<Id<crate::model::entity::Encoding>>()
                    .map_err(|err| PyValueError::new_err(format!("encoding {e}: {err}")))
            })
            .collect::<PyResult<Vec<_>>>()?;
        let mut library =
            crate::model::seam::Librarian::from_catalogue(self.projection.catalogue());
        // **This device's locators**, without which every copy reads as held
        // elsewhere: a librarian built from the catalogue knows every holding
        // and has not been told which machine it is on.
        library.here(self.device);
        let layout = crate::operations::storage::layout_of(&library, &copies);

        let placed: std::collections::BTreeMap<_, _> = layout.placed.into_iter().collect();
        let refused: std::collections::BTreeMap<_, _> = layout.refused.into_iter().collect();
        let out = PyList::empty(py);
        for (copy, raw) in copies.iter().zip(&encodings) {
            let row = pyo3::types::PyDict::new(py);
            row.set_item("encoding", raw)?;
            if let Some(at) = placed.get(copy) {
                row.set_item("path", at.to_string_lossy().into_owned())?;
            } else if let Some(why) = refused.get(copy) {
                row.set_item("refused", why.to_string())?;
                // **Contested says where**, because a rendering resolves some
                // contests itself: two copies of one recording wanting one path
                // are one appearance, and it keeps the better. Only the caller
                // knows which copies are of one recording.
                if let crate::operations::storage::Refused::Contested(at) = why {
                    row.set_item("path", at.to_string_lossy().into_owned())?;
                    row.set_item("contested", true)?;
                }
            } else {
                row.set_item("elsewhere", true)?;
            }
            out.append(row)?;
        }
        Ok(out.into())
    }

    /// **Which encoding are the bytes at this path?** — #199.
    ///
    /// A tool that opens a file and then proposes a fact about it has two ways
    /// to name what it measured: the encoding the work list said lives there,
    /// or the encoding the bytes it actually read *are*. Those are the same
    /// thing right up until the file changed, and then the first one is a
    /// **false fact in an append-only log** — permanent, repairable only by
    /// another fact, and nothing would surface it.
    ///
    /// So: ask. The hash is over Symphonia's packet boundaries and a tool
    /// cannot compute it — `quality.py` says so in as many words — which is
    /// why this belongs here rather than being reimplemented anywhere.
    ///
    /// **`None` for a file that cannot be read.** Not an error: a pass over
    /// seven thousand files meets some it cannot open, and that is a row to
    /// skip rather than a run to end.
    ///
    /// Guarded by a `stat`, so an unchanged file costs no re-read.
    fn encoding_at(&self, path: std::path::PathBuf) -> Option<String> {
        let audio = self.reads.lock().ok()?.audio(&path).ok()?;
        Some(crate::model::id::Id::<crate::model::entity::Encoding>::of_audio(&audio).to_string())
    }

    /// **The id a MusicBrainz release mbid derives to** — #203.
    ///
    /// Ids here are convergent: `Id::<Release>::of_musicbrainz` is a UUIDv5
    /// over the mbid, so two machines that never spoke mint the same id for
    /// the same release. It is **one-way**, which is why the library cannot
    /// hand an mbid back and why a pass that wants to look a release up still
    /// reads one from a tag.
    ///
    /// What this is for is checking that tag before believing it: derive the
    /// id it claims, compare it to the `release` on the row, and a
    /// disagreement is a row to skip rather than a `tracklisted` fact written
    /// against somebody else's pressing.
    ///
    /// The same shape as `encoding_at`: the outside world offers a candidate
    /// and the library confirms the identity.
    fn release_from_musicbrainz(&self, mbid: &str) -> PyResult<String> {
        let parsed = uuid::Uuid::parse_str(mbid)
            .map_err(|e| PyValueError::new_err(format!("not a MusicBrainz id: {e}")))?;
        Ok(
            crate::model::id::Id::<crate::model::entity::Release>::of_musicbrainz(parsed)
                .to_string(),
        )
    }

    /// **Check proposals without writing them** — for a pass that writes as it
    /// goes.
    ///
    /// `propose` takes the whole list and writes the file, which is right for
    /// a pass that finishes. `ingest tracklist` does not: MusicBrainz answers
    /// a heavy `inc` in anything from 0.4 to 25 seconds, so it is an hour on a
    /// library this size and the temptation to interrupt it is constant. It
    /// writes each line as it is settled, so an interrupted run keeps what it
    /// learned — and holding the lines back to validate them together would
    /// take that away.
    ///
    /// So: the same check, without the writing. Raises on the first one that
    /// is not a `Proposal`, naming it.
    fn check(&self, items: Vec<String>) -> PyResult<usize> {
        for (n, raw) in items.iter().enumerate() {
            serde_json::from_str::<crate::model::proposal::Proposal>(raw)
                .map_err(|e| PyValueError::new_err(format!("proposal {}: {e}", n + 1)))?;
        }
        Ok(items.len())
    }

    /// Where this library keeps its log and what it calls its roots, so a tool
    /// stops guessing at either.
    #[getter]
    fn data_dir(&self) -> std::path::PathBuf {
        self.data_dir.clone()
    }

    #[getter]
    fn roots(&self) -> Vec<std::path::PathBuf> {
        self.roots.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "<gmuse.Library {} device(s) folded, {}>",
            self.projection.position().devices(),
            self.data_dir.display()
        )
    }
}

#[pymodule]
fn gmuse_core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Library>()?;
    m.add(
        "__doc__",
        "gmuse's library, for the tools that prepare files for it.",
    )?;
    // **Which library a tool is actually running against** — #298.
    //
    // The extension is a build artefact and nothing made it say so, which is
    // how `quality propose` came to be run against an 18-day-old fold: it saw
    // 7,292 encodings where the CLI saw 7,887, skipped the 595 it could not
    // locate, and reported success. Both processes read the same log and the
    // same snapshot — the disagreement was entirely that one of them was old
    // code.
    //
    // `CARGO_PKG_VERSION` at compile time, so it is a property of the bytes
    // that are loaded rather than of anything on disk beside them: a wheel's
    // `dist-info` describes what was installed, and this describes what is
    // running.
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
