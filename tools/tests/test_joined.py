"""Telling a credit list from a band name — issue #152.

Every case here is a real string from this library, and the two tests that look
redundant are the two bugs a first version actually shipped: comparing a credit
against its own punctuation variant, and reading a spelling difference as a
reordering. Both flagged `Neko Case & Her Boyfriends`, which is a band.
"""

from __future__ import annotations

from gmuse_tools.joined import (
    Verdict,
    all_solo,
    locally,
    members,
    reordered,
    resolve,
    shares_members,
)


def test_a_credit_splits_into_the_names_it_lists() -> None:
    """`&` inside a name still splits — which is why splitting is not the verdict."""
    assert members("Armand Van Helden,Dizzee Rascal") == ("Armand Van Helden", "Dizzee Rascal")
    assert members("2Pac feat. Danny Boy, Syke & CPO") == ("2Pac", "Danny Boy", "Syke", "CPO"), (
        "without `feat.` the first member is `2Pac feat. Danny Boy`, an artist that never existed"
    )
    assert members("Chus & Ceballos, DJ Chus, Pablo Ceballos") == ("Chus", "Ceballos", "DJ Chus", "Pablo Ceballos")
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
