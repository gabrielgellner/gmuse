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

from collections.abc import Callable
from typing import TYPE_CHECKING

import pytest

import gmuse_tools.ingest as ing
from gmuse_tools.ingest import duration_share, missing_tracks, placeholders

#: A 404 is MusicBrainz saying it does not hold this — an answer, unlike a 503.
HTTP_NOT_FOUND = 404
HTTP_OK = 200

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
    assert got == [{"ours": "I Found a Bot (Bonus Track)", "theirs": "I Found a Boy"}]


def test_pairing_survives_a_different_running_order() -> None:
    """The verdict is computed on sets, so display must not assume position."""
    from gmuse_tools.ingest import diffs  # noqa: PLC0415

    got = diffs(["Tuming Tables", "Someone Like You"], ["Someone Like You", "Turning Tables"])
    assert got == [{"ours": "Tuming Tables", "theirs": "Turning Tables"}]


def test_a_title_with_nothing_like_it_is_shown_alone() -> None:
    """The interesting case: the records really differ, rather than one being spelled oddly."""
    from gmuse_tools.ingest import diffs  # noqa: PLC0415

    got = diffs(["Need You Now"], ["Hiding My Heart"])
    assert {"ours": "Need You Now", "theirs": None} in got
    assert {"ours": None, "theirs": "Hiding My Heart"} in got


def test_nothing_differing_says_nothing() -> None:
    """An exact match has no diff to print."""
    from gmuse_tools.ingest import diffs  # noqa: PLC0415

    assert diffs(["One", "Two"], ["One", "Two"]) == []


def test_every_difference_is_shown_and_the_cell_cannot_break_the_file() -> None:
    """All of them, because accepting a release accepts all of its titles.

    This joined the first three and finished `+9 more`, which hid the answer to
    the only question the row exists to ask — and the hidden tail is where a
    wrong edition lives.
    """
    from gmuse_tools.ingest import as_cell, diffs  # noqa: PLC0415

    # Nothing here pairs — `Ours 3` is not close to `Theirs 3` — so each side
    # is reported alone, which is 24 entries and the strongest form of the
    # point: none of them may be dropped.
    ours = [f"Ours {i}" for i in range(12)]
    theirs = [f"Theirs {i}" for i in range(12)]
    got = diffs(ours, theirs)
    assert len(got) == 24, "every difference, not the first three"

    cell = as_cell(got)
    assert "more" not in cell
    assert "Ours 11" in cell
    assert "Theirs 11" in cell

    # **A tab in a title would otherwise make `write_tsv` refuse the row**, so
    # the column that most wants free text becomes the one that cannot break
    # the file.
    nasty = as_cell(diffs(["A\tB"], ["C\nD"]))
    assert "\t" not in nasty
    assert "\n" not in nasty

    # And the titles stay readable, which is the point of them.
    assert "Me\u2019Shell" not in as_cell(diffs(["Me\u2019Shell"], ["Meshell"]))
    assert as_cell([]) == ""


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


def test_a_track_number_that_is_not_a_number_does_not_end_the_survey() -> None:
    """One bad file took forty-four albums down with it.

    `int(tag["TRCK"])` is only ever used to order a tracklist for comparison,
    and the Aphex Twin corpus carries the vinyl side there — `One4`, `A1`,
    `B2`. The survey crashed on the first of them, losing every rate limit
    spent reaching it and the review file that made the work legible.
    """
    from gmuse_tools.ingest import position  # noqa: PLC0415

    assert position("4") == 4
    assert position(" 7 ") == 7
    # A numbered take is still that number.
    assert position("12a") == 12
    # And a side is not a position, which is honest rather than clever.
    for side in ("One4", "A1", "B2", "", "  "):
        assert position(side) == 0, side


def test_the_album_searched_for_is_the_one_the_tags_name(tmp_path: Path) -> None:
    """A query that cannot match is not absence — lesson 13.

    This corpus files albums as `[1989] Red EP [1993]`, so searching
    `release:"[1989] Red EP [1993]"` returned nothing for all fifty albums.
    Fifty rows of `no candidate` that said only that the question was malformed.
    """
    from mutagen.flac import FLAC  # noqa: PLC0415

    from gmuse_tools.ingest import tagged_album  # noqa: PLC0415

    folder = album(tmp_path, "[1989] Red EP [1993]", ["1", "2", "3"])
    for path in sorted(folder.iterdir()):
        flac = FLAC(path)
        flac["album"] = ["Red EP"]
        flac.save()
    assert tagged_album(sorted(folder.iterdir())) == "Red EP"


def test_a_folder_holding_two_records_names_neither(tmp_path: Path) -> None:
    """A majority, not the first file.

    A tie is the honest answer to a folder that holds two albums, and the
    folder name then stands — the same refusal `missing_tracks` makes one
    level up.
    """
    from mutagen.flac import FLAC  # noqa: PLC0415

    from gmuse_tools.ingest import tagged_album  # noqa: PLC0415

    folder = album(tmp_path, "Two Records", ["1", "2"])
    for path, name in zip(sorted(folder.iterdir()), ["One", "Other"], strict=True):
        flac = FLAC(path)
        flac["album"] = [name]
        flac.save()
    assert tagged_album(sorted(folder.iterdir())) == ""


def track(title: str, position: int) -> dict[str, object]:
    """One MusicBrainz track, in the shape `apply` reads."""
    return {"title": title, "position": position}


def test_an_exact_match_can_still_be_in_a_different_order() -> None:
    """`exact` is computed on sets and says nothing about the running order.

    Two of the thirteen albums accepted from the Aphex Twin review were `exact`
    at 100% with two tracks transposed, and `zip` would have tagged each with
    the other's title. Silent, and worst on the strongest verdict — a high
    share is exactly when every title is present on both sides, which is
    exactly when a reordering leaves no trace in the diff.
    """
    from pathlib import Path  # noqa: PLC0415

    from gmuse_tools.ingest import seated  # noqa: PLC0415

    ours = [(Path("7.mp3"), "Every Day"), (Path("8.mp3"), "Arched Maid Via RDJ")]
    theirs = [({}, track("Arched Maid Via RDJ", 7)), ({}, track("Every Day", 8))]

    got = [(p.name, t["title"]) for (p, _), (_, t) in seated(ours, theirs)]
    assert got == [("7.mp3", "Every Day"), ("8.mp3", "Arched Maid Via RDJ")]


def test_a_near_twin_does_not_steal_an_exact_title() -> None:
    """Best pair first, not first-come.

    `We Have Arrived (Aphex Twin QQT mix)` is nearly as close to the `TTQ`
    track as to its own, so assigning greedily in our order would let the first
    file claim the wrong one. Taking every candidate strongest-first lets each
    exact title win before its near-twin gets a chance.
    """
    from pathlib import Path  # noqa: PLC0415

    from gmuse_tools.ingest import seated  # noqa: PLC0415

    ours = [
        (Path("a.mp3"), "We Have Arrived (Aphex Twin TTQ mix)"),
        (Path("b.mp3"), "We Have Arrived (Aphex Twin QQT mix)"),
    ]
    theirs = [
        ({}, track("We Have Arrived (Aphex Twin QQT mix)", 1)),
        ({}, track("We Have Arrived (Aphex Twin TTQ mix)", 2)),
    ]

    got = {p.name: t["title"] for (p, _), (_, t) in seated(ours, theirs)}
    assert got["a.mp3"] == "We Have Arrived (Aphex Twin TTQ mix)"
    assert got["b.mp3"] == "We Have Arrived (Aphex Twin QQT mix)"


def test_titles_that_cannot_decide_fall_back_to_position() -> None:
    """The case `named_release` exists for.

    MusicBrainz lists every track of `Studio 1 - Variationen` as `[untitled]`,
    which is the sleeve's own labelling — only the numbering carries meaning.
    There, position is the only signal there is.
    """
    from pathlib import Path  # noqa: PLC0415

    from gmuse_tools.ingest import seated  # noqa: PLC0415

    ours = [(Path("1.mp3"), "(Untitled) Red A1"), (Path("2.mp3"), "(Untitled) Red A2")]
    theirs = [({}, track("[untitled]", 1)), ({}, track("[untitled]", 2))]

    got = [(p.name, t["position"]) for (p, _), (_, t) in seated(ours, theirs)]
    assert got == [("1.mp3", 1), ("2.mp3", 2)]


def test_a_placeholder_title_is_not_a_disagreement() -> None:
    """**A share of 0% means two different things** — #178.

    `(Untitled) Peel Session 01` disagrees with every real title, so a
    tracklist gate reports no agreement — but it has found nothing, rather than
    found a conflict. The same distinction `Held` draws between "I do not hold
    it" and "I do not know how many there are", and a person reading the row
    needs it to know which kind of nothing they are looking at.
    """
    assert placeholders(["(Untitled) Red A1", "(Untitled) Analogue Bubblebath 05"]) == 2
    assert placeholders(["Track 3", "untitled", "Track"]) == 3
    # A real title is never one, including one that merely contains the word.
    assert placeholders(["Windowlicker", "Untitled Love Song", "Girl/Boy Song"]) == 0


def test_duration_is_a_second_opinion_and_not_a_gate() -> None:
    """**Measured over 34 Aphex discs: 6 better, 6 worse, 22 the same** — #178.

    That is noise, so nothing here decides on it. It earns its column on the
    rows where the titles are placeholders and there is no other evidence at
    all — `A Live Collection` shares no title with its release and matches
    every length.
    """
    release = {"media": [{"tracks": [{"length": 60_000}, {"length": 125_000}]}]}
    assert duration_share([60.0, 125.0], release) == 1.0
    # Within the tolerance, because a tagger's number is not exact: 34 files on
    # this library are wrong by more than two seconds.
    assert duration_share([61.5, 123.0], release) == 1.0
    assert duration_share([90.0, 200.0], release) == 0.0
    # **Held against the longer side**, so holding half a record cannot read as
    # a whole agreement.
    assert duration_share([60.0], release) == 0.5
    # Nothing to compare is not zero agreement.
    assert duration_share([60.0], {"media": []}) is None
    assert duration_share([], release) is None


class TestASilentServerIsNotAnEmptyAnswer:
    """**"I do not know" must not be reported as "no"** — #262.

    `bronze.get` returns `UNANSWERED` when a run of 503s or a reset produced no answer at
    all, and says in its own docstring that a caller must not cache it. Every caller in
    `ingest` folded it into "not a 200" anyway, so a throttled MusicBrainz produced `no
    MusicBrainz release matched that artist and album` — for a release MusicBrainz returns
    at score 100 when asked again a minute later.

    That is worse than a wrong message. It is the sentence that sends a person to `shelf
    import --unidentified`, which rests a recording on its bytes: the one outcome
    `ingestion-flow.md` §3 says the whole stage exists to prevent.
    """

    @staticmethod
    def answering(status: int, body: object) -> Callable[..., tuple[int, object]]:
        """A `bronze.get` that always says the same thing."""

        def get(*_args: object, **_kwargs: object) -> tuple[int, object]:
            return status, body

        return get

    def test_a_server_that_did_not_answer_raises(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """No answer is not an answer of nothing."""
        monkeypatch.setattr(ing.bronze, "get", self.answering(ing.bronze.UNANSWERED, None))
        monkeypatch.setattr(ing, "cache", lambda: None)
        with pytest.raises(ing.bronze.UnansweredError):
            ing.asked("https://example.invalid/release", {})

    def test_an_answer_of_nothing_is_still_an_answer(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """The instrument.

        If everything raised, the distinction would be lost the other way round and a
        genuine absence would read as a throttle.
        """
        monkeypatch.setattr(ing, "cache", lambda: None)

        # A 404 is an answer: MusicBrainz says it does not have this.
        monkeypatch.setattr(ing.bronze, "get", self.answering(HTTP_NOT_FOUND, None))
        assert ing.asked("https://example.invalid/release", {}) is None

        # And a 200 carrying an empty result set is an answer too.
        monkeypatch.setattr(ing.bronze, "get", self.answering(HTTP_OK, {"releases": []}))
        assert ing.asked("https://example.invalid/release", {}) == {"releases": []}

    def test_the_search_does_not_swallow_it(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """A throttle must not look like the first query finding nothing.

        `identify` runs two queries and takes the second where the first came back empty, so
        a silence swallowed there spends another rate limit to reach the same wrong answer.
        """
        monkeypatch.setattr(ing.bronze, "get", self.answering(ing.bronze.UNANSWERED, None))
        monkeypatch.setattr(ing, "cache", lambda: None)
        with pytest.raises(ing.bronze.UnansweredError):
            ing.identify("Possessed by Paul James", "Feed the Family", ["Texas Rose"])
