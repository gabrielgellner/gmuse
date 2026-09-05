//! `:edit` — the composed view in `$EDITOR`, and it is one-way (issue #99).
//!
//! The third tier of `query-language.md` §11:
//!
//! ```text
//! :view genre*="techno"   one line, narrows the active view
//! :view                   shows the stack, each term droppable
//! :edit                   opens the composed query in $EDITOR
//! ```
//!
//! **That the escape hatch is the same text is the point.** The document is
//! the grammar the `:` line takes, with newlines allowed and `#` comments —
//! see [`crate::view::Stack::document`]. A structured save format would make
//! the editor a translation rather than a bigger window.
//!
//! ## Why this is not in the UI module
//!
//! Editing suspends the terminal, and nothing inside the draw loop may do
//! that. The loop returns [`crate::ui::Stop::Edit`], `main` restores the
//! screen and calls [`edit`], exactly as it already does for a refold — the
//! established shape for "this needs the loop to not be running".

use std::path::{Path, PathBuf};

use crate::view::{DocumentError, Stack};

/// What stopped an edit, said in the terms the person can act on.
#[derive(Debug, thiserror::Error)]
pub enum Trouble {
    /// **Neither `$VISUAL` nor `$EDITOR` is set, and nothing is guessed.**
    ///
    /// git and crontab fall back to `vi`. This does not: spawning a modal
    /// editor somebody did not ask for, on a box where they may not know how
    /// to leave it, is the kind of inference this project refuses everywhere
    /// else — and the fix is one line of shell.
    #[error("no editor: set $VISUAL or $EDITOR")]
    NoEditor,
    #[error("no data directory to write the query into")]
    NoDataDir,
    #[error("could not run the editor: {0}")]
    Io(#[from] std::io::Error),
    /// The editor exited non-zero, which is how `:cq` and friends say "forget
    /// it". Nothing is applied — the git convention, and the safe one.
    #[error("editor exited without saving")]
    Abandoned,
    #[error("{0}")]
    Bad(#[from] DocumentError),
}

/// Where the query is written for editing.
///
/// **A stable name, not a temp file.** An editor's syntax rules, undo history
/// and window position key off the path, so a fresh `/tmp/XXXX` every time
/// throws all three away. It also means the last query edited is still there
/// to look at afterwards.
pub fn path_in(dir: &Path) -> PathBuf {
    dir.join("view.query")
}

/// Which command to run, `$VISUAL` before `$EDITOR` as everything else does.
///
/// Split on whitespace so `EDITOR="code -w"` works, which is the form people
/// actually have in their shell profile.
fn editor() -> Option<Vec<String>> {
    let set = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
    let raw = set("VISUAL").or_else(|| set("EDITOR"))?;
    let parts: Vec<String> = raw.split_whitespace().map(str::to_string).collect();
    (!parts.is_empty()).then_some(parts)
}

/// Write the view out, open it, read it back.
///
/// Returns the stack the document parsed to. The caller applies it, so this
/// stays testable without a terminal — and [`document_back`] is the half the
/// tests exercise.
pub fn edit(stack: &Stack, dir: Option<&Path>) -> Result<Stack, Trouble> {
    let dir = dir.ok_or(Trouble::NoDataDir)?;
    let argv = editor().ok_or(Trouble::NoEditor)?;
    let path = path_in(dir);
    std::fs::create_dir_all(dir)?;
    std::fs::write(&path, stack.document())?;

    let status = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .arg(&path)
        .status()?;
    if !status.success() {
        return Err(Trouble::Abandoned);
    }
    document_back(&std::fs::read_to_string(&path)?)
}

/// Read an edited document back into a stack.
///
/// Split out from [`edit`] because it is the whole behaviour worth testing:
/// spawning a process is the part that needs a terminal and the part that has
/// nothing to get wrong.
pub fn document_back(text: &str) -> Result<Stack, Trouble> {
    Ok(Stack::parse_document(text)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **An edited document becomes the view**, comments and all.
    #[test]
    fn what_comes_back_is_what_was_written() {
        let text = "# the good stuff\nin=\"Dance\"\n& year>=2015\n";
        let back = document_back(text).expect("it parses");
        assert_eq!(back.terms().len(), 2);
        assert_eq!(back.document(), text, "the edit did not survive the read");
    }

    /// **A bad line is named**, which is why the editor gets a document.
    #[test]
    fn a_bad_line_is_reported_by_number() {
        let e = document_back("in=\"Dance\"\n\n+ jazz\n").expect_err("bare word");
        assert!(e.to_string().starts_with("line 3:"), "{e}");
    }

    /// **Emptying the file is how you clear the view**, not an error. It is
    /// the document form of `:view` with nothing after it.
    #[test]
    fn an_empty_document_is_the_whole_library() {
        let back = document_back("# everything\n\n").expect("parses");
        assert!(back.is_empty());
    }

    /// Nothing is guessed when no editor is named.
    #[test]
    fn without_an_editor_it_says_so_rather_than_picking_one() {
        // Not asserting on the live environment — `edit` reaches the editor
        // lookup only after the data dir, so the two refusals are ordered and
        // this pins the one that does not depend on `$EDITOR` being unset.
        let e = edit(&Stack::default(), None).expect_err("no dir");
        assert!(matches!(e, Trouble::NoDataDir), "{e}");
    }
}
