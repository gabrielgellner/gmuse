"""Which version of a song a recording is — issue #171.

Every string here is a real bracket from this library, and the tests that look
paranoid are guarding the one number that matters: **306 of 1,114
parentheticals are part of the title, not a version.** A rule that got those
wrong would delete `(to Take My Man)` from a Loretta Lynn song and call it a
rendering.
"""

from __future__ import annotations

from gmuse_tools.version import SPELLINGS, TERMS, brackets_in, normalise, read, survey


def test_every_spelling_maps_into_the_vocabulary() -> None:
    """One place to rename a term, and nothing drifts.

    The same discipline `hierarchy.NARROWER` follows: the table a tag is read
    through and the terms the model holds cannot disagree, because one is
    derived from the other.
    """
    loose = {t for terms in SPELLINGS.values() for t in terms} - TERMS
    assert not loose, f"spellings map to terms nothing declares: {sorted(loose)}"


def test_a_curated_term_becomes_a_version() -> None:
    """The whole point, on the plainest cases."""
    assert read("live") == ("version", ("live",))
    assert read("Radio Edit") == ("version", ("radio-edit",))
    assert read("Acoustic Version") == ("version", ("acoustic",))


def test_a_bracket_that_is_part_of_the_title_is_left_alone() -> None:
    """**306 of them**, which is more than a quarter of every bracket here.

    `You Ain't Woman Enough (to Take My Man)` and `Jesse James (Just Ask)` are
    the songs' names. Both were stripped by a render and restored by hand, and
    that is why this pass never guesses.
    """
    for title_part in (
        "to Take My Man",
        "Just Ask",
        "Sooner or Later",
        "Luke the Drifter",
        "Antichrist Television Blues",
    ):
        verdict, terms = read(title_part)
        assert terms == (), f"{title_part!r} was read as a version"
        assert verdict.startswith("unrecognised")


def test_nothing_is_matched_on_a_substring() -> None:
    """The rule that protects the 306, stated as a test.

    `mix` sits inside `Danny Morris mix`, `Monotone mix` and `Hell mix` — three
    remix names — and a substring rule would file all three as a version of
    something. The whole bracket is looked up or nothing is.
    """
    for remix_name in ("Danny Morris mix", "Monotone mix", "Hell mix", "Knee Length mix"):
        assert read(remix_name)[1] == (), f"{remix_name!r} matched on a fragment"


def test_a_remixer_is_a_credit_and_not_a_version() -> None:
    """`Contributed` with role `remixer` is where these go (#170, #145).

    Filing them here would put a person's name on an axis of renderings, and
    they are matched only so they stay out of the review file, where 206 of
    them would drown the forms a person actually has to read.
    """
    verdict, terms = read("Fur Coat remix")
    assert terms == ()
    assert "credit" in verdict


def test_the_original_is_a_claim_and_silence_is_not() -> None:
    """A first version of the table dropped `(Original Mix)` as "the default".

    That confuses what is usually true with what has been *said*, which is the
    distinction this library is built on — `library-api.md`: a claim is not a
    value. No version term means nobody has said, not that it is the original,
    so `original` is the only way to state it positively.

    It is load-bearing already: `identify.original_among` finds the recording a
    remix was made out of partly by reading this exact bracket.
    """
    assert read("Original Mix") == ("version", ("original",))
    assert read("original version") == ("version", ("original",))
    assert read("Extended Original Mix") == ("version", ("original", "extended-mix"))


def test_what_a_track_is_is_a_different_axis() -> None:
    """`intro` says what the *track* is, not which rendering of a work.

    That is `facet::FUNCTION` if it is anywhere, and guessing across axes is
    how a taxonomy stops being trustworthy.
    """
    for other in ("intro", "Outro", "interlude", "bonus track"):
        verdict, terms = read(other)
        assert terms == ()
        assert "axis" in verdict


def test_a_compound_bracket_is_two_facts() -> None:
    """`rehearsal, mono` says two things and collapsing it loses one."""
    assert read("rehearsal, mono") == ("version", ("rehearsal", "mono"))
    assert read("live acoustic") == ("version", ("live", "acoustic"))


def test_a_numbered_take_is_the_one_pattern() -> None:
    """It cannot be enumerated, and the number is what makes it a claim.

    `take` alone is an ordinary word — `Take It All`, `to Take My Man` — so the
    pattern is anchored end to end.
    """
    assert read("take 4") == ("version", ("alternate-take",))
    assert read("Take 19") == ("version", ("alternate-take",))
    assert read("Take It All")[1] == ()
    assert read("to Take My Man")[1] == ()


def test_typography_does_not_decide_a_match() -> None:
    """A catalogue and a tagger disagree about quotes constantly.

    Folded for the comparison only — nothing is minted from it, so this cannot
    make the unrepairable merge the Rust side is careful about.
    """
    assert normalise("Don’t Look") == "don't look"
    assert normalise("  LIVE  ") == "live"


def test_a_title_can_carry_more_than_one_bracket() -> None:
    """And each is read on its own."""
    assert brackets_in("Ghettochip Malfunction (Hell Yes) (remixed by 8-Bit)") == ["Hell Yes", "remixed by 8-Bit"]


def test_a_proposal_is_written_only_for_a_known_term() -> None:
    """The safety property: an unknown form produces no fact at all."""
    lines, unknown = survey(
        [
            {"recording": "r1", "title": "Someone Like You (live acoustic)"},
            {"recording": "r2", "title": "You Ain't Woman Enough (to Take My Man)"},
            {"recording": "r3", "title": "Nagual (Original Mix)"},
        ]
    )
    assert [(x["recording"], x["qualifier"]) for x in lines] == [
        ("r1", "live"),
        ("r1", "acoustic"),
        ("r3", "original"),
    ]
    assert all(x["basis"] == "inferred" for x in lines), "the term is ours; the tag only carried the string"
    assert [u["form"] for u in unknown] == ["to take my man"], "and the one nobody knows is reported"
