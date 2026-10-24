"""The renderer's bookkeeping: what it writes, what it deletes, what it refuses — #332.

**The claims here are mostly absences** — nothing rewritten, nothing adopted,
nothing written inside the library — so each test carries a second assertion
computed some other way, proving the run it inspects did something at all.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from gmuse_tools.render import MANIFEST, Refused, render


class FakeLibrary:
    """What `render` asks of `gmuse_core.Library`, over files on disk.

    `held` maps encoding → (source file, rendered path). A query is a set of
    encodings, or everything.
    """

    def __init__(self, roots: list[Path], held: dict[str, tuple[Path, str]]) -> None:
        self.roots = roots
        self.held = held

    def query(self, spec: str | None = None) -> list[dict[str, Any]]:
        wanted = set(spec.split(",")) if spec else set(self.held)
        return [{"encoding": e, "locator": str(src)} for e, (src, _) in self.held.items() if e in wanted]

    def layout(self, encodings: list[str]) -> list[dict[str, Any]]:
        return [{"encoding": e, "path": self.held[e][1]} for e in encodings]


@pytest.fixture
def lib(tmp_path: Path) -> FakeLibrary:
    root = tmp_path / "MusicLibrary"
    held = {}
    for enc, rel in [("e1", "Orbital/Diversions/05 Lush.flac"), ("e2", "Beirut/Lon Gisland/01 Elephant Gun.mp3")]:
        src = root / "masters" / f"{enc}.bin"
        src.parent.mkdir(parents=True, exist_ok=True)
        src.write_bytes(f"audio of {enc}".encode())
        held[enc] = (src, rel)
    return FakeLibrary([root], held)


def files_in(target: Path) -> set[str]:
    """What is on disk under a target, the manifest aside — counted from the
    filesystem, not from anything the renderer reports."""
    return {str(p.relative_to(target)) for p in target.rglob("*") if p.is_file() and p.name != MANIFEST}


def test_a_second_render_of_an_unchanged_library_writes_nothing(lib: FakeLibrary, tmp_path: Path) -> None:
    target = tmp_path / "PlexMusic"
    first = render(lib, target, None, write=True)
    assert len(files_in(target)) == 2, "instrument: the first render put both files on disk"
    assert first.written == 2

    second = render(lib, target, None, write=True)
    assert second.written == 0, "nothing changed, so nothing is copied again"


def test_a_dry_run_writes_nothing(lib: FakeLibrary, tmp_path: Path) -> None:
    target = tmp_path / "PlexMusic"
    done = render(lib, target, None, write=False)
    assert done.written == 2, "instrument: the dry run did plan the writes"
    assert not target.exists(), "and made nothing, not even the directory"


def test_a_recording_leaving_the_query_is_named_by_the_dry_run_then_deleted(
    lib: FakeLibrary, tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    gone = target / lib.held["e2"][1]
    assert gone.is_file(), "instrument: the file to be deleted is there"
    capsys.readouterr()

    render(lib, target, "e1", write=False)
    assert f"delete  {lib.held['e2'][1]}" in capsys.readouterr().out
    assert gone.is_file(), "a dry run names the delete and does not do it"

    render(lib, target, "e1", write=True)
    assert not gone.exists()
    assert not gone.parent.exists(), "the album folder it left empty goes too"
    assert (target / lib.held["e1"][1]).is_file(), "and the recording still wanted stays"


def test_a_target_remembers_its_query(lib: FakeLibrary, tmp_path: Path) -> None:
    target = tmp_path / "laptop"
    render(lib, target, "e1", write=True)
    assert files_in(target) == {lib.held["e1"][1]}, "instrument: the subset was rendered"

    again = render(lib, target, None, write=True)
    assert again.deleted == 0, "no query given means the target's own, not everything"
    assert again.written == 0


def test_a_directory_with_files_and_no_manifest_is_refused(lib: FakeLibrary, tmp_path: Path) -> None:
    fresh = tmp_path / "fresh"
    fresh.mkdir()
    render(lib, fresh, None, write=False)  # instrument: an empty directory is a new rendering

    theirs = tmp_path / "Media" / "Music"
    theirs.mkdir(parents=True)
    (theirs / "something.mp3").write_bytes(b"somebody's music")
    with pytest.raises(Refused, match="not a rendering"):
        render(lib, theirs, None, write=True)
    assert files_in(theirs) == {"something.mp3"}


def test_a_target_inside_the_library_is_refused(lib: FakeLibrary, tmp_path: Path) -> None:
    beside = tmp_path / "PlexMusic"
    render(lib, beside, None, write=False)  # instrument: a sibling of the root is fine

    inside = lib.roots[0] / "Renderings" / "Plex"
    with pytest.raises(Refused, match="never scanned"):
        render(lib, inside, None, write=True)
    with pytest.raises(Refused, match="never scanned"):
        render(lib, tmp_path, None, write=True)  # a target holding the root is no better
    assert not inside.exists()


def test_a_file_the_manifest_does_not_name_is_never_overwritten(lib: FakeLibrary, tmp_path: Path) -> None:
    target = tmp_path / "PlexMusic"
    render(lib, target, "e1", write=True)
    stranger = target / lib.held["e2"][1]
    stranger.parent.mkdir(parents=True)
    stranger.write_bytes(b"put here by hand")

    done = render(lib, target, "e1,e2", write=True)
    assert stranger.read_bytes() == b"put here by hand"
    assert done.written == 0
    manifest = json.loads((target / MANIFEST).read_text())
    assert lib.held["e1"][1] in manifest["files"], "instrument: the manifest was read and kept"
    assert lib.held["e2"][1] not in manifest["files"]


def test_a_path_now_made_from_a_different_copy_is_rewritten(lib: FakeLibrary, tmp_path: Path) -> None:
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    at = target / lib.held["e1"][1]
    before = at.read_bytes()

    src, rel = lib.held.pop("e1")
    better = src.with_name("e3.bin")
    better.write_bytes(b"audio of a better copy")
    lib.held["e3"] = (better, rel)

    done = render(lib, target, None, write=True)
    assert done.written == 1
    assert at.read_bytes() != before
    assert at.read_bytes() == b"audio of a better copy"


def test_a_file_deleted_from_the_target_by_hand_is_written_again(lib: FakeLibrary, tmp_path: Path) -> None:
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    at = target / lib.held["e1"][1]
    at.unlink()
    assert len(files_in(target)) == 1, "instrument: one of the two is really gone"

    done = render(lib, target, None, write=True)
    assert done.written == 1
    assert at.is_file()


def test_a_layout_that_disowns_copies_the_query_located_is_refused(lib: FakeLibrary, tmp_path: Path) -> None:
    """The first `layout` forgot which device it was on and called all 83 of
    one artist's local copies held elsewhere. A `--write` on that answer
    would delete a full rendering, so it is refused instead."""
    target = tmp_path / "PlexMusic"
    render(lib, target, None, write=True)
    assert len(files_in(target)) == 2, "instrument: a full rendering exists to be lost"

    def disowning(encodings: list[str]) -> list[dict[str, Any]]:
        return [{"encoding": e, "elsewhere": True} for e in encodings]

    lib.layout = disowning  # type: ignore[method-assign]
    with pytest.raises(Refused, match="layout says they are not"):
        render(lib, target, None, write=True)
    assert len(files_in(target)) == 2, "and nothing was deleted on its word"
