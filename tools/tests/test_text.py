"""The accent fold, and the alphabet policies on top of it — #296, §19."""

from __future__ import annotations

from gmuse_tools.genre.common import compare_key, tracklist_match
from gmuse_tools.identify import folded
from gmuse_tools.text import fold_accents


def test_the_fixture_really_differs_by_an_accent() -> None:
    """The instrument. Without it every claim below can pass on two equal strings.

    Computed off the raw strings rather than off either normaliser, so it fails
    when the fixture is wrong and passes when only the fold is wrong — the
    independence §13 asks for.
    """
    assert "Electric Café" != "Electric Cafe"
    assert len("Electric Café") == len("Electric Cafe")
    assert {c for c in "Electric Café"} - {c for c in "Electric Cafe"} == {"é"}


def test_an_accent_is_folded_not_stripped() -> None:
    """§19's rule, and the failure that wrote it down.

    `Lvis Mejía` reaches this library as `Lvis Meji` + U+00B4 + `a`, a literal
    acute some tagger left behind. A normaliser that *deletes* non-ASCII maps
    the two spellings to `lvismejia` and `lvismeja`, which do not match.
    """
    literal_acute = "Lvis Meji´a"
    combining = "Lvis Mejía"
    assert literal_acute != combining, "the fixture is not the two spellings"
    assert folded(literal_acute) == folded(combining) == "lvismejia"

    # Stripping rather than folding is the thing that must not come back.
    assert "".join(c for c in combining if c.isascii()).lower().replace(" ", "") == "lvismeja"


def test_compare_key_folds_the_accent_that_cost_kraftwerk_a_verdict() -> None:
    """The case #296 was reopened on.

    Seven tracks, every duration within 2s of MusicBrainz, one accented
    character — and `tracklist_match` intersects *sets*, so there is no
    threshold to absorb it. The verdict came back `named (86%)` where `exact`
    is the only one a tool acts on unattended.
    """
    assert compare_key("Electric Café") == compare_key("Electric Cafe")

    ours = [
        "Boing Boom Tschak",
        "Techno Pop",
        "Musique Non Stop",
        "The Telephone Call",
        "House Phone",
        "Sex Object",
        "Electric Café",
    ]
    theirs = list(ours[:-1]) + ["Electric Cafe"]
    verdict, share = tracklist_match(ours, theirs)
    assert (verdict, share) == ("exact", 1.0), "one accent still costs a seventh of the verdict"


def test_a_non_latin_title_survives_compare_key() -> None:
    """The guard on *not* reusing `identify.folded` wholesale — #296.

    `folded` keeps `[a-z0-9]`, which is right for a Latin-script artist name and
    wrong here: a title in another script folds to the empty string, every such
    title then keys alike, and `tracklist_match`'s set intersection reports them
    as the same track. No title in the library is entirely non-Latin today, so
    this guards a hazard rather than a bug — and it is the silent, wrong kind.
    """
    hello, goodbye = "こんにちは", "さようなら"
    assert folded(hello) == folded(goodbye) == "", "the premise of this guard has changed"
    assert compare_key(hello) != ""
    assert compare_key(hello) != compare_key(goodbye)

    verdict, _ = tracklist_match([hello], [goodbye])
    assert verdict == "weak", "two different non-Latin titles were called the same track"


def test_the_fold_does_not_loosen_what_compare_key_already_decided() -> None:
    """Regression guard on the two lines `compare_key` holds deliberately.

    A trailing `(feat. …)` goes, because MusicBrainz keeps featured artists in
    the artist credit; a remix suffix stays, because a remix is a different
    recording.
    """
    assert compare_key("Song (feat. X)") == compare_key("Song")
    assert compare_key("Sleepless (Stephan Bodzin Remix)") != compare_key("Sleepless")
    # And the punctuation cases #296 tabulated, which were already folded
    # before it was filed — asserted so a future change cannot quietly undo them.
    assert compare_key("Warden's Wife") == compare_key("Warden’s Wife")
    assert compare_key("Men Men Men") == compare_key("Men, Men, Men")
    assert compare_key("Foot in Heaven/Hell") == compare_key("Foot in Heaven / Hell")
    assert compare_key("Diddly Bo Traditional") == compare_key("Diddly Bo (traditional)")
    # The one real difference on that record stays a difference.
    assert compare_key("Fiddle F**k Explicit") != compare_key("Fiddle Fuck")


def test_nfkd_folds_a_compatibility_form_too() -> None:
    """NFKD rather than NFD, so a ligature and a full-width digit fold."""
    assert fold_accents("ﬁle") == "file"
    assert fold_accents("１２３") == "123"
