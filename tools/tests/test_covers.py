"""Getting the front cover out of the tags without filing the wrong picture.

The library had the art all along — 965 of 1055 album folders carried it in
every track and 46 had it on disk. So the pass is mostly about *which* picture
and *where*, not about finding one: this library's files also hold 494 `FISH`
and 88 `PUBLISHER_LOGOTYPE` pictures, and a label logo written as `cover.jpg`
is worse than no cover, because nothing will ever flag it.

So most of these tests are about refusing.
"""

from __future__ import annotations

import struct
from typing import TYPE_CHECKING

from mutagen.id3 import APIC, ID3, TIT2, PictureType

from gmuse_tools.covers import (
    THUMBNAIL_PX,
    dimensions,
    front_cover,
    has_cover,
    image_suffix,
    is_album_level,
    plan,
    write_one,
)

if TYPE_CHECKING:
    from pathlib import Path

#: Enough of an MP3 for mutagen to write a tag onto.
FRAME = b"\xff\xfb\x90\x00" + b"\x00" * 413


def jpeg(width: int, height: int, pad: int = 0) -> bytes:
    """A byte string with a real JPEG header declaring those dimensions."""
    sof = b"\xff\xc0" + struct.pack(">H", 17) + b"\x08" + struct.pack(">HH", height, width)
    return b"\xff\xd8" + sof + b"\x00" * 10 + b"\x00" * pad + b"\xff\xd9"


def png(width: int, height: int) -> bytes:
    """The same for a PNG."""
    return b"\x89PNG\r\n\x1a\n" + struct.pack(">I", 13) + b"IHDR" + struct.pack(">II", width, height) + b"\x00" * 8


def track(folder: Path, name: str, *pictures: tuple[int, bytes]) -> Path:
    """An mp3 in `folder` carrying those pictures."""
    folder.mkdir(parents=True, exist_ok=True)
    at = folder / name
    at.write_bytes(FRAME * 40)
    tag = ID3()
    tag.add(TIT2(encoding=3, text=name))
    for kind, data in pictures:
        tag.add(APIC(encoding=3, mime="image/jpeg", type=kind, desc=str(kind), data=data))
    tag.save(at)
    return at


def test_a_label_logo_is_not_a_cover(tmp_path: Path) -> None:
    """The reason this cannot take whichever picture it finds first.

    494 `FISH` and 88 `PUBLISHER_LOGOTYPE` pictures sit in this library. Either
    one written as `cover.jpg` is a wrong cover, which is harder to notice than
    a missing one.
    """
    logo = jpeg(600, 600, pad=500)
    art = jpeg(500, 500, pad=100)
    at = track(
        tmp_path / "Artist" / "Album",
        "01 A.mp3",
        (PictureType.PUBLISHER_LOGOTYPE, logo),
        (PictureType.COVER_FRONT, art),
    )

    # The instrument: the logo really is the bigger of the two, so "largest
    # wins" alone would have picked it. Computed from the fixture, not from
    # the answer under test.
    assert len(logo) > len(art)
    assert front_cover([at]) == art


def test_an_artist_shot_alone_is_no_cover_at_all(tmp_path: Path) -> None:
    """`FISH` is what one tagger used for an artist photo. It is not a front."""
    at = track(tmp_path / "Artist" / "Album", "01 A.mp3", (PictureType.FISH, jpeg(600, 600)))
    assert front_cover([at]) is None, "better no cover than the wrong one"


def test_the_largest_front_across_the_album_is_taken(tmp_path: Path) -> None:
    """Several tracks usually carry the same image at different sizes."""
    album = tmp_path / "Artist" / "Album"
    small = track(album, "01 A.mp3", (PictureType.COVER_FRONT, jpeg(300, 300, pad=50)))
    big = track(album, "02 B.mp3", (PictureType.COVER_FRONT, jpeg(1000, 1000, pad=900)))
    assert front_cover([small, big]) == jpeg(1000, 1000, pad=900)


def test_bytes_that_are_not_an_image_are_refused(tmp_path: Path) -> None:
    """A picture frame holds whatever the tagger put there."""
    assert image_suffix(jpeg(10, 10)) == ".jpg"
    assert image_suffix(png(10, 10)) == ".png"
    assert image_suffix(b"<html>not an image</html>") is None

    at = track(tmp_path / "Artist" / "Album", "01 A.mp3", (PictureType.COVER_FRONT, b"<html>nope</html>"))
    row = plan(tmp_path, at.parent, [at])
    assert row is not None
    assert row["apply"] == "", "nothing is written from bytes no viewer will open"
    assert row["why"] == "embedded bytes are not an image"
    assert not list(at.parent.glob("cover.*"))


def test_a_loose_single_under_an_artist_gets_no_cover(tmp_path: Path) -> None:
    """Issue #20: singles have no layout, and this must not invent one.

    A first run over the real library wrote `Jeremie Albino/cover.jpg` and
    `TAISUN/cover.jpg` beside two stray singles, which is exactly the ad-hoc
    layout #20 says not to invent.
    """
    loose = track(tmp_path / "Artist", "Rollin'.mp3", (PictureType.COVER_FRONT, jpeg(500, 500)))
    assert is_album_level(tmp_path, loose.parent) is False

    row = plan(tmp_path, loose.parent, [loose])
    assert row is not None
    assert row["apply"] == ""
    assert "#20" in str(row["why"])

    # The instrument: the same picture one level down *is* written, so the
    # refusal is about where the folder sits and not about the art.
    proper = track(tmp_path / "Artist" / "Album", "01 A.mp3", (PictureType.COVER_FRONT, jpeg(500, 500)))
    deeper = plan(tmp_path, proper.parent, [proper])
    assert deeper is not None
    assert deeper["apply"] == "y"


def test_a_disc_folder_is_deep_enough(tmp_path: Path) -> None:
    """`Artist/Album/CD1` is an album folder; only the artist level is not."""
    assert is_album_level(tmp_path, tmp_path / "Artist" / "Album") is True
    assert is_album_level(tmp_path, tmp_path / "Artist" / "Album" / "CD1") is True
    assert is_album_level(tmp_path, tmp_path / "Artist") is False


def test_an_existing_cover_is_never_overwritten(tmp_path: Path) -> None:
    """The art on disk may be better than the art in the tag."""
    album = tmp_path / "Artist" / "Album"
    at = track(album, "01 A.mp3", (PictureType.COVER_FRONT, jpeg(500, 500)))
    kept = album / "cover.jpg"
    kept.write_bytes(b"\xff\xd8\xff already here")

    assert has_cover(album) is True
    assert plan(tmp_path, album, [at]) is None
    assert write_one(album, [at]) == "already has a cover file"
    assert kept.read_bytes() == b"\xff\xd8\xff already here"


def test_writing_puts_the_embedded_bytes_on_disk(tmp_path: Path) -> None:
    """End to end, and the file must be the picture rather than a re-encode."""
    album = tmp_path / "Artist" / "Album"
    art = jpeg(700, 700, pad=300)
    at = track(album, "01 A.mp3", (PictureType.COVER_FRONT, art))

    assert write_one(album, [at]) == "wrote cover.jpg"
    assert (album / "cover.jpg").read_bytes() == art, "byte for byte, not re-encoded"
    assert write_one(album, [at]) == "already has a cover file", "a second pass has nothing to do"


def test_a_png_keeps_its_own_extension(tmp_path: Path) -> None:
    """46 of these are PNG. Naming one `.jpg` would be a lie on disk."""
    album = tmp_path / "Artist" / "Album"
    at = track(album, "01 A.mp3", (PictureType.COVER_FRONT, png(500, 500)))
    assert write_one(album, [at]) == "wrote cover.png"
    assert (album / "cover.png").exists()
    assert not (album / "cover.jpg").exists()


def test_dimensions_are_read_from_the_header() -> None:
    """So a thumbnail can be named as one rather than silently enshrined."""
    assert dimensions(jpeg(1400, 1400)) == (1400, 1400)
    assert dimensions(png(640, 480)) == (640, 480)
    assert dimensions(b"not an image") is None


def test_a_thumbnail_is_written_but_said_to_be_one(tmp_path: Path) -> None:
    """24 covers here are 174-240px.

    They are written, because the same picture is already what a player reads
    out of the tag — so writing it costs nothing. What it must not do is pass
    silently, or nothing will ever find the ones worth replacing.
    """
    album = tmp_path / "Artist" / "Album"
    at = track(album, "01 A.mp3", (PictureType.COVER_FRONT, jpeg(174, 174)))
    row = plan(tmp_path, album, [at])
    assert row is not None
    assert row["apply"] == "y", "a small cover still beats none"
    assert "thumbnail" in str(row["why"])
    assert row["px"] == "174x174"

    # The instrument: a full-size cover goes through the same path and says
    # nothing, so the warning is about the size and not about every row.
    other = tmp_path / "Artist" / "Big"
    full = track(other, "01 A.mp3", (PictureType.COVER_FRONT, jpeg(THUMBNAIL_PX + 200, THUMBNAIL_PX + 200)))
    big_row = plan(tmp_path, other, [full])
    assert big_row is not None
    assert big_row["why"] == ""
