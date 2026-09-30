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
