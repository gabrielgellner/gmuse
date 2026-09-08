"""The matching rules, every one of which has been wrong at least once.

Each case here is a real failure: nine good matches downgraded over a hyphen,
a recording matching to the second discarded for scoring 80 on its title, a
MusicBrainz length of zero read as a zero-second song, and an ID3 frame name
that made every artist come back empty.

All were found by hand and none of that survived the next edit, which is what
this file is for. The Rust-side matchers — Beatport and video ids, the primary
artist of a joined credit — are tested in `src/model/ingest.rs`.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from gmuse_tools.identify import (
    SAME_MIX,
    SAME_RECORDING,
    SIBLING_SPAN,
    best,
    first_artist,
    has_recording_id,
    normalise,
    searchable_title,
    unasked_qualifier,
    variance,
    verdict,
    work_list,
)


class TestQuery:
    def test_only_the_first_artist_is_queried(self) -> None:
        """`artist:` scores a joined credit at zero; the first name scores 100."""
        assert first_artist("Stephan Bodzin & Marc Romboy") == "Stephan Bodzin"
        assert first_artist("ANNA,NERVO") == "ANNA"

    def test_beatports_not_a_remix_suffix_is_stripped(self) -> None:
        """`(Original Mix)` is Beatport for "not a remix", not a MusicBrainz term."""
        assert searchable_title("Distant Trip (Original Mix)") == "Distant Trip"

    def test_a_real_remix_name_survives(self) -> None:
        """`(Nolah Remix)` names a different recording and must not be lost."""
        assert searchable_title("Modulo Tierra (Nolah Remix)") == "Modulo Tierra (Nolah Remix)"


class TestUnaskedQualifier:
    def test_a_qualifier_we_did_not_ask_for_is_found(self) -> None:
        """A vocal and its instrumental are the same length by construction."""
        assert unasked_qualifier("212 (Original Mix)", "212 (Instrumental)") is not None
        assert unasked_qualifier("Years", "Years (Live)") is not None

    @pytest.mark.parametrize(
        ("ours", "theirs"),
        [
            # The first version of this compared bracketed text literally and
            # downgraded nine good matches out of eleven.
            ("Phobos (Pan-Pot Remix)", "Phobos (Pan‐Pot remix)"),
            ("Me Roar (Format:B Remix)", "Me Roar (Format: B remix)"),
            ("Alors on danse Radio Edit", "Alors on danse (radio edit)"),
        ],
    )
    def test_the_same_qualifier_spelled_differently_is_not_unasked(self, ours: str, theirs: str) -> None:
        """One U+2010, one space, or no brackets at all — still the same word."""
        assert unasked_qualifier(ours, theirs) is None

    def test_a_remixers_name_is_not_a_qualifier(self) -> None:
        """`(Pan-Pot remix)` names who, not what."""
        assert unasked_qualifier("Phobos", "Phobos (Pan-Pot remix)") is None


class TestVariance:
    """Which kind of not-a-match, when a qualifier says it is not one.

    Syntactically `(Instrumental)` and `(Radio Edit)` are one shape, so the
    length is the only thing that separates a qualifier from a derivation —
    which is why the two tolerances must not be one number.
    """

    def test_the_same_length_is_mix_level_variance(self) -> None:
        """Muting a channel does not change a length."""
        assert variance(200.0, 200_500) == "qualifier"
        assert variance(200.0, int((200 + SAME_MIX - 0.1) * 1000)) == "qualifier"

    def test_a_materially_different_length_is_a_derivation(self) -> None:
        """The recording was cut or rebuilt — a different fact, another home."""
        assert variance(400.0, 220_000) == "derivation"

    def test_far_enough_apart_is_neither(self) -> None:
        """What this excludes is a DJ mix that happens to contain the track."""
        assert variance(200.0, int(200 * (SIBLING_SPAN + 1) * 1000)) == "unrelated"

    @pytest.mark.parametrize("theirs", [None, 0, "nope"])
    def test_no_length_recorded_is_not_a_zero_second_recording(self, theirs: object) -> None:
        """A zero is "none recorded", not a zero-second song.

        Seven candidate lists carry one, and comparing against it made a real
        recording look wrong by its whole duration.
        """
        assert variance(200.0, theirs) == "unknown"

    def test_the_tolerances_are_ordered_and_stay_ordered(self) -> None:
        """`SAME_MIX` must stay tighter than `SAME_RECORDING`.

        Widen it and a radio edit starts reading as an instrumental.
        """
        assert SAME_MIX < SAME_RECORDING
        assert SIBLING_SPAN > 1.0


class TestBest:
    """Which candidate to believe, when MusicBrainz offers several."""

    @staticmethod
    def cand(score: int, length_ms: int | None, title: str = "x") -> dict[str, object]:
        """One candidate as MusicBrainz returns it; a missing length is absent."""
        c: dict[str, object] = {"score": score, "title": title}
        if length_ms is not None:
            c["length"] = length_ms
        return c

    def test_a_length_that_agrees_outranks_a_higher_score(self) -> None:
        """A length that agrees is worth more than a name that scores well.

        `Triton`: ours 463.9s, MB offered 222.0s at 100 and 465.0s at 80.
        Filtering to 95-and-above first discarded the right answer.
        """
        chosen = best([self.cand(100, 222_000), self.cand(80, 465_000)], 463.9)
        assert chosen is not None
        assert chosen["length"] == 465_000

    def test_a_zero_length_is_not_a_zero_second_recording(self) -> None:
        """MusicBrainz records no length for some recordings.

        Comparing against it made a real recording look wrong by its whole
        duration.
        """
        chosen = best([self.cand(100, 0), self.cand(100, 200_000)], 200.0)
        assert chosen is not None
        assert chosen["length"] == 200_000

    def test_a_name_that_is_not_really_a_match_is_not_rescued_by_its_length(self) -> None:
        """A floor on the name score bounds what length reasoning can reach.

        Without it, any candidate of the right duration wins — including one
        whose title is not a match at all.
        """
        chosen = best([self.cand(96, 300_000), self.cand(20, 200_000)], 200.0)
        assert chosen is not None
        assert chosen["score"] == 96

    def test_with_no_duration_the_ranking_is_all_there_is(self) -> None:
        """An unreadable file still gets the match its name earns."""
        chosen = best([self.cand(100, 222_000), self.cand(80, 465_000)], None)
        assert chosen is not None
        assert chosen["score"] == 100


class TestVerdict:
    def test_a_length_that_agrees_makes_it_matched(self) -> None:
        """Agreement to a tenth of a second is worth more than the name score."""
        confidence, _ = verdict({"score": 84, "length": 199_300}, 199.3)
        assert confidence == "matched"

    def test_no_length_and_a_weak_name_is_not_probable(self) -> None:
        """`probable` means the name is strong and nothing contradicts it."""
        confidence, why = verdict({"score": 80, "length": 0}, 200.0)
        assert confidence == "uncertain"
        assert "no duration" in why


class TestTagReading:
    def test_the_id3_frame_for_a_recording_id_is_recognised(self) -> None:
        """Two frames, one of which is a trap.

        Ffprobe cannot see `UFID` at all, so through it a fully tagged file
        looks unidentified. And `musicbrainz release track id` sits beside the
        real one on every Picard-tagged file — matching it would key the
        library by a per-release id that changes when the album does.
        """
        assert has_recording_id({"ufid:http://musicbrainz.org": "…"})
        assert has_recording_id({"musicbrainz_trackid": "…"})
        assert not has_recording_id({"musicbrainz release track id": "…"})
        assert not has_recording_id({"title": "x"})

    def test_id3_frame_names_are_normalised_to_plain_ones(self) -> None:
        """Mutagen names ID3 by raw frame id.

        So `tags["artist"]` came back empty for every MP3, and the pass
        reported that MusicBrainz had none of the library.
        """
        assert normalise({"tpe1": "Burial"})["artist"] == "Burial"
        assert normalise({"tit2": "Archangel"})["title"] == "Archangel"

    def test_a_plain_name_already_present_is_not_overwritten(self) -> None:
        """Vorbis and ID3 names can both appear; the plain one is already right."""
        assert normalise({"artist": "Vorbis", "tpe1": "ID3"})["artist"] == "Vorbis"


class TestWorkList:
    """Reading what gmuse hands over, rather than walking a disk.

    This pass emitted `{"locator": ...}` for 712 confident matches and none of
    them could be applied: a path names nothing the library knows about.
    """

    def write(self, tmp_path: Path, *lines: str) -> Path:
        """A work list on disk, as `gmuse library query` would leave one."""
        p = tmp_path / "work.jsonl"
        p.write_text("\n".join(lines) + "\n", encoding="utf-8")
        return p

    def test_a_row_keeps_both_the_id_and_the_path(self, tmp_path: Path) -> None:
        """The two have different lifetimes and both are needed."""
        rows = work_list(
            self.write(
                tmp_path,
                '{"recording":"r1","encoding":"e1","locator":"/m/a.mp3","title":"A","artist":"B"}',
            )
        )
        assert len(rows) == 1
        assert rows[0].encoding == "e1"
        assert rows[0].locator == "/m/a.mp3"

    def test_a_row_this_machine_cannot_open_is_left_out(self, tmp_path: Path) -> None:
        """An ordinary row, and nothing this pass can act on.

        The library knows about it and this disk does not hold it, so there is
        no audio to read and no locator to open.
        """
        rows = work_list(
            self.write(
                tmp_path,
                '{"recording":"r1","encoding":null,"locator":null,"title":"On the NAS","artist":"B"}',
            )
        )
        assert rows == []

    def test_blank_lines_and_comments_are_skipped(self, tmp_path: Path) -> None:
        """A work list is a file somebody may have been editing."""
        rows = work_list(
            self.write(
                tmp_path,
                "# a note",
                "",
                '{"recording":"r1","encoding":"e1","locator":"/m/a.mp3","title":"A","artist":"B"}',
            )
        )
        assert len(rows) == 1


def test_a_remix_is_never_the_top_hit_and_the_window_must_hold_it() -> None:
    """The window was three, and three loses every remix.

    MusicBrainz ranks by name, and a remix cannot outrank the original it is a
    remix *of* — the titles differ only by a suffix, so the plain track sorts
    first. `How Many Times (Andhim Remix)` returned the 3:39 original twice
    before any remix, and the four candidates matching our 8:15 file to the
    second sat at ranks 5, 7, 8 and 10.

    Widening is safe because `best` selects on duration, not on the ranking:
    more candidates give that choice more to work with and do not loosen it.
    """
    from gmuse_tools.identify import CANDIDATES, best  # noqa: PLC0415

    ours = 495.4
    # The shape MusicBrainz actually returned, in the order it returned it.
    ranked = [
        {"id": "orig-a", "score": 100, "length": 219_000, "title": "How Many Times"},
        {"id": "orig-b", "score": 100, "length": 218_000, "title": "How Many Times"},
        {"id": "rmx-6", "score": 88, "length": 377_000, "title": "How Many Times (andhim remix)"},
        {"id": "rmx-6b", "score": 88, "length": 382_000, "title": "How Many Times (Andhim remix)"},
        {"id": "rmx-8", "score": 88, "length": 495_000, "title": "How Many Times (Andhim remix)"},
    ]
    assert len(ranked) <= CANDIDATES, "the window is narrower than the case that motivated it"
    assert best(ranked, ours)["id"] == "rmx-8", "the 8:15 remix is the one our file is"
    assert best(ranked[:3], ours)["id"] != "rmx-8", (
        "with the old window of three the right answer is not even a candidate"
    )


def test_a_remix_asks_about_the_original_artist() -> None:
    """The swap: MusicBrainz credits the original, the DJ tag credits the remixer.

    `Andhim — How Many Times (Andhim Remix)` asked about Andhim returns 8
    candidates; asked about Elderbrook it returns 16, including the one whose
    own title names the remixer. And the suffix leaves the query, because
    `recording:"Slip Away (Olivier Giacomotto Remix)"` matches nothing at all.
    """
    from gmuse_tools.identify import asked_about  # noqa: PLC0415

    artist, title, who = asked_about({"artist": "Andhim,Elderbrook", "title": "How Many Times (Andhim Remix)"}, "stem")
    assert (artist, title, who) == ("Elderbrook", "How Many Times", "Andhim")


def test_a_remixer_who_is_the_only_name_is_kept() -> None:
    """A query naming nobody returns 25 unrelated candidates and no duration match.

    Asking imprecisely beats asking about nothing, and the corroboration below
    still has to be satisfied either way.
    """
    from gmuse_tools.identify import asked_about  # noqa: PLC0415

    artist, title, who = asked_about({"artist": "Andhim", "title": "How Many Times (Andhim Remix)"}, "stem")
    assert (artist, title, who) == ("Andhim", "How Many Times", "Andhim")


def test_an_original_mix_names_no_remixer() -> None:
    """`Original` is Beatport saying *not* a remix, and it is not a person."""
    from gmuse_tools.identify import asked_about, remixer_in  # noqa: PLC0415

    assert remixer_in("Cream (Original Mix)") is None
    artist, title, who = asked_about({"artist": "Danny Avila,Tujamo", "title": "Cream (Original Mix)"}, "stem")
    assert (artist, title, who) == ("Danny Avila", "Cream", None)


def test_two_remixes_of_one_track_are_the_same_length_and_the_title_decides() -> None:
    """The `Larse Vox` trap, which is why duration cannot be the corroboration.

    `Slip Away (Larse Vox Remix)` is 518s and our Giacomotto remix is 517s. A
    rule taking the closest duration files one as the other.
    """
    from gmuse_tools.identify import best, contested  # noqa: PLC0415

    ours = 517.0
    larse = {"id": "larse", "score": 84, "length": 518_000, "title": "Slip Away (Larse Vox Remix)"}
    giac = {"id": "giac", "score": 84, "length": 519_000, "title": "Slip Away (Olivier Giacomotto Remix)"}

    # Named, so it wins even though Larse is nearer in length.
    assert best([larse, giac], ours, "Olivier Giacomotto")["id"] == "giac"
    # Nobody claims it, and two fit: the length has decided nothing.
    assert contested(
        [larse, {"id": "x", "score": 84, "length": 516_000, "title": "Slip Away"}], ours, "Olivier Giacomotto"
    )
    # One candidate is not a contest — there is nothing to confuse it with.
    assert not contested([larse], ours, "Olivier Giacomotto")
    # And with no remixer to look for, this rule says nothing at all.
    assert not contested([larse, giac], ours, None)


def test_a_corroborated_remixer_is_credited_and_an_uncertain_one_is_not() -> None:
    """What else a confident match established — #162.

    The identification says which recording this is. That the candidate's own
    title names the remixer our title named is evidence no tag provides on its
    own, and it was being discarded at the moment it was known.

    Nothing is emitted for a row a person still has to read: an uncertain
    identification is not evidence of a remix relationship.
    """
    from gmuse_tools.identify import Work, also  # noqa: PLC0415

    row = Work(recording="rec-1", encoding="enc-1", locator="/m/a.mp3", title="t", artist="a")
    named = {"id": "mb-1", "title": "How Many Times (Andhim remix)"}
    plain_title = {"id": "mb-2", "title": "How Many Times"}

    # `contributed`, not `credited`: the latter is the split, which refuses a
    # single name and asserts `performer` — a role a remixer must not have.
    assert also(row, named, "Andhim", "matched") == [
        {
            "fact": "contributed",
            "recording": "rec-1",
            "artist": "Andhim",
            "role": "remixer",
            "basis": "musicbrainz",
        }
    ]
    assert also(row, plain_title, "Andhim", "matched") == [], "nothing corroborated the remixer"
    assert also(row, named, "Andhim", "uncertain") == [], "a row awaiting a person establishes nothing"
    assert also(row, named, None, "matched") == [], "no remixer in our title, nothing to credit"


def test_the_version_qualifier_is_not_emitted_here() -> None:
    """`Instance::ingest` already reads it off the title and asserts it.

    Adding `original` to that vocabulary is the whole of what #162 needed on
    that side; emitting it again from this pass would be a second answer to one
    question, at a different rung.
    """
    from gmuse_tools.identify import Work, also  # noqa: PLC0415

    row = Work(recording="rec-1", encoding="enc-1", locator="/m/a.mp3", title="t", artist="a")
    lines = also(row, {"id": "mb-1", "title": "Cream"}, None, "matched")
    assert not any(line.get("fact") == "qualifier_assigned" for line in lines)
