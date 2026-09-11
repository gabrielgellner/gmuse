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

        let (log, _) =
            crate::model::instance::Instance::open(&data_dir, device, crate::history::now_ms());
        let at = crate::cache::projection_path();
        let print = crate::cache::LogPrint::of(&data_dir);
        let projection = match crate::cache::current_projection(&at, &print) {
            Some(ready) => ready,
            None => {
                let folded = crate::projection::Projected::rebuild(log.records());
                crate::cache::save_projection(&at, &print, &folded);
                folded
            }
        };
        Ok(Library {
            projection,
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

        let rows = crate::query::rows(
            &self.projection,
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
    Ok(())
}
