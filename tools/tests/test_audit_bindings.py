"""Does each binding name a recording of the right length — #307.

The claims here are mostly absences — "no binding disagrees", "nothing was
counted as agreeing that was never compared" — and an absence is asserted by
showing a set is empty. So each test carries the second assertion `CLAUDE.md`
asks for: one computed some other way, which proves the comparison happened.
"""

from __future__ import annotations

import pytest

from gmuse_tools import audit_bindings as ab
from gmuse_tools import bronze
from gmuse_tools.audit_bindings import (
    ABSENT,
    MISSING,
    NO_LOCAL_LENGTH,
    NO_MB_LENGTH,
    NOT_FOUND,
    UNANSWERED,
    Copy,
    compare,
    copies_from,
)


def copy(rec: str, ours: float | None, *, enc: str | None = None, release: str | None = None) -> Copy:
    return Copy(
        encoding=enc or f"enc-{rec}",
        recording=rec,
        release=release,
        ours_s=ours,
        locator=f"/m/{rec}.mp3",
        title=rec,
        artist="Someone",
    )


def test_a_length_that_matches_agrees() -> None:
    """The half that must not report: 237.2s bound to a 237.0s recording."""
    found = compare([copy("wild-roses", 237.2)], {"wild-roses": 237.0})
    assert found.compared == 1, "nothing was compared, so agreeing proves nothing"
    assert [c.recording for c in found.agree] == ["wild-roses"]
    assert found.disagree == []


def test_a_length_that_does_not_match_is_reported() -> None:
    """The half that must report — **off a different fixture than the one above**.

    #307's own words: a pass that reports everything scores the same as a
    correct one if both halves are asserted over one fixture. This is the
    measured case from the Kylie brief: `Where the Wild Roses Grow` is 237.2s
    and was bound to a 389.0s recording.
    """
    found = compare([copy("wrong-roses", 237.2)], {"wrong-roses": 389.0})
    assert found.compared == 1, "nothing was compared, so the finding proves nothing"
    assert found.agree == []
    assert len(found.disagree) == 1
    assert found.disagree[0].delta == pytest.approx(237.2 - 389.0)


def test_the_tolerance_is_a_boundary_both_ways() -> None:
    """Just inside agrees, just outside does not, and sign does not matter."""
    lengths = {"a": 200.0, "b": 200.0, "c": 200.0, "d": 200.0}
    found = compare(
        [copy("a", 209.9), copy("b", 190.1), copy("c", 210.1), copy("d", 189.9)],
        lengths,
        tolerance=10.0,
    )
    assert found.compared == 4
    assert sorted(c.recording for c in found.agree) == ["a", "b"]
    assert sorted(d.copy.recording for d in found.disagree) == ["c", "d"]


def test_nothing_that_was_not_compared_is_counted_as_agreeing() -> None:
    """The failure an absence-report is built to make — #298's shape.

    Every one of these copies has a real length and a real recording id, so a
    pass that treated "no answer" as "fine" would count four agreements. None of
    them was compared, and each is not compared for a *different* reason, which
    has to survive into the report as a different sentence.
    """
    copies = [
        copy("no-local", None),
        copy("no-mb-length", 200.0),
        copy("not-found", 200.0),
        copy("never-asked", 200.0),
    ]
    found = compare(copies, {"no-mb-length": ABSENT, "not-found": MISSING})

    # The instrument: four copies went in, and every one is accounted for.
    # Computed off the unchecked buckets rather than off `compared`, so it can
    # fail while the claim below passes and the other way round.
    accounted = sum(len(v) for v in found.unchecked.values()) + found.compared
    assert accounted == len(copies), "a copy went in and was never sorted anywhere"

    assert found.compared == 0
    assert found.agree == [], "a copy that was never compared was counted as agreeing"
    assert {k: [c.recording for c in v] for k, v in found.unchecked.items()} == {
        NO_LOCAL_LENGTH: ["no-local"],
        NO_MB_LENGTH: ["no-mb-length"],
        NOT_FOUND: ["not-found"],
        UNANSWERED: ["never-asked"],
    }


def test_the_worst_disagreement_is_listed_first() -> None:
    """Worst by magnitude, whichever way it runs."""
    found = compare(
        [copy("small", 215.0), copy("huge-short", 410.8), copy("huge-long", 530.2)],
        {"small": 200.0, "huge-short": 627.0, "huge-long": 217.8},
    )
    assert [d.copy.recording for d in found.disagree] == ["huge-long", "huge-short", "small"]


def test_a_row_with_no_recording_is_out_of_scope_not_unchecked() -> None:
    """Bytes nothing has named have no length to compare against.

    Counting them as "not compared" would make every library with unidentified
    music look like a failed pass, so they never become a `Copy` at all.
    """
    rows = [
        {"musicbrainz": "rec-1", "encoding": "e1", "length_ms": 200_000},
        {"musicbrainz": None, "encoding": "e2", "length_ms": 200_000},
        {"musicbrainz": "rec-3", "encoding": None, "length_ms": 200_000},
    ]
    got = copies_from(rows)
    assert len(rows) == 3, "the fixture is not the one this claim is about"
    assert [c.recording for c in got] == ["rec-1"]
    assert got[0].ours_s == pytest.approx(200.0)


def test_a_recording_on_a_release_is_not_asked_about_again(monkeypatch: pytest.MonkeyPatch) -> None:
    """Release first, because `ingest` already asked — and only fall back.

    On this library 516 of 526 releases were already in the cache from
    `ingest`, against 7,218 recordings: asking per recording would be about two
    hours at one request a second. So a recording the release covers must never
    be looked up on its own.
    """
    asked_urls: list[str] = []

    def fake_asked(url: str, params: dict[str, str]) -> dict[str, object] | None:
        asked_urls.append(url)
        if url.endswith("/release/rel-1"):
            assert params.get("inc") == ab.INC, "not ingest's parameters, so the cache key will miss"
            return {"media": [{"tracks": [{"recording": {"id": "on-release", "length": 237_000}}]}]}
        if url.endswith("/recording/loose"):
            return {"length": 180_000}
        return None

    monkeypatch.setattr(ab, "asked", fake_asked)
    copies = [copy("on-release", 237.0, release="rel-1"), copy("loose", 180.0)]
    lengths = ab.lengths_of(copies)

    # The instrument: the fake was actually consulted, both ways.
    assert any(u.endswith("/release/rel-1") for u in asked_urls), "the release was never asked"
    assert any(u.endswith("/recording/loose") for u in asked_urls), "the fallback never ran"

    assert lengths == {"on-release": 237.0, "loose": 180.0}
    assert not any(u.endswith("/recording/on-release") for u in asked_urls), (
        "a recording the release already covered was asked about again"
    )


def test_an_unanswered_lookup_stays_unanswered(monkeypatch: pytest.MonkeyPatch) -> None:
    """`bronze` distinguishes "no" from "did not answer" — #262 — and so must this.

    A run of 503s must not become "MusicBrainz does not know this recording",
    and must certainly not become an agreement.
    """

    def unanswered(url: str, params: dict[str, str]) -> None:
        raise bronze.UnansweredError(url)

    monkeypatch.setattr(ab, "asked", unanswered)
    copies = [copy("flaky", 200.0)]
    found = compare(copies, ab.lengths_of(copies))
    assert found.compared == 0
    assert [c.recording for c in found.unchecked[UNANSWERED]] == ["flaky"]
    assert NOT_FOUND not in found.unchecked, "an unanswered lookup was reported as a missing recording"


def _verified(ours: float, theirs: float, real: float | None) -> str:
    """One accused copy, measured by a fake, and the kind it came out as."""
    found = compare([copy("x", ours)], {"x": theirs})
    assert len(found.disagree) == 1, "the fixture did not disagree, so nothing was accused"
    measured: list[str] = []

    def fake(locator: str) -> float | None:
        measured.append(locator)
        return real

    ab.verify(found, measure=fake)
    assert measured, "verify never measured the file, so the kind below is a default"
    return found.disagree[0].kind


def test_a_file_that_matches_musicbrainz_clears_the_binding() -> None:
    """The case that made up a third of the first report — #307.

    *Neon Bible* was recorded at 1,882s, decodes to 310.3s, and MusicBrainz says
    310.0s. The binding is right and the library's length is wrong, and
    reporting it as a wrong binding sends a person to relink something that is
    correct.
    """
    assert _verified(ours=1882.0, theirs=310.0, real=310.3) == ab.LENGTH_WRONG


def test_a_file_that_matches_the_library_keeps_the_accusation() -> None:
    """The case this audit exists for: *Sad-Eyed Lady of the Lowlands* runs 11:20
    and is bound to a 142.8s recording. The file agrees with the library, so the
    binding is what is wrong.
    """
    assert _verified(ours=680.0, theirs=142.8, real=680.0) == ab.BINDING_WRONG


def test_a_file_that_barely_decodes_is_broken() -> None:
    """`The Girl in the Corner` is 60 KB and decodes to 0.18s."""
    assert _verified(ours=0.3, theirs=357.5, real=0.18) == ab.BROKEN
    assert _verified(ours=200.0, theirs=300.0, real=None) == ab.BROKEN


def test_a_file_matching_neither_is_said_so() -> None:
    """*No Cars Go*: recorded 1,192s, decodes to 237.1s, bound to 343.0s."""
    assert _verified(ours=1192.0, theirs=343.0, real=237.1) == ab.BOTH_OFF


def test_a_copy_not_on_this_device_is_left_as_accused() -> None:
    """Nothing to decode, so the accusation stands as made — and says it was not re-measured."""
    far = Copy("e", "far", None, 680.0, None, "t", "a")
    found = compare([far], {"far": 142.8})
    ab.verify(found, measure=lambda _: pytest.fail("measured a file that is not here"))
    assert found.disagree[0].kind == ab.BINDING_WRONG
    assert found.disagree[0].decoded_s is None
