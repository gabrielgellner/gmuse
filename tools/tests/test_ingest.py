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
