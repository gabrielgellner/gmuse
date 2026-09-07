"""Refusing a folder that is part of a record rather than a record — the Adele case.

`Adele/21` arrived in three folders: fifteen tracks in one, two bonus tracks in
another, one Target bonus disc in a third. MusicBrainz has a release covering
all eighteen, and no folder-shaped search can ever find it — but worse, a
two-track folder searched on its own will match some unrelated two-track
single, confidently and exactly.

The fragment says so itself: those two files are numbered `12/13` and `13/13`.
So the check reads the tags, never the folder name — a rule about names is how
`Studio 1 - Variationen` came to be held twice.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

from gmuse_tools.ingest import missing_tracks

if TYPE_CHECKING:
    from pathlib import Path


def album(tmp_path: Path, name: str, numbers: list[str], discs: list[str] | None = None) -> Path:
    """A folder of FLACs carrying the track numbers given, and nothing else."""
    from mutagen.flac import FLAC  # noqa: PLC0415

    import tests.flacfixture as fixture  # noqa: PLC0415

    d = tmp_path / name
    d.mkdir(parents=True)
    for i, n in enumerate(numbers):
        p = d / f"{i + 1:02d} Track.flac"
        fixture.write_flac(p)
        tag = FLAC(p)
        tag["tracknumber"] = [n]
        if discs:
            tag["discnumber"] = [discs[i]]
        tag.save()
    return d


def test_a_fragment_of_an_album_is_named_as_one(tmp_path: Path) -> None:
    """Two of thirteen, and the tags are what say so."""
    d = album(tmp_path, "21 (Bonus Tracks)", ["12/13", "13/13"])
    short = missing_tracks(d)
    assert len(short) == 1
    assert "2 of 13" in short[0]
    assert "missing [1, 2, 3" in short[0]


def test_a_whole_album_is_not_flagged(tmp_path: Path) -> None:
    """Three of three says nothing is missing, so nothing is said."""
    d = album(tmp_path, "Whole", [f"{i}/3" for i in (1, 2, 3)])
    assert missing_tracks(d) == []


def test_a_folder_that_declares_no_total_is_left_alone(tmp_path: Path) -> None:
    """Saying nothing about how whole it should be is not evidence of a gap."""
    d = album(tmp_path, "Bare", ["1", "2"])
    assert missing_tracks(d) == []


def test_each_disc_declares_its_own_total(tmp_path: Path) -> None:
    """A two-disc album has two tracks numbered 1, and two totals to satisfy."""
    d = album(tmp_path, "Double", ["1/2", "2/2", "1/2", "2/2"], discs=["1", "1", "2", "2"])
    assert missing_tracks(d) == []

    short = album(tmp_path, "HalfSecond", ["1/2", "2/2", "1/2"], discs=["1", "1", "2"])
    got = missing_tracks(short)
    assert len(got) == 1
    assert got[0].startswith("disc 2:")


def test_a_difference_shows_both_sides() -> None:
    """A reviewer is asking what it becomes, not what we have."""
    from gmuse_tools.ingest import diffs  # noqa: PLC0415

    got = diffs(["Rolling in the Deep", "I Found a Bot (Bonus Track)"], ["Rolling in the Deep", "I Found a Boy"])
    assert got == "'I Found a Bot (Bonus Track)' -> 'I Found a Boy'"


def test_pairing_survives_a_different_running_order() -> None:
    """The verdict is computed on sets, so display must not assume position."""
    from gmuse_tools.ingest import diffs  # noqa: PLC0415

    got = diffs(["Tuming Tables", "Someone Like You"], ["Someone Like You", "Turning Tables"])
    assert got == "'Tuming Tables' -> 'Turning Tables'"


def test_a_title_with_nothing_like_it_is_shown_alone() -> None:
    """The interesting case: the records really differ, rather than one being spelled oddly."""
    from gmuse_tools.ingest import diffs  # noqa: PLC0415

    got = diffs(["Need You Now"], ["Hiding My Heart"])
    assert "'Need You Now' -> (nothing like it)" in got
    assert "(nothing like it) -> 'Hiding My Heart'" in got


def test_nothing_differing_says_nothing() -> None:
    """An exact match has no diff to print."""
    from gmuse_tools.ingest import diffs  # noqa: PLC0415

    assert diffs(["One", "Two"], ["One", "Two"]) == ""
