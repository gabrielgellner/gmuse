"""The rules that decide what counts as the same music held twice.

The nomination rule is the one worth pinning. Tightening it is always
tempting and is how this pass would come to find nothing: matching on the
title as well looks obviously right, scores 0 of 9 on the only real
duplicates in the library, and reports success.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import gmuse_tools.duplicates as dup

if TYPE_CHECKING:
    from collections.abc import Callable

    import pytest
from gmuse_tools.duplicates import (
    FLOOR,
    IDENTICAL,
    Copy,
    Findings,
    Suspect,
    agreement,
    examine,
    keeping,
    nominate,
    render,
    why,
)


def copy(**over: object) -> Copy:
    """One file, with everything that is not under test held fixed."""
    fields: dict[str, object] = {
        "recording": "r1",
        "encoding": "e1",
        "locator": "/m/Thomas Brinkmann/Studio1/01 Variation1.mp3",
        "title": "Variation1",
        "artist": "Thomas Brinkmann",
        "seconds": 320.03,
        "bitrate": 320_000,
        "channels": 2,
        "sample_rate": 44_100,
        "size": 12_915_974,
        "tags": 14,
        "musicbrainz": False,
        "replaygain": True,
    }
    fields.update(over)
    return Copy(**fields)  # type: ignore[arg-type]


class TestNominate:
    """What gets fingerprinted at all — and a miss here is never revisited."""

    def test_the_copy_that_lost_its_title_is_still_nominated(self) -> None:
        """**The whole point.** The one worth finding is the untagged one.

        Adding the title to the nomination key finds all the pairs nobody
        needed and none of the nine that were real.
        """
        pairs = nominate(
            [
                copy(),
                copy(encoding="e2", title="[untitled]", locator="/m/Thomas Brinkmann/Studio 1/x.mp3"),
            ]
        )
        assert len(pairs) == 1

    def test_a_different_artist_is_not_nominated(self) -> None:
        """Two tracks of one length is a coincidence, and the library has 70,540."""
        assert nominate([copy(), copy(encoding="e2", artist="Someone Else")]) == []

    def test_a_length_beyond_the_tolerance_is_not_nominated(self) -> None:
        """The fingerprint aligns at the start, so it wants the same length."""
        assert nominate([copy(), copy(encoding="e2", seconds=340.0)]) == []

    def test_one_file_is_not_a_pair_with_itself(self) -> None:
        """Two rows can name one encoding; that is one file, not two."""
        assert nominate([copy(), copy(recording="r2")]) == []

    def test_an_untagged_artist_is_left_out_rather_than_grouped(self) -> None:
        """Empty is not a name — grouping on it would pair everything with it."""
        assert nominate([copy(artist=""), copy(encoding="e2", artist="")]) == []


class TestAgreement:
    def test_the_same_fingerprint_agrees_completely(self) -> None:
        """The trivial end, which is also the one the real pairs hit."""
        assert agreement([1, 2, 3], [1, 2, 3]) == 1.0

    def test_an_inverted_fingerprint_agrees_not_at_all(self) -> None:
        """And the other end, so the scale is known to run the whole way."""
        assert agreement([0, 0], [0xFFFFFFFF, 0xFFFFFFFF]) == 0.0

    def test_nothing_to_compare_is_not_a_match(self) -> None:
        """`fpcalc` returns nothing for a file it cannot decode.

        Read as 1.0 that would report every unreadable file as a duplicate of
        every other one.
        """
        assert agreement([], [1, 2, 3]) == 0.0

    def test_the_floor_clears_the_noise_and_nothing_more(self) -> None:
        """The floor clears the noise and stops there.

        Unrelated audio agrees on about half its bits, and 2,458 of 2,603
        measured pairs sit at 0.5. The floor has to be above that and below
        every real pair somebody would want to see — the Pixies' `Holiday
        Song` against `The Holiday Song` scores 0.754.
        """
        assert 0.5 < FLOOR < 0.754
        assert FLOOR < IDENTICAL < 1.0


class TestBand:
    """Which list a pair lands in — and that it stays a statement about audio.

    The band replaced a `verdict` that read `redundant` / `two qualities`.
    Its call on the Brinkmann pair was "the second buys nothing"; the second
    holds Picard's barcode, catalogue number and ASIN. A rule cannot make
    that call, so it no longer tries.
    """

    def test_matching_audio_is_named_for_the_audio(self) -> None:
        """`identical audio` says what was measured — not what to do about it."""
        assert Suspect(copy(), copy(encoding="e2"), 1.0, []).band == "identical audio"

    def test_the_format_does_not_move_a_pair_between_lists(self) -> None:
        """The format must not move a pair between lists.

        The old code sorted on container and bitrate, which is how it came to
        call the better-tagged copy worthless. Two files with the same audio
        belong in the same list whatever they are encoded as.
        """
        other = copy(encoding="e2", locator="/m/Emmylou/Portraits/50 x.ogg", bitrate=160_000)
        assert Suspect(copy(), other, 1.0, []).band == Suspect(copy(), copy(encoding="e2"), 1.0, []).band

    def test_a_lower_score_is_close_rather_than_dismissed(self) -> None:
        """0.883 was Emmylou Harris's `Hello Stranger` against `Hello Stranger`.

        The old cutoff called it not-a-duplicate and printed nothing.
        """
        assert Suspect(copy(), copy(encoding="e2"), 0.883, []).band == "close"


class TestWhy:
    """The reasons, which are what a person acts on."""

    def test_the_folders_that_differ_only_in_spelling_are_named(self) -> None:
        """The evidence that made this an import accident rather than a choice."""
        other = copy(encoding="e2", locator="/m/Thomas Brinkmann/Studio 1/01 Variation1.mp3")
        said = why(copy(), other, 1.0)
        assert any("differ only in spelling" in r for r in said)

    def test_a_missing_title_is_reported_as_missing_not_as_a_difference(self) -> None:
        """An empty title explains why nothing converged them; a different one does not."""
        said = why(copy(), copy(encoding="e2", title=""), 1.0)
        assert any("no title" in r for r in said)

    def test_the_encodings_are_described_and_not_ranked(self) -> None:
        """The encodings are described, never ranked.

        "both mp3 at 320k" is a measurement. "the second buys nothing" was
        the same measurement with a conclusion attached, and the conclusion
        was wrong about the only pair anybody checked by hand.
        """
        said = why(copy(), copy(encoding="e2"), 1.0)
        assert any("both mp3 at 320k" in r for r in said)
        assert not any("buys nothing" in r or "redundant" in r for r in said)

    def test_a_shared_title_is_said_rather_than_left_implicit(self) -> None:
        """Two files, one artist, one title is what a person scans for.

        Woody Guthrie's `Brown Eyes` against `Brown Eyes` scores 0.734 — well
        under the old cutoff, and obviously worth a look.
        """
        assert any('both titled "Variation1"' in r for r in why(copy(), copy(encoding="e2"), 0.734))


class TestKeeping:
    """What each side would cost to lose — the half a person decides on.

    Choosing between the two Brinkmann copies meant hand-inspecting both for
    Picard tags and ReplayGain, *after* the tool had announced which was
    worthless. That evidence belongs in the report.
    """

    def test_a_side_with_musicbrainz_ids_is_named(self) -> None:
        """The Picard copy, whose loss the old verdict called free."""
        said = keeping(copy(musicbrainz=True), copy(encoding="e2"))
        assert any("MusicBrainz ids on the first only" in r for r in said)

    def test_a_side_with_replaygain_is_named(self) -> None:
        """The other copy's advantage, and the reason the choice is a choice."""
        said = keeping(copy(replaygain=False), copy(encoding="e2", replaygain=True))
        assert any("ReplayGain on the second only" in r for r in said)

    def test_what_both_sides_have_is_not_reported(self) -> None:
        """Only a difference helps; a shared property is noise in a long list."""
        assert keeping(copy(), copy(encoding="e2")) == []

    def test_the_tag_counts_are_given_when_they_differ(self) -> None:
        """A blunt proxy for "how much was written here", and it is honest."""
        assert any("tag count 14 vs 31" in r for r in keeping(copy(), copy(encoding="e2", tags=31)))


class TestAccounting:
    """What the report says about the pairs it is not showing.

    Both of these were zero before, and both were zero by construction rather
    than because there was nothing to say. A pass that quietly drops what it
    could not handle reads exactly like a pass that found nothing wrong.
    """

    def test_a_pair_that_could_not_be_fingerprinted_is_its_own_list(self) -> None:
        """`fpcalc` failing scored 0.0, which is also "definitely unrelated".

        Thirty-eight pairs in the real library took that path and the report
        said nothing about any of them.
        """
        found = Findings(suspects=[], below=0, unchecked=[(copy(), copy(encoding="e2"))], pairs=1, files=2)
        said = render(found, root=None)
        assert "could not fingerprint" in said

    def test_the_pairs_below_the_floor_are_counted_out_loud(self) -> None:
        """No silent caps: a bounded report has to say what it bounded."""
        said = render(Findings(suspects=[], below=2458, unchecked=[], pairs=2603, files=2579), root=None)
        assert "2458" in said

    def test_the_report_refuses_the_decision_in_writing(self) -> None:
        """The reader should not have to infer that these are not instructions."""
        said = render(Findings(suspects=[], below=0, unchecked=[], pairs=0, files=0), root=None)
        assert "is yours" in said


class TestExamine:
    """The routing itself, which the `Findings` tests above only assume.

    Building a `Findings` by hand proves the *report* handles an unchecked
    pair; it says nothing about whether one ever reaches it. A mutation that
    deleted the routing left every test above passing.
    """

    @staticmethod
    def two() -> list[Copy]:
        """A pair that nomination will always produce."""
        return [copy(), copy(encoding="e2", locator="/m/Thomas Brinkmann/Studio 1/01 Variation1.mp3")]

    @staticmethod
    def stub(**by_side: list[int]) -> Callable[[str], list[int]]:
        """Stand in for `fpcalc`, answering by which side of the pair asked."""

        def fake(locator: str) -> list[int]:
            return by_side["second" if "Studio 1/" in locator else "first"]

        return fake

    def test_an_unreadable_file_is_unchecked_rather_than_unrelated(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """`fpcalc` returning nothing must not read as a score of zero."""
        monkeypatch.setattr(dup, "fingerprint", self.stub(first=[], second=[]))
        found = examine(self.two(), workers=1)
        assert len(found.unchecked) == 1
        assert found.below == 0
        assert found.suspects == []

    def test_one_readable_side_is_still_unchecked(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """A comparison needs both, and half of one is not a weak match."""
        monkeypatch.setattr(dup, "fingerprint", self.stub(first=[1, 2, 3], second=[]))
        assert len(examine(self.two(), workers=1).unchecked) == 1

    def test_a_pair_under_the_floor_is_counted_not_dropped(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """It leaves the listing, not the accounting."""
        monkeypatch.setattr(dup, "fingerprint", self.stub(first=[0xFFFFFFFF], second=[0]))
        found = examine(self.two(), workers=1)
        assert found.below == 1
        assert found.suspects == []
        assert found.unchecked == []

    def test_a_matching_pair_becomes_a_suspect(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """And the ordinary path still works."""
        monkeypatch.setattr(dup, "fingerprint", self.stub(first=[1, 2, 3], second=[1, 2, 3]))
        found = examine(self.two(), workers=1)
        assert len(found.suspects) == 1
        assert found.suspects[0].score == 1.0


def test_an_unrelated_pair_sits_at_the_noise_floor() -> None:
    """Two fingerprints of different music agree on about half their bits.

    That is the whole reason `FLOOR` is 0.60 and not something nearer zero:
    chance agreement is 0.5 by construction, so a score has to be read against
    that floor rather than against perfection.
    """
    import random  # noqa: PLC0415

    from gmuse_tools.duplicates import agreement  # noqa: PLC0415

    rng = random.Random(7)  # noqa: S311 -- fixture noise, not a secret
    a = [rng.getrandbits(32) for _ in range(900)]
    b = [rng.getrandbits(32) for _ in range(900)]
    assert 0.45 < agreement(a, b) < 0.55, "unrelated audio should sit at chance"
    assert agreement(a, a) == 1.0


def test_the_packed_compare_agrees_with_the_word_by_word_one() -> None:
    """`packed` exists only to make the sweep affordable, not to change it.

    Ignoring the artist takes the pass from 2,603 pairs to 149,546, and a
    Python loop per pair is what made that unaffordable. The measure must come
    out the same.
    """
    import random  # noqa: PLC0415

    from gmuse_tools.duplicates import agreement, agrees, packed  # noqa: PLC0415

    rng = random.Random(11)  # noqa: S311 -- fixture noise, not a secret
    a = [rng.getrandbits(32) for _ in range(400)]
    # A copy with a tenth of its words disturbed — a re-encode, roughly.
    b = [w ^ (1 if rng.random() < 0.1 else 0) for w in a]
    slow = 1.0 - sum(((a[i] ^ b[i]) & 0xFFFFFFFF).bit_count() for i in range(400)) / (32.0 * 400)
    assert abs(agrees(packed(a), packed(b)) - slow) < 1e-12
    assert abs(agreement(a, b) - slow) < 1e-12


def test_the_shorter_fingerprint_bounds_the_comparison() -> None:
    """A pair where one side decoded only partly must not read as agreement.

    The span is the shorter of the two, so a one-word fingerprint compared
    against a full one is measured over one word — which is chance, not
    evidence. Nomination keeps the two within a second of each other, so this
    is a guard against a truncated decode rather than against a real edit.
    """
    from gmuse_tools.duplicates import agreement  # noqa: PLC0415

    assert agreement([], [1, 2, 3]) == 0.0
    assert agreement([0xFFFF_FFFF], [0xFFFF_FFFF, 0, 0, 0]) == 1.0
