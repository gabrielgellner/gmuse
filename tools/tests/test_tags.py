"""Finding an APE tag the parser refuses — issue #144.

`ape_of` returns None for a malformed tag, which is right for reading and was
wrong for removing: the block stays on disk and the file reads as already
clean. Of the 64 APE tags left in this library after a full pass, **every one
was unparseable** — and they are the files the pass exists for, since a single
bad item makes `lofty` abandon the whole file.

So these tests are about finding the tag by position and cutting it off by its
own length, without understanding a byte of its contents.
"""

from __future__ import annotations

from typing import TYPE_CHECKING

import pytest
from mutagen.id3 import ID3

from gmuse_tools.tags import (
    APE_MAGIC,
    FOOTER_LEN,
    KEYS_SHOWN,
    ape_span,
    extra_id3_span,
    id3_chain,
    id3_conflicts,
    id3_split,
    key_label,
    merge_stacked,
    plan,
    strip_ape_bytes,
    strip_extra_id3_bytes,
)

if TYPE_CHECKING:
    from pathlib import Path

#: Enough of an MP3 for mutagen to read a duration from. Two silent frames of
#: 128 kbps 44.1 kHz layer III, which is 417 bytes each.
FRAME = b"\xff\xfb\x90\x00" + b"\x00" * 413


def footer(*, size: int, is_header: bool, has_header: bool = True) -> bytes:
    """An APEv2 header or footer block. `size` excludes the header."""
    flags = (1 << 31 if has_header else 0) | (1 << 29 if is_header else 0)
    return (
        APE_MAGIC
        + (2000).to_bytes(4, "little")
        + size.to_bytes(4, "little")
        + (1).to_bytes(4, "little")
        + flags.to_bytes(4, "little")
        + b"\x00" * 8
    )


def tagged(tmp_path: Path, items: bytes, trailer: bytes = b"") -> Path:
    """An mp3 with a well-formed APE tag, and whatever else after it."""
    body = items + footer(size=len(items) + FOOTER_LEN, is_header=False)
    head = footer(size=len(items) + FOOTER_LEN, is_header=True)
    at = tmp_path / "t.mp3"
    at.write_bytes(FRAME * 40 + head + body + trailer)
    return at


def test_a_tag_at_the_very_end_is_found(tmp_path: Path) -> None:
    """The ordinary case: nothing after the tag."""
    at = tagged(tmp_path, b"MP3GAIN_MINMAX\x00086,193")
    span = ape_span(at)
    assert span is not None
    start, end = span
    assert end == at.stat().st_size
    assert at.read_bytes()[start : start + 8] == APE_MAGIC


def test_a_tag_followed_by_another_trailer_is_still_found(tmp_path: Path) -> None:
    """The reason the first attempt found nothing.

    It looked only at the last 32 and 160 bytes, on the assumption that an APE
    tag is last or sits before a 128-byte ID3v1 block. The files here end with
    a Lyrics3v2 tag, putting the APE header 374 bytes from the end.
    """
    at = tagged(tmp_path, b"MP3GAIN_MINMAX\x00086,193", trailer=b"0000037LYRICS200")
    span = ape_span(at)
    assert span is not None
    _, end = span
    assert end == at.stat().st_size - len(b"0000037LYRICS200")


def test_a_tag_running_past_the_end_is_cut_to_the_end(tmp_path: Path) -> None:
    """A tag whose declared length exceeds the file — one in this library.

    Its header claims 420 bytes with 98 present. Nothing can follow a tag that
    never ends, so everything from the header on is the tag.
    """
    at = tmp_path / "t.mp3"
    at.write_bytes(FRAME * 40 + footer(size=388, is_header=True) + b"MP3GAIN_MINMAX\x00086,193")
    span = ape_span(at)
    assert span is not None
    start, end = span
    assert end == at.stat().st_size
    assert at.read_bytes()[start : start + 8] == APE_MAGIC


def test_a_file_with_no_tag_has_no_span(tmp_path: Path) -> None:
    """A clean file is left alone, and says so."""
    at = tmp_path / "t.mp3"
    at.write_bytes(FRAME * 40)
    assert ape_span(at) is None
    assert strip_ape_bytes(at) is False


def test_stripping_removes_the_tag_and_leaves_the_audio(tmp_path: Path) -> None:
    """The whole point: the bytes before the tag are untouched.

    Checked here as an identity, and checked against the real instrument on the
    real files — gmuse's `AudioId` hashes the audio packets, and it was equal
    before and after on every one of the 65 stripped from this library.
    """
    at = tagged(tmp_path, b"MP3GAIN_MINMAX\x00086,193")
    audio = FRAME * 40
    assert at.stat().st_size > len(audio)

    assert strip_ape_bytes(at) is True
    assert at.read_bytes() == audio
    assert ape_span(at) is None
    assert strip_ape_bytes(at) is False, "a second pass has nothing to do"


def test_a_trailer_after_the_tag_survives_the_strip(tmp_path: Path) -> None:
    """Only the tag goes. What follows it is somebody else's business."""
    trailer = b"0000037LYRICS200"
    at = tagged(tmp_path, b"MP3GAIN_MINMAX\x00086,193", trailer=trailer)
    assert strip_ape_bytes(at) is True
    assert at.read_bytes() == FRAME * 40 + trailer


def test_a_cut_that_would_change_the_audio_is_refused(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """**Truncating a file is destructive**, so the audio is checked after.

    Forced here by making the span cover the audio too. The file must come back
    exactly as it was rather than being left half-cut.
    """
    at = tagged(tmp_path, b"MP3GAIN_MINMAX\x00086,193")
    was = at.read_bytes()
    monkeypatch.setattr("gmuse_tools.tags.ape_span", lambda _: (0, at.stat().st_size - 4))

    with pytest.raises(ValueError, match="refused"):
        strip_ape_bytes(at)
    assert at.read_bytes() == was, "a refused cut left the file damaged"


# --- A second ID3v2 tag stacked behind the first — issue #303 -----------------
#
# `plan` read the leading tag through mutagen and called the file "alone",
# which meant only "no APEv2, no ID3v1". A tag declares its own length, so
# another begins where it ends, and 39% of the staging tree has one. The two
# can name different MusicBrainz recordings, so this is an identity question
# rather than tidiness.


def synchsafe(n: int) -> bytes:
    """An ID3v2 size: four bytes, seven bits each."""
    return bytes([(n >> 21) & 0x7F, (n >> 14) & 0x7F, (n >> 7) & 0x7F, n & 0x7F])


def txxx(desc: str, value: str) -> bytes:
    """One ID3v2.4 TXXX frame, UTF-8."""
    payload = b"\x03" + desc.encode() + b"\x00" + value.encode()
    return b"TXXX" + synchsafe(len(payload)) + b"\x00\x00" + payload


def trck(value: str) -> bytes:
    """An ID3v2.4 TRCK frame."""
    payload = b"\x03" + value.encode()
    return b"TRCK" + synchsafe(len(payload)) + b"\x00\x00" + payload


def id3_tag(*frames: bytes) -> bytes:
    """A well-formed ID3v2.4 tag holding those frames."""
    body = b"".join(frames)
    return b"ID3" + b"\x04\x00" + b"\x00" + synchsafe(len(body)) + body


def stacked(tmp_path: Path, first: bytes, second: bytes) -> Path:
    """An mp3 carrying two ID3v2 tags, one behind the other."""
    tmp_path.mkdir(parents=True, exist_ok=True)
    at = tmp_path / "s.mp3"
    at.write_bytes(first + second + FRAME * 40)
    return at


def test_a_second_id3_tag_behind_the_first_is_found(tmp_path: Path) -> None:
    """The chain walk, which is the whole of #303.

    Mutagen reads the leading tag and stops, so the check that called these
    files "alone" could not see the second one.
    """
    at = stacked(
        tmp_path,
        id3_tag(txxx("MusicBrainz Album Id", "aaaaaaaa")),
        id3_tag(txxx("MusicBrainz Album Id", "bbbbbbbb")),
    )
    # **The instrument first, computed the other way.** Mutagen — what the old
    # check used — reads the leading tag and sees one TXXX, which is both why
    # this was invisible and proof the fixture really holds two tags: the
    # second frame exists in the bytes and not in what mutagen returns.
    assert len(ID3(at).getall("TXXX")) == 1, "mutagen should see only the leading tag"
    assert at.read_bytes().count(b"MusicBrainz Album Id") == 2, "the file really has two"

    chain = id3_chain(at)
    assert len(chain) == 2, "the stacked tag is the thing being looked for"
    assert chain[0][1] == 0
    assert chain[1][1] == chain[0][2], "the second begins where the first ends"


def test_one_tag_is_a_chain_of_one(tmp_path: Path) -> None:
    """The detector must be able to say no, or `alone` means nothing."""
    at = tmp_path / "one.mp3"
    at.write_bytes(id3_tag(txxx("MusicBrainz Album Id", "aaaaaaaa")) + FRAME * 40)
    assert len(id3_chain(at)) == 1
    assert extra_id3_span(at) is None


def test_only_a_disagreement_is_reported_as_a_conflict(tmp_path: Path) -> None:
    """Two tags repeating each other are untidy; two that differ are a question."""
    same = stacked(
        tmp_path / "a",
        id3_tag(txxx("MusicBrainz Album Id", "aaaaaaaa")),
        id3_tag(txxx("MusicBrainz Album Id", "aaaaaaaa")),
    )
    differ = stacked(
        tmp_path / "b",
        id3_tag(txxx("MusicBrainz Album Id", "aaaaaaaa")),
        id3_tag(txxx("MusicBrainz Album Id", "bbbbbbbb")),
    )
    assert id3_conflicts(same) == [], "agreement is not a conflict"
    assert id3_conflicts(differ) == ["TXXX:MusicBrainz Album Id"]


def test_stripping_removes_the_stacked_tag_and_leaves_the_audio(tmp_path: Path) -> None:
    """Cutting from the middle of a file, checked against what it is for."""
    first = id3_tag(txxx("MusicBrainz Album Id", "aaaaaaaa"))
    at = stacked(tmp_path, first, id3_tag(txxx("MusicBrainz Album Id", "bbbbbbbb")))
    audio_before = at.read_bytes()[-len(FRAME) * 40 :]

    assert strip_extra_id3_bytes(at) is True
    assert len(id3_chain(at)) == 1
    assert at.read_bytes() == first + audio_before, "the leading tag and the audio, and nothing else"
    assert strip_extra_id3_bytes(at) is False, "nothing left to do the second time"


def test_a_conflict_is_reported_but_not_marked_to_apply(tmp_path: Path) -> None:
    """Which tag is right is a judgement — `plan` declines to make it."""
    differ = stacked(
        tmp_path / "b",
        id3_tag(txxx("MusicBrainz Album Id", "aaaaaaaa")),
        id3_tag(txxx("MusicBrainz Album Id", "bbbbbbbb")),
    )
    row = plan(differ)
    assert row is not None, "a stacked file is no longer 'already modern and alone'"
    assert row["drop_extra_id3"] == "1"
    assert row["id3_conflict"] == "TXXX:MusicBrainz Album Id"
    assert row["apply"] == "", "a disagreement waits for a person"


def test_stacked_tags_that_agree_go_through_as_usual(tmp_path: Path) -> None:
    """The instrument for the test above: without a conflict, `apply` is set."""
    same = stacked(
        tmp_path / "a",
        id3_tag(txxx("MusicBrainz Album Id", "aaaaaaaa")),
        id3_tag(txxx("MusicBrainz Album Id", "aaaaaaaa")),
    )
    row = plan(same)
    assert row is not None
    assert row["drop_extra_id3"] == "1"
    assert row["apply"] == "y", "redundant is not ambiguous"


def test_what_only_the_stacked_tag_holds_is_carried_rather_than_lost(tmp_path: Path) -> None:
    """The case the first cut of this fix got backwards, and would have destroyed.

    On this staging tree the *leading* tag is the poor one — a Windows Media /
    AMG tag with no MusicBrainz frames and no art — and every id sits in the
    54 KB Picard tag behind it. Nothing disagrees, so a rule that only looked
    for disagreement called the file safe to strip and would have deleted the
    better record on 93 of 93 files.

    Nothing is in question here, so nothing waits for a person: it is only in
    the wrong place, and moves. The same shape as the lyrics migration above.
    """
    at = stacked(
        tmp_path / "silent",
        id3_tag(txxx("SCRIPT", "Latn")),
        id3_tag(txxx("MusicBrainz Album Id", "bbbbbbbb")),
    )
    # The instrument: the leading tag really is silent on identity, read from
    # the frames rather than from the answer under test.
    assert [f.desc for f in ID3(at).getall("TXXX")] == ["SCRIPT"]

    split = id3_split(at)
    assert split.conflicts == [], "a field only one tag holds contradicts nothing"
    assert sorted(split.salvage) == ["TXXX:MusicBrainz Album Id"], "and is carried, not dropped"
    assert plan(at)["apply"] == "y", "nothing here needs a judgement"


def test_merging_keeps_the_frame_the_stacked_tag_held(tmp_path: Path) -> None:
    """End to end: after the merge the id is in the one remaining tag."""
    at = stacked(
        tmp_path / "merge",
        id3_tag(txxx("SCRIPT", "Latn")),
        id3_tag(txxx("MusicBrainz Album Id", "bbbbbbbb")),
    )
    assert len(ID3(at).getall("TXXX")) == 1, "before: the id is not in the leading tag"

    stripped, carried = merge_stacked(at)
    assert (stripped, carried) == (True, 1)
    assert len(id3_chain(at)) == 1, "one tag now"
    kept = {f.desc: str(f) for f in ID3(at).getall("TXXX")}
    assert kept == {"SCRIPT": "Latn", "MusicBrainz Album Id": "bbbbbbbb"}, "nothing was lost"


def test_a_conflicting_frame_is_not_carried_over_the_leading_one(tmp_path: Path) -> None:
    """Salvage fills gaps; it never overwrites. The conflict is what stops it."""
    at = stacked(
        tmp_path / "differ2",
        id3_tag(txxx("MusicBrainz Album Id", "aaaaaaaa")),
        id3_tag(txxx("MusicBrainz Album Id", "bbbbbbbb")),
    )
    split = id3_split(at)
    assert split.conflicts == ["TXXX:MusicBrainz Album Id"]
    assert split.salvage == {}, "a contested field is not quietly merged either way"


def test_a_frame_name_holding_raw_bytes_cannot_break_the_review() -> None:
    """A PRIV frame's owner is binary, tabs and newlines included.

    Writing one into the review file ended a real survey with `holds a tab or
    newline`: the column describing the file broke the format it was describing
    it in.
    """
    assert "\t" not in key_label(["PRIV:WM/X\tY\nZ"])
    assert "\n" not in key_label(["PRIV:WM/X\tY\nZ"])
    # The instrument: an ordinary name is left alone, so the cleaning is doing
    # something to the binary one rather than flattening everything.
    assert key_label(["TXXX:MusicBrainz Album Id"]) == "TXXX:MusicBrainz Album Id"


def test_a_long_list_of_frames_is_summarised() -> None:
    """A cell exists to be read; a hundred frame names is not readable."""
    many = key_label([f"TXXX:Field {n:02d}" for n in range(30)])
    assert many.endswith("and 22 more")
    assert many.count(";") == KEYS_SHOWN, "eight named, then the count"


def test_a_track_number_written_two_ways_is_not_a_conflict(tmp_path: Path) -> None:
    """`1` and `1/10` are one track number — paths.py's lesson, paid for again.

    Every one of 93 stacked files on one album reported a `TRCK` conflict and
    not one was a disagreement. Blocking on those would have left the pass
    unable to touch anything while burying the case that mattered.
    """
    at = stacked(
        tmp_path / "trck",
        id3_tag(trck("1"), txxx("MusicBrainz Album Id", "aaaaaaaa")),
        id3_tag(trck("1/10"), txxx("MusicBrainz Album Id", "aaaaaaaa")),
    )
    split = id3_split(at)
    assert split.conflicts == [], "a track number does not decide what a recording is"
    # The instrument: it is seen and reported, just not as a thing that stops
    # the pass — so silence above is a judgement, not a blind spot.
    assert split.differs == ["TRCK"]
    assert plan(at)["apply"] == "y"


def test_an_identity_disagreement_still_stops_the_pass(tmp_path: Path) -> None:
    """The instrument for the test above: the dangerous one is still caught."""
    at = stacked(
        tmp_path / "both",
        id3_tag(trck("1"), txxx("MusicBrainz Album Id", "aaaaaaaa")),
        id3_tag(trck("1/10"), txxx("MusicBrainz Album Id", "bbbbbbbb")),
    )
    split = id3_split(at)
    assert split.conflicts == ["TXXX:MusicBrainz Album Id"]
    assert split.differs == ["TRCK"], "and the harmless one is still merely noted"
    assert plan(at)["apply"] == ""


# --- An ID3v1 stump in front of the full value — issue #315 --------------------
#
# The merge keeps the leading tag wherever both hold a frame, and on this
# material the leading tag is where an ID3v1 round trip left its mark. Three
# imports in one day would have written the stump into the one remaining tag
# and deleted the full value with the stacked one.


def text(frame_id: str, value: str) -> bytes:
    """One ID3v2.4 text frame — TIT2, TALB and the like."""
    payload = b"\x03" + value.encode()
    return frame_id.encode() + synchsafe(len(payload)) + b"\x00\x00" + payload


FULL = "Flushed From the Bathroom of Your Heart"
STUMP = FULL[:30]


def test_a_stump_in_the_leading_tag_gives_way_to_the_full_value(tmp_path: Path) -> None:
    """At Folsom Prison #11, as it arrived."""
    at = stacked(tmp_path / "stump", id3_tag(text("TIT2", STUMP)), id3_tag(text("TIT2", FULL)))
    # The instrument, read from the bytes rather than from the code under test:
    # the leading tag really is the stump, and the full value is really there.
    assert str(ID3(at)["TIT2"]) == STUMP
    assert len(STUMP) == 30, "the ID3v1 field width, which is what makes it a stump"
    assert FULL.encode() in at.read_bytes()

    merge_stacked(at)
    assert len(id3_chain(at)) == 1
    assert ID3(at)["TIT2"].text == [FULL], "one value, the whole one — not both, not the stump"


def test_a_stump_is_reported_as_restored_not_as_a_difference(tmp_path: Path) -> None:
    """What the survey says is what apply will do."""
    at = stacked(tmp_path / "say", id3_tag(text("TALB", STUMP)), id3_tag(text("TALB", FULL)))
    split = id3_split(at)
    assert sorted(split.restore) == ["TALB"]
    assert split.differs == [], "it no longer claims the leading one is kept"
    assert plan(at)["restore_id3"] == "TALB"


def test_a_longer_title_that_is_not_a_stump_is_still_a_difference(tmp_path: Path) -> None:
    """`Hurt` against `Hurt (Live)` is a prefix and a different recording.

    Preferring the longer value there changes which version the file claims,
    which is the version-qualifier trap. Length is what makes a truncation.
    """
    at = stacked(tmp_path / "live", id3_tag(text("TIT2", "Hurt")), id3_tag(text("TIT2", "Hurt (Live)")))
    split = id3_split(at)
    assert split.restore == {}
    assert split.differs == ["TIT2"]
    merge_stacked(at)
    assert ID3(at)["TIT2"].text == ["Hurt"], "the leading tag is kept, as before"


def test_a_29_character_value_continued_by_the_stacked_tag_is_a_stump(tmp_path: Path) -> None:
    """ID3v1 pads to 30 and readers strip the trailing blank — #302's worked case."""
    full = "(I Got That) Boom Boom (feat. Ying Yang Twins)"
    stump = full[:30].rstrip()
    assert len(stump) == 29, "the fixture has to be the 29 case"
    at = stacked(tmp_path / "29", id3_tag(text("TIT2", stump)), id3_tag(text("TIT2", full)))
    merge_stacked(at)
    assert ID3(at)["TIT2"].text == [full]
