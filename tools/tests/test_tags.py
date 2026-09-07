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

from gmuse_tools.tags import APE_MAGIC, FOOTER_LEN, ape_span, strip_ape_bytes

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
