"""The one place a tool opens the library — #298.

**Because the extension is a build artefact and nothing checked it.**
`gmuse_core` is compiled from the crate next door and installed into this
environment by `just wheel-dev`. Nothing re-runs that when the crate changes,
so the `.so` a tool imports can be any age at all, and it answers confidently
from whatever fold that build knew how to make.

Measured, 2026-09-30: the installed extension was **18 days and one minor
version** behind the source tree (0.1.0 against 0.2.1). `quality propose` read
7,292 encodings through it where `gmuse shelf query --every-copy` read 7,887
from the same log and the same snapshot — 595 copies it could not see at all,
including every file imported since. It skipped them as "nothing to measure"
and reported success.

That is `CLAUDE.md`'s fourth way to be green and wrong: the pass had no
instrument attached and no sentence in its output could have said so. After a
rebuild the two views are row-for-row identical, which is what makes the cause
the build rather than the lookup.

**The check compares what is loaded against what the source says**, because
those are the two things that can disagree. A wheel's `dist-info` records what
was installed; `gmuse_core.__version__` is compiled in, so it describes the
bytes actually running.
"""

from __future__ import annotations

import re
import tomllib
from pathlib import Path
from typing import Final

import gmuse_core

#: Where the workspace version lives, relative to this file: `tools/gmuse_tools/`
#: up to the repository root.
CARGO: Final = Path(__file__).resolve().parents[2] / "Cargo.toml"

REBUILD: Final = "run `just wheel-dev`"


class Stale(RuntimeError):
    """The loaded extension is not the one the source tree describes."""


def workspace_version() -> str | None:
    """What the crate beside this checkout says it is, or `None` outside one.

    **`None` is not a silent hole.** Outside a checkout there is no source to
    be out of step with: the wheel and these tools were installed together from
    one build, and the drift this guards against is a development-time one —
    editing the crate and not resyncing. Raising there would be a false alarm
    about a state that cannot arise.
    """
    if not CARGO.is_file():
        return None
    try:
        parsed = tomllib.loads(CARGO.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError):
        return None
    # `[workspace.package] version` is where `cargo set-version --workspace`
    # writes, and `[package] version` is the fallback for a plain crate.
    workspace = parsed.get("workspace", {}).get("package", {}).get("version")
    return workspace or parsed.get("package", {}).get("version")


def loaded_version() -> str:
    """What the extension that is actually imported was compiled from.

    A build old enough to predate `__version__` reports `0.0.0-unknown`, which
    is not a version the crate ever had — so it fails the comparison below and
    names itself rather than passing for want of an attribute.
    """
    return getattr(gmuse_core, "__version__", "0.0.0-unknown")


def check(*, strict: bool = True) -> str | None:
    """Compare the two, and say what is wrong in a sentence a person can act on.

    Returns the complaint, or `None` when there is nothing to complain about.
    Raises [`Stale`] instead when `strict`, which is what an entry point wants:
    a pass that writes facts about files must not run against a fold that
    cannot see them.
    """
    want = workspace_version()
    if want is None:
        return None
    have = loaded_version()
    if have == want:
        return None
    return (
        f"gmuse_core is {have} and the crate beside it is {want}. "
        f"A tool reading the library through an old build sees an old fold and "
        f"says nothing — {REBUILD}."
    )


def open_library(*, strict: bool = True) -> gmuse_core.Library:
    """`gmuse_core.Library.open()`, having first checked what is opening it.

    Every tool goes through here rather than calling `Library.open()` itself,
    so the guard cannot be added to some callers and forgotten by others — the
    shape #213 dissolved a pair of lists into.
    """
    complaint = check(strict=strict)
    if complaint is not None:
        if strict:
            raise Stale(complaint)
        print(f"  warning: {complaint}")
    return gmuse_core.Library.open()


#: A version string as `cargo` writes one. Used only to tell a real version
#: from the placeholder above, so it is deliberately loose about pre-release
#: suffixes.
SEMVER: Final = re.compile(r"^\d+\.\d+\.\d+")


def is_a_version(text: str) -> bool:
    """Whether `text` looks like a version rather than the unknown placeholder."""
    return bool(SEMVER.match(text)) and "unknown" not in text
