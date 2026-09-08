"""Spelling a genre term one way — the `Contemporary R&b` case.

`CASE` is the table of exceptions title-casing gets wrong, and it was consulted
against the **whole term** while most of its entries are words. So `R&b` was
corrected to `R&B` when it was the entire term and never inside one, and this
library carried `Contemporary R&B` on 125 files and `Contemporary R&b` on 45.

The bare `R&B` was right the whole time, which is what kept it hidden: the
table looked like it worked.
"""

from __future__ import annotations

from gmuse_tools.genre.common import fix_case


def test_an_exception_applies_inside_a_term_as_well_as_alone() -> None:
    """The bug. One of these was right and the other was not."""
    assert fix_case("R&b") == "R&B"
    assert fix_case("Contemporary R&b") == "Contemporary R&B"


def test_a_term_already_right_is_left_alone() -> None:
    """125 of the 170 files were correct, and a repair must not churn them."""
    assert fix_case("R&B") == "R&B"
    assert fix_case("Contemporary R&B") == "Contemporary R&B"
    assert fix_case("IDM Something") == "IDM Something", "an acronym is not re-cased"


def test_the_whole_term_wins_over_its_words() -> None:
    """Two entries mean something only as a phrase.

    `Drum N Bass` is Discogs' spelling and becomes `Drum & Bass` — not three
    words corrected separately, which would give `Drum N Bass` back.
    """
    assert fix_case("Drum N Bass") == "Drum & Bass"
    assert fix_case("Uk Garage") == "UK Garage"


def test_a_hyphen_keeps_what_follows_it_lowercase() -> None:
    """The convention, not a gap.

    27 terms in this library are spelled this way — `Hip-hop`, `Post-punk`,
    `Lo-fi`, `Avant-garde` — so the one deviation, `Non-Music`, is what wants
    fixing rather than the rule. A change here would respell all 27.
    """
    for term in ("Hip-hop", "Post-punk", "Lo-fi", "Avant-garde", "Synth-pop"):
        assert fix_case(term) == term
    assert fix_case("Non-Music") == "Non-music"


def test_an_ampersand_with_spaces_around_it_was_never_the_problem() -> None:
    """`Drum & Bass` splits into words already, so title-casing handles it.

    Only the *unspaced* separator fell through, which is why exactly one of the
    library's nine ampersand terms was mis-cased.
    """
    for term in ("Drum & Bass", "Rock & Roll", "Stage & Screen", "Melodic House & Techno"):
        assert fix_case(term) == term
