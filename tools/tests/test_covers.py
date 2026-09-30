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
    folder_art,
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


# --- The art beside the audio — issue #304 -----------------------------------
#
# `covers` read the tags and nothing else, which is where the art was for 965
# of 1055 folders. But 57 of 213 albums matched against their staging source
# had an image on disk more than 1.5x larger than what the tag held, and 49
# folders reported as having no cover at all sometimes had a 950x950 sleeve
# sitting next to the music.
#
# So most of these are about *not* taking the biggest file: the folder is full
# of decoys and they are systematic.


def test_the_windows_media_cache_is_not_a_cover(tmp_path: Path) -> None:
    """`AlbumArt_{GUID}_Large.jpg` is a 200px cache copy, never the source."""
    album = tmp_path / "Artist" / "Album"
    album.mkdir(parents=True)
    (album / "AlbumArt_{C396AD83-FBC5-474C-BC95-70F5A0504E2C}_Large.jpg").write_bytes(jpeg(900, 900, pad=800))
    (album / "AlbumArtSmall.jpg").write_bytes(jpeg(75, 75))
    assert folder_art(album) is None, "a cache copy is not art somebody chose"

    # The instrument: an ordinary name in the same folder *is* found, so the
    # refusal is about the name and not about the folder or the bytes.
    (album / "folder.jpg").write_bytes(jpeg(600, 600, pad=500))
    found = folder_art(album)
    assert found is not None
    assert found[0].name == "folder.jpg"


def test_a_portrait_inlay_is_not_a_sleeve(tmp_path: Path) -> None:
    """The biggest file is often a back cover, a disc scan or an inlay.

    On this library the largest images beside the audio include `802x2301`
    and `941x1902` — Sublime inlays, taller than they are wide. A sleeve is
    square, and that is what separates them without naming them.
    """
    album = tmp_path / "Artist" / "Album"
    album.mkdir(parents=True)
    (album / "inlay.jpg").write_bytes(jpeg(941, 1902, pad=2000))
    (album / "front.jpg").write_bytes(jpeg(500, 500, pad=400))
    found = folder_art(album)
    assert found is not None
    assert found[0].name == "front.jpg", "square wins over merely large"
    assert max(found[1]) == 500


def test_the_best_folder_image_is_not_always_the_first(tmp_path: Path) -> None:
    """Jewel's *This Way*: `folder.jpg` is 200x199 and `folder(2).jpg` is 640x640."""
    album = tmp_path / "Artist" / "Album"
    album.mkdir(parents=True)
    (album / "folder.jpg").write_bytes(jpeg(200, 199))
    (album / "folder(1).jpg").write_bytes(jpeg(200, 200))
    (album / "folder(2).jpg").write_bytes(jpeg(640, 640, pad=600))
    found = folder_art(album)
    assert found is not None
    assert found[0].name == "folder(2).jpg"


def test_nothing_embedded_takes_the_folder_without_asking(tmp_path: Path) -> None:
    """*Pieces of You* had no tag picture and a 950x950 on disk.

    With nothing to weigh it against there is no judgement to make, so it
    goes through rather than being reported as "no front cover embedded".
    """
    album = tmp_path / "Artist" / "Album"
    at = track(album, "01 A.mp3")  # no pictures at all
    (album / "folder.jpg").write_bytes(jpeg(950, 950, pad=900))

    row = plan(tmp_path, album, [at])
    assert row is not None
    assert row["apply"] == "y"
    assert row["source"] == "folder.jpg"
    assert row["px"] == "950x950"

    assert write_one(album, [at], "folder.jpg") == "wrote cover.jpg from the folder"
    assert (album / "cover.jpg").read_bytes() == jpeg(950, 950, pad=900)


def test_a_bigger_folder_image_is_offered_not_taken(tmp_path: Path) -> None:
    """It is usually the same sleeve scanned larger — and sometimes it is not.

    57 of 213 albums have one, so this must not swap silently: which *image*
    is the cover is a judgement, unlike which file is bigger.
    """
    album = tmp_path / "Artist" / "Album"
    at = track(album, "01 A.mp3", (PictureType.COVER_FRONT, jpeg(500, 500, pad=100)))
    (album / "folder.jpg").write_bytes(jpeg(1417, 1417, pad=1400))

    row = plan(tmp_path, album, [at])
    assert row is not None
    assert row["apply"] == "", "a person decides which image is the cover"
    assert row["source"] == "folder.jpg"
    assert "1417x1417" in str(row["why"])
    assert not list(album.glob("cover.*")), "and nothing was written"


def test_a_folder_image_no_bigger_than_the_tag_is_left_alone(tmp_path: Path) -> None:
    """The instrument for the test above: the same scan twice is not a finding.

    Without this, every album with a `folder.jpg` would land in the review and
    the offer would mean nothing.
    """
    album = tmp_path / "Artist" / "Album"
    at = track(album, "01 A.mp3", (PictureType.COVER_FRONT, jpeg(600, 600, pad=500)))
    (album / "folder.jpg").write_bytes(jpeg(640, 640, pad=540))  # only 1.07x

    row = plan(tmp_path, album, [at])
    assert row is not None
    assert row["apply"] == "y", "the embedded one goes in as usual"
    assert row["source"] == ""
    assert row["why"] == ""


def test_a_back_cover_or_disc_scan_is_not_the_front(tmp_path: Path) -> None:
    """Shape alone does not separate these, which the first fix got wrong.

    A disc scan is round on a square canvas and a back cover is the same size
    as the front, so both pass a squareness test. Run over the real library,
    the first version offered `back.jpeg`, `cd.jpg` and `cd back.jpg` as front
    covers on three Aphex Twin albums.
    """
    album = tmp_path / "Artist" / "Album"
    album.mkdir(parents=True)
    for name in ("back.jpeg", "cd.jpg", "cd back.jpg", "inlay.jpg", "booklet.jpg"):
        (album / name).write_bytes(jpeg(900, 900, pad=800))
    assert folder_art(album) is None, "none of those is a front cover"

    # The instrument: a front-cover name of the *same shape and smaller* is
    # taken, so the refusal is about the name rather than the image.
    (album / "front.jpg").write_bytes(jpeg(400, 400, pad=300))
    found = folder_art(album)
    assert found is not None
    assert found[0].name == "front.jpg"


def test_a_name_merely_containing_those_words_is_kept(tmp_path: Path) -> None:
    """`Comeback.jpg` is not a back cover, and `Discovery.jpg` is not a disc."""
    album = tmp_path / "Artist" / "Album"
    album.mkdir(parents=True)
    (album / "Discovery.jpg").write_bytes(jpeg(800, 800, pad=700))
    found = folder_art(album)
    assert found is not None, "the words have to stand alone to mean anything"
    assert found[0].name == "Discovery.jpg"


def test_a_name_that_says_front_beats_a_bigger_one_that_does_not(tmp_path: Path) -> None:
    """Size alone picked a promo shot over the sleeve.

    `Aphex Twin/Come to Daddy` holds twelve scans. The largest square one is
    `promo.jpg` — a photograph of the artist — while `front2.jpg` and
    `vinyl front.jpg` are the sleeve.
    """
    album = tmp_path / "Artist" / "Album"
    album.mkdir(parents=True)
    (album / "promo.jpg").write_bytes(jpeg(900, 900, pad=800))
    (album / "front2.jpg").write_bytes(jpeg(435, 435, pad=300))
    found = folder_art(album)
    assert found is not None
    assert found[0].name == "front2.jpg", "the sleeve wins even though it is smaller"

    # The instrument: with no front-ish name present, the big one is still
    # taken — so the preference is a ranking and not a second rejection list.
    (album / "front2.jpg").unlink()
    fallback = folder_art(album)
    assert fallback is not None
    assert fallback[0].name == "promo.jpg"


def test_among_front_names_the_largest_wins(tmp_path: Path) -> None:
    """The ranking is name first, then size — not name instead of size."""
    album = tmp_path / "Artist" / "Album"
    album.mkdir(parents=True)
    (album / "front.jpg").write_bytes(jpeg(300, 300, pad=200))
    (album / "cover art.jpg").write_bytes(jpeg(900, 900, pad=800))
    found = folder_art(album)
    assert found is not None
    assert found[0].name == "cover art.jpg"
