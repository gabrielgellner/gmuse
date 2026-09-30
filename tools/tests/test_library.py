"""The guard on what a tool is reading the library through — #298.

`quality propose` ran against a `gmuse_core` 18 days and one minor version
behind the crate, saw 595 fewer copies than `gmuse shelf query` saw from the
same log, skipped them as "nothing to measure" and reported success. Nothing in
its output could have said so, which is `CLAUDE.md`'s fourth way to be green
and wrong.
"""

from __future__ import annotations

import tomllib
from pathlib import Path

import pytest

from gmuse_tools import library
from gmuse_tools.quality import propose


def test_the_crate_version_is_found_and_is_a_version() -> None:
    """The instrument: there is a `Cargo.toml` and it parses.

    Computed off the file rather than off `workspace_version`, so this can fail
    while the comparison tests pass and the other way round. Without it every
    assertion below passes vacuously on a checkout where the path is wrong —
    `check` returns `None` when it cannot find the crate, which is exactly the
    shape §13 warns about: an absence that reads the same whether nothing is
    wrong or nothing was looked at.
    """
    assert library.CARGO.is_file(), f"no crate at {library.CARGO}"
    parsed = tomllib.loads(library.CARGO.read_text(encoding="utf-8"))
    declared = parsed.get("workspace", {}).get("package", {}).get("version") or parsed[
        "package"
    ]["version"]
    assert library.is_a_version(declared), f"{declared!r} is not a version"
    assert library.workspace_version() == declared


def test_the_loaded_extension_reports_a_version() -> None:
    """And it is a real one, not the placeholder a pre-#298 build would give."""
    have = library.loaded_version()
    assert library.is_a_version(have), (
        f"gmuse_core reports {have!r} — a build predating #298, "
        f"which is itself the staleness this guards against"
    )


def test_a_matching_build_is_not_complained_about() -> None:
    """The working case, so the guard cannot pass by always complaining."""
    assert library.check() is None
    assert library.check(strict=True) is None


def test_a_stale_build_is_refused_and_says_how_to_fix_it(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The case that actually happened: 0.1.0 loaded against a 0.2.1 crate."""
    monkeypatch.setattr(library, "loaded_version", lambda: "0.1.0")
    monkeypatch.setattr(library, "workspace_version", lambda: "0.2.1")

    complaint = library.check(strict=False)
    assert complaint is not None, "a version mismatch was not noticed"
    assert "0.1.0" in complaint and "0.2.1" in complaint, "the complaint names neither version"
    assert library.REBUILD in complaint, "the complaint does not say what to do"

    with pytest.raises(library.Stale):
        library.open_library()


def test_outside_a_checkout_there_is_nothing_to_compare(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """`None` from `workspace_version` must not become a complaint.

    Guarded because the alternative — treating "no crate found" as a mismatch —
    would make every installed copy of the tools refuse to start.
    """
    monkeypatch.setattr(library, "workspace_version", lambda: None)
    monkeypatch.setattr(library, "loaded_version", lambda: "0.0.0-unknown")
    assert library.check() is None


def test_propose_counts_a_row_it_cannot_locate_apart_from_one_it_cannot_measure(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    """The three causes are three sentences — #298.

    They were one counter printed as "had nothing to measure", which is true of
    a file that could not be decoded and equally true of a row naming no copy at
    all. Only the first is a fact about the music; the second was how 595
    invisible copies went unremarked.
    """

    class FakeLibrary:
        def query(self, every_copy: bool = False) -> list[dict[str, object]]:
            assert every_copy, "propose must ask for every copy (#183)"
            return [
                {"encoding": "e-nowhere", "locator": None},
                {"encoding": None, "locator": "/gone/orphan.mp3"},
                {"encoding": "e-unreadable", "locator": "/x/silent.mp3"},
                {"encoding": "e-nameless", "locator": "/x/nameless.mp3"},
                {"encoding": "e-good", "locator": "/x/good.mp3"},
            ]

        def encoding_at(self, locator: str) -> str | None:
            return None if locator.endswith("nameless.mp3") else "e-good"

        def propose(self, lines: list[str], path: Path) -> int:
            path.write_text("\n".join(lines))
            return len(lines)

    monkeypatch.setattr("gmuse_tools.quality.open_library", FakeLibrary)
    monkeypatch.setattr(
        "gmuse_tools.quality.measure",
        lambda locator: None if locator.endswith("silent.mp3") else (19.2, 30.0),
    )

    propose(tmp_path / "out.jsonl", analyser=1)
    printed = capsys.readouterr().out

    # The instrument: the fake library really was walked, and the shapes it
    # offered really did land in different buckets. Counted off the fixture
    # rather than off the output, so it can fail while the claims pass.
    rows = FakeLibrary().query(every_copy=True)
    assert len(rows) == 5, "the fixture is not the one these claims are about"
    assert sum(1 for r in rows if not r["encoding"] or not r["locator"]) == 2

    assert "2 row(s) name no copy this device holds" in printed
    assert "1 file(s) the analyser could not measure" in printed
    assert "1 file(s) whose bytes could not be named" in printed
    assert "had nothing to measure" not in printed, "the one-bucket sentence is back"
    assert "1 measurement(s)" in printed
