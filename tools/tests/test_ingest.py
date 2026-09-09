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


# ---- #155: what a record has, including what we do not hold ----


def catalogued(tmp_path: Path, name: str, album_ids: list[str], discs: list[str]) -> Path:
    """A folder of FLACs carrying an album id and a disc number each."""
    from mutagen.flac import FLAC  # noqa: PLC0415

    import tests.flacfixture as fixture  # noqa: PLC0415

    d = tmp_path / name
    d.mkdir(parents=True)
    for i, (mbid, disc) in enumerate(zip(album_ids, discs, strict=True)):
        p = d / f"{i + 1:02d} Track.flac"
        fixture.write_flac(p)
        tag = FLAC(p)
        if mbid:
            tag["musicbrainz_albumid"] = [mbid]
        tag["discnumber"] = [disc]
        tag.save()
    return d


def test_the_release_and_the_discs_come_from_the_files(tmp_path: Path) -> None:
    """**The tags, not the release**, because the tags are what `import` read.

    A tracklist proposed against an id the library never saw is refused as
    stale, so the id has to be the one the files carried. And the discs are the
    ones present here: a two-disc record often arrives as two folders, and
    proposing a tracklist for a disc nothing holds would be inventing it.
    """
    from gmuse_tools.ingest import held_discs  # noqa: PLC0415

    mbid = "996d1cb0-9c75-4e25-9bd3-5eaff0419f37"
    d = catalogued(tmp_path, "Two Discs", [mbid, mbid], ["1", "2"])
    assert held_discs(d) == (mbid, {1, 2})


def test_a_folder_two_records_got_mixed_in_is_skipped(tmp_path: Path) -> None:
    """Guessing which pressing is right would catalogue the wrong tracklist over both."""
    from gmuse_tools.ingest import held_discs  # noqa: PLC0415

    d = catalogued(tmp_path, "Mixed", ["aaaa", "bbbb"], ["1", "1"])
    assert held_discs(d) == ("", {1})


def test_a_folder_with_no_album_id_is_skipped(tmp_path: Path) -> None:
    """Without one, `import` minted the release from the *group* and the ids disagree."""
    from gmuse_tools.ingest import held_discs  # noqa: PLC0415

    d = catalogued(tmp_path, "Untagged", ["", ""], ["1", "1"])
    assert held_discs(d) == ("", {1})


def test_a_slot_names_the_recording_and_not_the_track() -> None:
    """The one thing that has to be right, and the two ids look alike.

    MusicBrainz gives a release entry a `track` id and the performance filling
    it a `recording` id. The recording is what a copy bought later — off any
    release at all — would also be, so it is what makes a slot converge with a
    file. The track id would mint a slot nothing could ever fill.

    And the artists go as a **list**: the joined line would mint one artist
    called `A feat. B`, which is the #152 bug, asserted on a stronger rung than
    the tag it came from.
    """
    from gmuse_tools.ingest import slots_of  # noqa: PLC0415

    disc = {
        "tracks": [
            {
                "id": "the-track-entry",
                "position": 1,
                "title": "Elephant Gun",
                "recording": {"id": "the-performance", "title": "Elephant Gun"},
                "artist-credit": [
                    {"name": "Beirut", "artist": {"id": "e0e1ce9c", "name": "Beirut", "sort-name": "Beirut"}}
                ],
            },
            # A track MusicBrainz has no recording for is dropped rather than
            # given an invented one: a slot's identity *is* its recording.
            {"id": "no-performance", "position": 2, "title": "Unlinked", "recording": {}},
        ]
    }
    assert slots_of(disc) == [
        {
            "position": 1,
            "recording": "the-performance",
            "title": "Elephant Gun",
            "artists": ["Beirut"],
        }
    ]


def test_the_sleeve_title_is_sent_only_when_the_release_really_renames_it() -> None:
    """MusicBrainz carries both titles, and this used to collapse them.

    The Rust side sent `title_on_release: None` on every row with the note that
    "a tracklist read straight off the release cannot tell the difference". It
    can — it just was not looking. Measured on *Luna*, 3 of 34 slots differ and
    only one is a rename; the other two are a non-breaking hyphen and a curly
    apostrophe.
    """
    from gmuse_tools.ingest import slots_of  # noqa: PLC0415

    def track(n: int, printed: str, called: str) -> dict[str, object]:
        return {
            "position": n,
            "title": printed,
            "recording": {"id": f"rec-{n}", "title": called},
            "artist-credit": [{"name": "Stephan Bodzin", "artist": {"id": "a", "name": "Stephan Bodzin"}}],
        }

    slots = slots_of(
        {
            "tracks": [
                track(9, "Phobos (Synthapella)", "Phobos"),
                # Typography, not a rename: a non-breaking hyphen and a curly
                # apostrophe. Claiming these would put a rename on rows where
                # MusicBrainz merely normalised a dash.
                track(24, "Phobos (Pan-Pot remix)", "Phobos (Pan‐Pot remix)"),
                track(34, "Triton (Gaiser's Trial Tone remix)", "Triton (Gaiser’s Trial Tone remix)"),
            ]
        }
    )
    renamed = {s["position"]: s.get("title_on_release") for s in slots}
    assert renamed == {9: "Phobos (Synthapella)", 24: None, 34: None}

    # **And the recording's own title is what `title` carries**, because that
    # is the field the Rust side catalogues the *recording* with. Sending the
    # track's would have named the recording after the sleeve.
    assert slots[0]["title"] == "Phobos"
