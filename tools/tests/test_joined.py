"""Telling a credit list from a band name — issue #152.

Every case here is a real string from this library, and the two tests that look
redundant are the two bugs a first version actually shipped: comparing a credit
against its own punctuation variant, and reading a spelling difference as a
reordering. Both flagged `Neko Case & Her Boyfriends`, which is a band.
"""

from __future__ import annotations

import pytest

import gmuse_tools.joined as jnd
from gmuse_tools import bronze
from gmuse_tools.joined import (
    Verdict,
    all_solo,
    attribute,
    locally,
    members,
    proposals,
    remixed,
    reordered,
    resolve,
    shares_members,
)


def test_a_credit_splits_into_the_names_it_lists() -> None:
    """Splitting is a candidate, not the verdict — but the candidate has to be sane.

    **This asserted `Chus | Ceballos` and that was wrong**, on the theory that
    a bad split is harmless because the corroboration decides. It is not: the
    corroboration *agreed*, because a sibling credit listed the same
    wrongly-split parts in another order, and the pass confirmed its own
    mistake. See the ampersand test below.
    """
    assert members("Armand Van Helden,Dizzee Rascal") == ("Armand Van Helden", "Dizzee Rascal")
    assert members("2Pac feat. Danny Boy, Syke & CPO") == ("2Pac", "Danny Boy", "Syke", "CPO"), (
        "without `feat.` the first member is `2Pac feat. Danny Boy`, an artist that never existed"
    )
    assert members("Chus & Ceballos, DJ Chus, Pablo Ceballos") == (
        "Chus & Ceballos",
        "DJ Chus",
        "Pablo Ceballos",
    )
    assert members("Deerhoof") == ("Deerhoof",), "one name is not a list"


def test_the_same_members_in_another_order_are_a_list() -> None:
    """The strongest signal: a name is stable, a list gets reordered."""
    got = reordered(["Okain,Onno", "Onno, Okain", "Itch-E & Scratch-E"])
    assert got == {"Okain,Onno", "Onno, Okain"}


def test_a_spelling_variant_is_not_a_reordering() -> None:
    """The first bug. `&` against `and` is the same order, so it says nothing.

    Counting it flagged `Neko Case & Her Boyfriends`, which MusicBrainz calls a
    Group.
    """
    assert reordered(["Neko Case & Her Boyfriends", "Neko Case and Her Boyfriends"]) == set()


def test_a_credit_is_not_compared_against_a_variant_of_itself() -> None:
    """The second bug, and the same victim.

    Two spellings of one credit share every member trivially. Requiring the
    member *sets* to differ is what makes the signal mean something.
    """
    both = ["Neko Case & Her Boyfriends", "Neko Case and Her Boyfriends"]
    assert shares_members(both[0], both) is None
    # A genuinely different credit that overlaps by two is evidence.
    assert shares_members("Kiko, Spencer K", ["Kiko, Spencer K", "Kiko,Oxia,Spencer K"]) == "Kiko,Oxia,Spencer K"


def test_one_shared_member_is_not_enough() -> None:
    """Two credits sharing a single person is a collaboration, not a list."""
    assert shares_members("Hank Williams & Rod Brasfield", ["Hank Williams, Red Foley & Minnie Pearl"]) is None


def test_every_member_credited_alone_is_a_list() -> None:
    """The second signal, and the one #145 was burned by."""
    solo = frozenset({"tomwaits", "crystalgayle"})
    assert all_solo("Tom Waits and Crystal Gayle", solo) is True
    assert all_solo("Angus & Julia Stone", solo) is False


def test_a_band_name_nothing_corroborates_is_left_unsure_not_split() -> None:
    """The safety property: no evidence means no verdict, never a guess."""
    got = locally(["Itch-E & Scratch-E", "Angus & Julia Stone", "Deerhoof"])
    assert got["Itch-E & Scratch-E"].verdict == "unsure"
    assert got["Angus & Julia Stone"].verdict == "unsure"
    assert "Deerhoof" not in got, "a single name is not a case"


def test_the_whole_string_decides_even_when_the_members_also_exist() -> None:
    """A duo whose members record solo is still a duo — not a coin flip."""
    known = {
        "Angus & Julia Stone": {"name": "Angus & Julia Stone", "type": "Group", "id": "x", "note": ""},
        "Angus": {"name": "Angus", "type": "Group", "id": "y", "note": ""},
        "Julia Stone": {"name": "Julia Stone", "type": "Person", "id": "z", "note": ""},
    }
    v = Verdict("Angus & Julia Stone", "unsure", "", ("Angus", "Julia Stone"))
    assert resolve(v, known.get).verdict == "one"


def test_members_without_the_whole_string_are_a_list() -> None:
    """The case that opened #152: neither man is in this library alone."""
    known = {
        "Armand Van Helden": {"name": "Armand van Helden", "type": "Person", "id": "a", "note": ""},
        "Dizzee Rascal": {"name": "Dizzee Rascal", "type": "Person", "id": "b", "note": ""},
    }
    v = Verdict("Armand Van Helden,Dizzee Rascal", "unsure", "", ("Armand Van Helden", "Dizzee Rascal"))
    got = resolve(v, known.get)
    assert got.verdict == "list"


def test_a_member_the_authority_does_not_know_stays_unsure() -> None:
    """`m.O.N.R.O.E.` is not in MusicBrainz, so the credit is not decided."""
    known = {"Daniel Dubb": {"name": "Daniel Dubb", "type": "Person", "id": "a", "note": ""}}
    v = Verdict("Daniel Dubb, m.O.N.R.O.E.", "unsure", "", ("Daniel Dubb", "m.O.N.R.O.E."))
    got = resolve(v, known.get)
    assert got.verdict == "unsure"
    assert "m.O.N.R.O.E." in got.why


def test_a_credit_name_that_is_an_alias_still_resolves() -> None:
    """The name on the tag is the name a record was *credited* to.

    MusicBrainz files that artist under a different one often enough to matter:
    `Puffy AmiYumi` is an alias of `PUFFY`, `Dramacydal` of `Outlawz`. Matching
    the primary name alone reported both as unknown, which read as a limit of
    the method and was a limit of the query.
    """
    known = {
        "2Pac": {"name": "2Pac", "type": "Person", "id": "a", "note": ""},
        "Dramacydal": {"name": "Outlawz", "type": "Group", "id": "b", "note": "filed as Outlawz"},
    }
    v = Verdict("2Pac feat. Dramacydal", "unsure", "", ("2Pac", "Dramacydal"))
    got = resolve(v, known.get)
    assert got.verdict == "list"


def test_a_person_whose_name_contains_an_ampersand_survives() -> None:
    """`Dom & Roland` is one man, Dominic Angas. The end of every string rule."""
    known = {"Dom & Roland": {"name": "Dom & Roland", "type": "Person", "id": "a", "note": "drum & bass artist"}}
    v = Verdict("Dom & Roland", "unsure", "", ("Dom", "Roland"))
    got = resolve(v, known.get)
    assert got.verdict == "one"
    assert "drum & bass artist" in got.why


def test_only_a_settled_list_becomes_a_proposal() -> None:
    """The whole discipline: an unsure credit goes to a person, not the library."""
    work = [
        {"recording": "r1", "artist": "Okain,Onno"},
        {"recording": "r2", "artist": "Itch-E & Scratch-E"},
        {"recording": "r3", "artist": "Deerhoof"},
    ]
    decided = {
        "Okain,Onno": Verdict("Okain,Onno", "list", "reordered", ("Okain", "Onno")),
        "Itch-E & Scratch-E": Verdict("Itch-E & Scratch-E", "unsure", "nothing", ("Itch-E", "Scratch-E")),
    }
    got = proposals(work, decided)
    assert [p["recording"] for p in got] == ["r1"]
    assert got[0]["artists"] == ["Okain", "Onno"]
    assert got[0]["fact"] == "credited"


def test_the_basis_says_which_evidence_settled_it() -> None:
    """A library reading and an authority's answer must not be the same fact."""
    work = [{"recording": "r1", "artist": "A,B"}, {"recording": "r2", "artist": "C,D"}]
    decided = {
        "A,B": Verdict("A,B", "list", "the same members appear in another order", ("A", "B")),
        "C,D": Verdict("C,D", "list", "MusicBrainz holds every member and not the whole", ("C", "D")),
    }
    got = {p["recording"]: p["basis"] for p in proposals(work, decided)}
    assert got == {"r1": "inferred", "r2": "musicbrainz"}


def test_a_credit_is_proposed_as_the_sleeve_spells_it() -> None:
    """MusicBrainz's spelling is a separate claim, and `as_credited` is for it.

    Proposing `Armand van Helden` because an authority files him that way would
    smuggle a rename into a split.
    """
    work = [{"recording": "r1", "artist": "Armand Van Helden,Dizzee Rascal"}]
    decided = {
        "Armand Van Helden,Dizzee Rascal": Verdict(
            "Armand Van Helden,Dizzee Rascal",
            "list",
            "MusicBrainz holds every member and not the whole",
            ("Armand Van Helden", "Dizzee Rascal"),
        )
    }
    assert proposals(work, decided)[0]["artists"] == ["Armand Van Helden", "Dizzee Rascal"]


def test_an_ampersand_inside_a_comma_list_stays_part_of_the_name() -> None:
    """`Chus & Ceballos` is a duo, and the split invented two people.

    The credit is `Chus & Ceballos, DJ Chus, Pablo Ceballos, Leonardo Gonnelli`
    — the duo *is* DJ Chus and Pablo Ceballos, so the list is redundant and not
    wrong. Splitting on the ampersand as well produced `Chus` and `Ceballos`,
    who have never existed.

    Worse, the corroboration agreed: a sibling credit listed the same
    wrongly-split parts in another order, so the pass confirmed its own mistake
    and marked it `list`. A fold is the one thing the model cannot repair.
    """
    assert members("Chus & Ceballos, DJ Chus, Pablo Ceballos, Leonardo Gonnelli") == (
        "Chus & Ceballos",
        "DJ Chus",
        "Pablo Ceballos",
        "Leonardo Gonnelli",
    )


def test_the_last_item_is_where_english_puts_the_ampersand() -> None:
    """So the tail is split by everything, and only the tail."""
    assert members("Dolly Parton, Linda Ronstadt & Emmylou Harris") == (
        "Dolly Parton",
        "Linda Ronstadt",
        "Emmylou Harris",
    )


def test_a_credit_with_no_comma_is_not_a_list_yet() -> None:
    """Every joiner applies, which is what makes a bare pair two."""
    assert members("Stephan Bodzin & Marc Romboy") == ("Stephan Bodzin", "Marc Romboy")


def test_featuring_still_splits_inside_the_list() -> None:
    """The case that put `feat.` in the pattern, and it must not regress.

    `2Pac feat. Danny Boy, Syke & CPO` is four people. A rule that spared every
    joiner inside a comma list would trade one bug for the one it replaced —
    MusicBrainz asked about `2Pac feat. Danny Boy`, an artist that has never
    existed.
    """
    assert members("2Pac feat. Danny Boy, Syke & CPO") == ("2Pac", "Danny Boy", "Syke", "CPO")


def rows(*specs: tuple[str, str, str]) -> list[dict[str, str]]:
    """Work-list rows as `gmuse shelf query` writes them."""
    return [{"recording": r, "artist": credit, "musicbrainz": mbid} for r, credit, mbid in specs]


def test_musicbrainz_naming_several_artists_is_the_split() -> None:
    """The arbiter #152 wanted and could not have: one artist id per artist.

    `Sante,Sidney Charles` is two people, and MusicBrainz says so without any
    rule about commas — and corrects the spelling to `Santé` on the way past.
    """
    got = attribute(
        rows(("r1", "Sante,Sidney Charles", "mb1")),
        {"mb1": ("Santé", "Sidney Charles")}.get,
    )
    assert got.splits == [
        {"fact": "credited", "recording": "r1", "artists": ["Santé", "Sidney Charles"], "basis": "musicbrainz"}
    ]
    assert got.disputed == []


def test_an_ampersand_inside_a_comma_list_survives_because_nothing_parses_it() -> None:
    """`Chus & Ceballos` is one act inside a list of two — the case a string rule cannot get right.

    A comma rule splits it into three and an ampersand rule splits it into
    three differently. MusicBrainz returns two artists because it is reading
    ids, not punctuation.
    """
    got = attribute(
        rows(("r1", "Chus & Ceballos,DJ Chus,Leonardo Gonnelli", "mb1")),
        {"mb1": ("Leonardo Gonnelli", "Chus & Ceballos")}.get,
    )
    assert got.splits[0]["artists"] == ["Leonardo Gonnelli", "Chus & Ceballos"]


def test_one_artist_where_the_credit_names_several_is_reported_not_acted_on() -> None:
    """#145's mistake, arriving from the other side.

    The tag says two and the authority says one. Splitting is not the danger
    here — *taking* the authority's answer is, because writing it would drop a
    credited artist. 48 of the first 90 real answers looked like this.
    """
    got = attribute(
        rows(("r1", "Carl Craig,La Fleur", "mb1")),
        {"mb1": ("Carl Craig",)}.get,
    )
    assert got.splits == [], "nothing may be written from a disagreement"
    assert len(got.disputed) == 1
    assert got.disputed[0]["credit"] == "Carl Craig,La Fleur"


def test_a_band_name_the_authority_confirms_produces_nothing_at_all() -> None:
    """`Angus & Julia Stone` is one act, and the right output for that is silence.

    Not a proposal, and not a dispute either: the credit and the authority
    agree, so there is nothing for anybody to look at.
    """
    got = attribute(rows(("r1", "Angus & Julia Stone", "mb1")), {"mb1": ("Angus & Julia Stone",)}.get)
    assert got.splits == []
    assert got.disputed == []
    assert got.asked == 1


def test_a_confirmed_band_costs_one_lookup_however_many_recordings_it_has() -> None:
    """`Angus & Julia Stone` holds 24 recordings here and a first version asked about all of them.

    A credit the authority calls one artist yields no proposal, so the other
    23 answers could not have changed anything.
    """
    asked: list[str] = []

    def ask(mbid: str) -> tuple[str, ...] | None:
        asked.append(mbid)
        return ("Angus & Julia Stone",)

    band = "Angus & Julia Stone"
    attribute(rows(("r1", band, "mb1"), ("r2", band, "mb2"), ("r3", band, "mb3")), ask)
    assert asked == ["mb1"], "the other two were a rate limit spent to be told the same thing"


def test_a_split_credit_is_asked_about_every_recording() -> None:
    """The opposite case, and why the shortcut is only a shortcut.

    A proposal names one recording, so every recording under a split credit
    needs its own answer — two recordings sharing a credit string are not
    obliged to carry the same artists.
    """
    answers = {"mb1": ("Okain", "Onno"), "mb2": ("Okain", "Onno", "A Third")}
    got = attribute(rows(("r1", "Okain,Onno", "mb1"), ("r2", "Okain,Onno", "mb2")), answers.get)
    assert [line["artists"] for line in got.splits] == [["Okain", "Onno"], ["Okain", "Onno", "A Third"]]


def test_a_failed_lookup_is_not_an_empty_credit() -> None:
    """`None` means no answer was obtained, which is not "nobody is credited".

    The same distinction `bronze` draws by refusing to cache a status of 0: a
    503 run or a reset must not freeze into a permanent verdict.
    """
    got = attribute(rows(("r1", "Okain,Onno", "mb1")), lambda _: None)
    assert got.splits == []
    assert got.disputed == []
    assert got.unanswered == 1


def test_a_row_the_library_cannot_look_up_is_not_asked_about() -> None:
    """No `musicbrainz` means no question to ask — the 7.3% resting on bytes."""
    got = attribute(
        [{"recording": "r1", "artist": "Okain,Onno"}],
        lambda _: pytest.fail("a row with no rung was looked up anyway"),
    )
    assert got.asked == 0


def remix_rows(*specs: tuple[str, str, str, str]) -> list[dict[str, str]]:
    """Work-list rows carrying a title, which is the third of the three sources."""
    return [{"recording": r, "artist": credit, "musicbrainz": mbid, "title": title} for r, credit, mbid, title in specs]


def test_a_performer_and_a_remixer_are_two_roles_not_two_performers() -> None:
    """The shape 47 of 60 disagreements turned out to have.

    MusicBrainz credits the recording to whoever performed it and files the
    remixer as a relationship, so `Adam Beyer,Tiger Stripes` is not a joined
    credit at all — it is one performer and one remixer, and the title says
    which is which.
    """
    got = remixed(
        remix_rows(("r1", "Adam Beyer,Tiger Stripes", "mb1", "Time Flies (Tiger Stripes remix)")),
        {"mb1": ("Adam Beyer",)}.get,
    )
    assert got.lines == [
        {"fact": "contributed", "recording": "r1", "artist": "Adam Beyer", "role": "performer", "basis": "musicbrainz"},
        {"fact": "contributed", "recording": "r1", "artist": "Tiger Stripes", "role": "remixer", "basis": "inferred"},
    ]
    assert got.left == []


def test_the_remixer_is_never_filed_as_a_performer() -> None:
    """`line_of` reads performers, so the wrong role puts the record in the remixer's folder.

    The bug that made the role filter exist, asserted here because this pass
    is the one that hands out both roles at once.
    """
    got = remixed(
        remix_rows(("r1", "Dubfire,Joseph Capriati", "mb1", "Roadkill (Joseph Capriati remix)")),
        {"mb1": ("Dubfire",)}.get,
    )
    performers = [line["artist"] for line in got.lines if line["role"] == "performer"]
    assert performers == ["Dubfire"], "only the artist MusicBrainz credits may perform it"


def test_every_other_name_must_be_in_the_title_before_anything_is_written() -> None:
    """Three sources agree or nothing is written — a two-out-of-three is a guess.

    `Drift (Mr Tophat & Art Alfie Remix)` names both, so both are remixers.
    A title naming only one of two leaves the credit for a person.
    """
    both = remixed(
        remix_rows(("r1", "Art Alfie,Mr Tophat,Scuba", "mb1", "Drift (Mr Tophat & Art Alfie Remix)")),
        {"mb1": ("Scuba",)}.get,
    )
    assert sorted(x["artist"] for x in both.lines if x["role"] == "remixer") == ["Art Alfie", "Mr Tophat"]

    half = remixed(
        remix_rows(("r1", "Art Alfie,Mr Tophat,Scuba", "mb1", "Drift (Mr Tophat Remix)")),
        {"mb1": ("Scuba",)}.get,
    )
    assert half.lines == []
    assert "does not name every other artist" in half.left[0]["why"]


def test_a_bare_mix_names_a_version_and_not_a_person() -> None:
    """`(Original Mix)` is the string this whole distinction exists for.

    The same line `identify.py` draws with `CREDITS_A_PERSON`: without it, a
    version marker becomes a remixer credit.
    """
    got = remixed(
        remix_rows(("r1", "Kiko,Spencer K", "mb1", "Nagual (Original Mix)")),
        {"mb1": ("Kiko",)}.get,
    )
    assert got.lines == []
    assert "credits a person" in got.left[0]["why"]


def test_a_spelling_difference_in_the_title_still_matches() -> None:
    """`Leif Müller` is printed `Leif Muller` on the sleeve, and that is not a different person.

    `key` folds accents for comparison and nothing is minted from it, so this
    cannot make the unrepairable merge the Rust side is careful about.
    """
    got = remixed(
        remix_rows(
            (
                "r1",
                "Anja Schneider,Konstantin Sibold,Leif Müller",
                "mb1",
                "Circle Culture (Konstantin Sibold & Leif Muller Remix)",
            )
        ),
        {"mb1": ("Anja Schneider",)}.get,
    )
    assert sorted(x["artist"] for x in got.lines if x["role"] == "remixer") == ["Konstantin Sibold", "Leif Müller"]


def test_a_credit_musicbrainz_calls_several_is_not_this_pass_s_business() -> None:
    """A genuine split belongs to `attribute`, and asking twice would write it twice."""
    got = remixed(
        remix_rows(("r1", "Okain,Onno", "mb1", "Bruk (Okain & Onno Remix)")),
        {"mb1": ("Okain", "Onno")}.get,
    )
    assert got.lines == []
    assert got.left == [], "not settled here and not unsettled here either — it is the other pass's row"


def test_a_split_is_proposed_on_the_album_as_well_as_the_recording() -> None:
    """The half of #152 that actually names a folder.

    `ingest` credits the recording *and* the release group from one tag, so a
    joined credit is written on both — and `file_under` asks the album first
    (#84). Splitting only the recording made 62 correct splits that moved
    nothing: 171 release groups stayed credited to the joined string.
    """
    rows = [
        {"recording": "r1", "album": "g1", "artist": "Okain,Onno", "album_artist": "Okain,Onno", "musicbrainz": "mb1"}
    ]
    got = attribute(rows, {"mb1": ("Okain", "Onno")}.get)
    assert got.splits == [
        {"fact": "credited", "artists": ["Okain", "Onno"], "basis": "musicbrainz", "recording": "r1"},
        {"fact": "credited", "artists": ["Okain", "Onno"], "basis": "musicbrainz", "album": "g1"},
    ]


def test_a_remix_credit_reaches_the_album_too() -> None:
    """A remix single's album carries the same joined credit its recording does.

    So the performer and the remixer both have to be said twice, or the folder
    keeps the name the tag gave it.
    """
    rows = [
        {
            "recording": "r1",
            "album": "g1",
            "artist": "Dubfire,Joseph Capriati",
            "album_artist": "Dubfire,Joseph Capriati",
            "musicbrainz": "mb1",
            "title": "Roadkill (Joseph Capriati remix)",
        }
    ]
    got = remixed(rows, {"mb1": ("Dubfire",)}.get)
    assert [(x["role"], x.get("recording") or x.get("album")) for x in got.lines] == [
        ("performer", "r1"),
        ("performer", "g1"),
        ("remixer", "r1"),
        ("remixer", "g1"),
    ]


def test_a_row_on_no_album_still_credits_its_recording() -> None:
    """A loose recording is an ordinary row, not a reason to skip the split."""
    got = attribute([{"recording": "r1", "artist": "Okain,Onno", "musicbrainz": "mb1"}], {"mb1": ("Okain", "Onno")}.get)
    assert [x.get("album") for x in got.splits] == [None]
    assert got.splits[0]["recording"] == "r1"


def test_a_compilation_keeps_its_own_credit() -> None:
    """The 47 files this guard exists for.

    A Lomax field recording is credited `Sid Hemphill, Lucius Smith` on the
    performance and `Various Artists` on the album. Those are two different
    claims, and writing the performers onto the album proposed moving 47 files
    out of `Various Artists` into a folder named after nine people.
    """
    rows = [
        {
            "recording": "r1",
            "album": "g1",
            "artist": "Sid Hemphill,Lucius Smith",
            "album_artist": "Various Artists",
            "musicbrainz": "mb1",
        }
    ]
    got = attribute(rows, {"mb1": ("Sid Hemphill", "Lucius Smith")}.get)
    assert [x.get("album") for x in got.splits] == [None], "a compilation's credit is not this credit"
    assert got.splits[0]["recording"] == "r1", "the performance is still split"


def test_a_single_credited_the_same_way_at_both_levels_is_corrected_at_both() -> None:
    """The case correcting the album is the whole point of.

    A remix single carries one joined string on the recording and on the
    album, so leaving the album alone would make the split invisible.
    """
    rows = [
        {
            "recording": "r1",
            "album": "g1",
            "artist": "Okain,Onno",
            "album_artist": "Okain,Onno",
            "musicbrainz": "mb1",
        }
    ]
    got = attribute(rows, {"mb1": ("Okain", "Onno")}.get)
    assert [x.get("album") for x in got.splits] == [None, "g1"]


class TestASilentAuthorityIsNotAnUnknownArtist:
    """**A throttle is not "MusicBrainz has never heard of them"** — #262.

    `ask_musicbrainz` folded "no answer was obtained" into "not a 200" and returned
    `None`, and the caller memoises per name — so one silence would have stood for that
    name for the rest of the run, and this pass proposes splits from exactly that
    signal.
    """

    @staticmethod
    def silent(*_args: object, **_kwargs: object) -> tuple[int, object]:
        """A `bronze.get` that never obtained an answer."""
        return bronze.UNANSWERED, None

    def test_a_silent_lookup_raises(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """A name nobody answered about is undecided, not unknown."""
        monkeypatch.setattr(jnd.bronze, "get", self.silent)
        with pytest.raises(bronze.UnansweredError):
            jnd.ask_musicbrainz(None, "Sonya Elise")

    def test_an_authority_with_no_such_artist_still_answers(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """The instrument. "Nobody by that name" is a real answer and this pass acts on it."""

        def get(*_args: object, **_kwargs: object) -> tuple[int, object]:
            return 200, {"artists": []}

        monkeypatch.setattr(jnd.bronze, "get", get)
        assert jnd.ask_musicbrainz(None, "Nobody At All") is None
