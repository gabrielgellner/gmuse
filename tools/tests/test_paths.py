"""What a path claims, and whether the tags already claim it — issue #143.

The pass is only useful if `differs` means something. A first version compared
spellings and produced 914 rows over 592 files: `119` against `119.00`, `Am`
against `8A`, a filename naming the lead artist where the tag named four. Every
one of those is agreement wearing a different coat, and burying the real
conflicts in them is the way to make the report unread.

So most of these tests are about *not* reporting.
"""

from __future__ import annotations

from pathlib import Path
from typing import TYPE_CHECKING

from gmuse_tools.paths import (
    covers,
    findings,
    half_time,
    key_position,
    loose,
    same_bpm,
    same_key,
    verdict,
)

if TYPE_CHECKING:
    import pytest


def test_a_tempo_is_compared_as_a_number() -> None:
    """`119` and `119.00` are one tempo; a string comparison said otherwise."""
    assert same_bpm("119", "119.00") is True
    assert same_bpm("126", "125.98") is True
    assert same_bpm("128", "132") is False
    assert same_bpm("128", "") is None, "unreadable is not a conflict"


def test_half_time_is_named_rather_than_just_reported() -> None:
    """A detector reading 117 as 59 is a known failure, worth its own label."""
    assert half_time("117", "59") is True
    assert half_time("64", "128") is True
    assert half_time("128", "132") is False


def test_a_key_reads_from_any_of_its_three_notations() -> None:
    """`Am`, `8A` and `1m` are one key. This library's files carry all three."""
    assert key_position("Am") == key_position("8A") == key_position("1m")
    assert key_position("C") == key_position("8B") == key_position("1d")
    assert key_position("F#m") == key_position("4m")
    assert key_position("C#m") == key_position("Dbm")
    assert same_key("Cmin", "Cm") is True
    assert same_key("Am", "9m") is False, "9m is F minor — a real disagreement"
    assert same_key("Am", "banana") is None


def test_a_subset_is_not_a_conflict() -> None:
    """The dominant false positive, and the reason 276 rows went away.

    A DJ filename names the lead artist where the tag names all four, and drops
    the `(Original Mix)` the tag carries. Neither contradicts the other.
    """
    assert covers("Dizzee Rascal", "Armand Van Helden,Dizzee Rascal") is True
    assert covers("Distant Trip", "Distant Trip (Original Mix)") is True
    assert covers("Sleepless(Stephan Bodzin Remix)", "Sleepless (Stephan Bodzin Remix)") is True
    assert covers("Free", "Ultra Nate") is False


def test_nothing_in_the_tags_is_a_proposal_not_a_conflict() -> None:
    """The three outcomes, and which is which."""
    assert verdict("Field Recordings", "") == "only in path"
    assert verdict("Techno", "Techno") == "agrees"
    assert verdict("Techno", "House") == "differs"
    assert verdict("117", "59", same=False) == "differs"


def test_loose_ignores_spacing_and_punctuation() -> None:
    """Spelling is not information, when the question is what a name says."""
    assert loose("Mouth to Mouth") == loose("Mouth To  Mouth")
    assert loose("O.C.M.S.") == loose("OCMS")


def test_a_dj_filename_that_agrees_produces_no_rows(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """The 99% case: everything the name says, the tags say too."""
    monkeypatch.setattr(
        "gmuse_tools.paths.tags_of",
        lambda _: {"tbpm": "132.00", "tkey": "C#m", "artist": "Christopher Just", "title": "I'm A Disco Dancer"},
    )
    at = tmp_path / "132 - C#m - Christopher Just - I'm A Disco Dancer.mp3"
    at.write_bytes(b"\x00")
    assert [f.verdict for f in findings(at)] == ["agrees"] * 4


def test_a_number_is_never_read_as_an_artist(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    """A DJ name whose key this build does not know is still not `117` by `Cm7`.

    The dj pattern fails, and the looser `artist - title` one then reads the
    tempo as the artist. That produced rows claiming Deerhoof was called 117.
    """
    monkeypatch.setattr("gmuse_tools.paths.tags_of", lambda _: {"artist": "Deerhoof", "title": "Twin Killers"})
    at = tmp_path / "117 - Cm7 - Deerhoof - Twin Killers.mp3"
    at.write_bytes(b"\x00")
    assert findings(at) == []


def test_a_mastering_marker_is_the_one_thing_only_the_path_knows(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """144 files carry `_pn` and nothing in the tags says so."""
    monkeypatch.setattr("gmuse_tools.paths.tags_of", lambda _: {"artist": "Audion", "title": "Mouth to Mouth"})
    at = tmp_path / "Audion - Mouth to Mouth_pn.mp3"
    at.write_bytes(b"\x00")
    marks = [f for f in findings(at) if f.what == "processed by a mastering tool"]
    assert len(marks) == 1
    assert marks[0].verdict == "only in path"


def test_a_folder_that_extends_a_thirty_character_album_confirms_it(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """The truncation detector, and why the folder is the better record.

    ID3v1 fields are exactly 30 bytes. Where the folder continues the tag, the
    tag is a stump and the folder holds what was cut off.
    """
    album = tmp_path / "Artist" / "AVE MARIA - SACRED ARIAS AND CHORUSES"
    album.mkdir(parents=True)
    monkeypatch.setattr(
        "gmuse_tools.paths.tags_of", lambda _: {"album": "AVE MARIA - SACRED ARIAS AND C", "artist": "Artist"}
    )
    at = album / "01 Ave Maria.mp3"
    at.write_bytes(b"\x00")
    cut = [f for f in findings(at) if "album truncated" in f.what]
    assert len(cut) == 1
    assert cut[0].verdict == "only in path", "the folder confirms it, so it is a proposal"
    assert cut[0].from_path == "AVE MARIA - SACRED ARIAS AND CHORUSES"


def test_a_thirty_character_album_nothing_confirms_is_only_suspect(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Some albums really are 30 characters. Without corroboration, say less."""
    album = tmp_path / "Artist" / "Exactly Thirty Characters Long"
    album.mkdir(parents=True)
    monkeypatch.setattr(
        "gmuse_tools.paths.tags_of", lambda _: {"album": "Exactly Thirty Characters Long", "artist": "Artist"}
    )
    at = album / "01 Track.mp3"
    at.write_bytes(b"\x00")
    cut = [f for f in findings(at) if "album truncated" in f.what]
    assert [f.verdict for f in cut] == ["suspect"]


def test_the_mastering_marker_is_shared_with_the_quality_pass() -> None:
    """One definition, because both passes ask the same question — #143.

    `paths` finds the marker as something only the path knows; `quality` reads
    it as evidence of a re-encode its own measurement cannot see. Two spellings
    of `_pn` would let one drift.
    """
    from gmuse_tools.quality import processed  # noqa: PLC0415

    assert processed(Path("/x/A - B (Radio edit)_pn.mp3")) is True
    assert processed(Path("/x/A - B.mp3")) is False
    assert processed(Path("/x/openpn.mp3")) is False, "a suffix, not a substring"
