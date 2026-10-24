"""What a rendering's tags say, written into real files of every format here — #335.

The claim is mostly about **what is gone**: a stale album, a wrong MusicBrainz
id, an ID3v1 block, an APE tag, a stacked second ID3v2 tag. An absence is only
worth asserting where the thing was first seen to be there, so every file is
seeded with the stale tag and the test checks it was present before it checks
it was removed.

The files are made by ffmpeg, like `flacfixture.py`'s: a real tone, so the
"audio unchanged" check compares decoded samples rather than silence.
"""

from __future__ import annotations

import subprocess
from typing import TYPE_CHECKING, Any

import pytest
from mutagen import File as MutagenFile
from mutagen.apev2 import APEv2
from mutagen.flac import FLAC, Picture
from mutagen.id3 import ID3, TALB, TXXX, USLT
from mutagen.mp4 import MP4

from gmuse_tools.stated import canonical, digest, write
from gmuse_tools.tags import extra_id3_span, has_id3v1, id3_chain
from tests.test_tags import id3_tag, txxx

if TYPE_CHECKING:
    from pathlib import Path

#: What the library says about Orbital's "Lush", shaped as `statement` hands it out.
STATEMENT: dict[str, Any] = {
    "title": "Lush 3–4 Warrior Drift",
    "artist": "Orbital",
    "album_artist": "Orbital",
    "label": "FFRR",
    "track": 5,
    "disc": 1,
    "discs": 2,
    "year": 1994,
    "genres": ["Electronic", "Techno"],
    "musicbrainz_recording": "bb51fcff-fe80-46d4-af12-ab0dedcf57de",
    "musicbrainz_release": "d30d02cf-ec86-4c1a-9514-9daa09cf3139",
    "track_gain_db": -5.159999847412109,
    "track_peak": 0.9711999893188477,
    # No album: the library cannot name one, so the stale one must go.
}

CODECS = {
    "mp3": ["-c:a", "libmp3lame", "-b:a", "128k"],
    "flac": ["-c:a", "flac"],
    "ogg": ["-c:a", "vorbis", "-strict", "-2", "-ac", "2"],
    "opus": ["-c:a", "libopus"],
    "m4a": ["-c:a", "aac"],
}


def tone(path: Path) -> Path:
    """One second of a real tone in `path`'s format."""
    subprocess.run(
        ["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", "sine=f=440:d=1", *CODECS[path.suffix[1:]], str(path)],
        check=True,
    )
    return path


def audio_md5(path: Path) -> str:
    """The decoded samples' hash — what the file sounds like, not what it is called."""
    out = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(path), "-map", "0:a", "-f", "md5", "-"],
        check=True,
        capture_output=True,
        text=True,
    )
    return out.stdout.strip()


def picture() -> Picture:
    """A front cover, as FLAC holds one."""
    p = Picture()
    p.type = 3
    p.mime = "image/png"
    p.data = b"\x89PNG\r\n\x1a\n" + b"\x00" * 32
    return p


def seed(path: Path) -> None:
    """Give a file what a master in this library carries: a stale album, a wrong id, lyrics, art."""
    f = MutagenFile(path)
    if path.suffix == ".mp3":
        # The APE tag first: mutagen writing one drops an ID3v1 block behind it,
        # and the ID3v1 block belongs at the very end.
        ape = APEv2()
        ape["REPLAYGAIN_TRACK_GAIN"] = "+9.99 dB"  # mp3gain's, stale
        ape.save(path)
        id3 = ID3(path)
        id3.add(TALB(encoding=3, text=["a stale album"]))
        id3.add(TXXX(encoding=3, desc="MusicBrainz Album Id", text=["00000000-0000-0000-0000-000000000000"]))
        id3.add(USLT(encoding=3, lang="eng", desc="", text="la la la"))
        id3.save(path, v1=2)
    elif isinstance(f, MP4):
        f["\xa9alb"] = ["a stale album"]
        f["\xa9lyr"] = ["la la la"]
        f.save()
    else:
        f["ALBUM"] = ["a stale album"]
        f["MUSICBRAINZ_ALBUMID"] = ["00000000-0000-0000-0000-000000000000"]
        f["LYRICS"] = ["la la la"]
        if isinstance(f, FLAC):
            f.add_picture(picture())
        f.save()


def text(x: object) -> str:
    """A tag value as a string; an MP4 freeform atom is bytes."""
    return x.decode() if isinstance(x, bytes) else str(x)


def read(path: Path) -> dict[str, list[str]]:
    """Every text tag the file now carries, by mutagen's own key lowercased, values as strings."""
    f = MutagenFile(path)
    if path.suffix == ".mp3":
        return {k.lower(): [text(x) for x in getattr(v, "text", [v])] for k, v in ID3(path).items()}
    return {k.lower(): [text(x) for x in v] for k, v in (f.tags or {}).items()}


@pytest.mark.parametrize("ext", sorted(CODECS))
def test_a_file_says_what_the_library_says_and_nothing_stale(tmp_path: Path, ext: str) -> None:
    """Every format: the statement written, the stale album and id gone, lyrics kept, audio untouched."""
    at = tone(tmp_path / f"t.{ext}")
    seed(at)
    before = read(at)
    sound = audio_md5(at)
    stale_album = {"talb", "album", "\xa9alb"} & set(before)
    assert stale_album, f"instrument: the seeded album is there to be removed: {sorted(before)}"

    write(at, STATEMENT)
    after = read(at)
    flat = {v for vs in after.values() for v in vs}
    assert "Lush 3–4 Warrior Drift" in flat
    assert "Orbital" in flat
    assert {"Electronic", "Techno"} <= flat, "genres as two values, not one joined string"
    assert "a stale album" not in flat, "the library names no album, so the file names none"
    assert "00000000-0000-0000-0000-000000000000" not in flat, "the wrong release id is gone"
    assert "d30d02cf-ec86-4c1a-9514-9daa09cf3139" in flat
    assert "-5.16 dB" in flat, "the gain as ReplayGain writes it"
    assert any("la la la" in v for v in flat) or "uslt::eng" in after, "lyrics are kept"
    assert audio_md5(at) == sound, "tagging does not touch a sample"


def test_an_mp3_ends_with_one_id3v24_tag_and_nothing_else(tmp_path: Path) -> None:
    """ID3v1, APE and a stacked second ID3v2 are all cut — `tags.py`'s own cuts."""
    at = tone(tmp_path / "t.mp3")
    seed(at)
    (_, _, first_end), *_ = id3_chain(at)
    raw = at.read_bytes()
    stack = id3_tag(txxx("MusicBrainz Release Track Id", "a different recording"))
    at.write_bytes(raw[:first_end] + stack + raw[first_end:])
    assert extra_id3_span(at) is not None, "instrument: a stacked tag is there"
    assert has_id3v1(at), "instrument: an ID3v1 block is there"
    assert APEv2(at), "instrument: an APE tag is there"

    write(at, STATEMENT)
    assert extra_id3_span(at) is None
    assert not has_id3v1(at)
    with pytest.raises(Exception):  # noqa: B017, PT011 — mutagen says "no APE tag" by raising
        APEv2(at)
    assert ID3(at).version == (2, 4, 0)


def test_a_transcode_gets_its_masters_pictures(tmp_path: Path) -> None:
    """Ffmpeg encodes audio only, so the art comes across from the FLAC."""
    master = tone(tmp_path / "m.flac")
    f = FLAC(master)
    f.add_picture(picture())
    f.save()
    encode = tone(tmp_path / "e.mp3")
    assert not ID3(encode).getall("APIC"), "instrument: the encode has no art of its own"

    write(encode, STATEMENT, pictures_from=master)
    art = ID3(encode).getall("APIC")
    assert len(art) == 1
    assert art[0].type == 3
    assert art[0].data == picture().data


def test_a_flacs_own_pictures_survive(tmp_path: Path) -> None:
    """A FLAC's art is its own metadata block, which clearing comments must not touch."""
    at = tone(tmp_path / "t.flac")
    seed(at)
    assert FLAC(at).pictures, "instrument: the seeded picture is there"
    write(at, STATEMENT)
    assert len(FLAC(at).pictures) == 1


def test_the_hash_is_of_what_is_written_not_of_float_noise() -> None:
    """Two gains that round to one tag value are one statement; a retitle is another."""
    noisy = {**STATEMENT, "track_gain_db": -5.1600001}
    assert digest(noisy) == digest(STATEMENT)
    assert digest({**STATEMENT, "title": "Lush 3–4"}) != digest(STATEMENT)


def test_nothing_said_is_nothing_written() -> None:
    """An empty value, a `False` and an unknown field are not tag values."""
    out = canonical({"title": "x", "album": "", "compilation": False, "genres": [], "mood": "calm"})
    assert out == {"title": ["x"]}
    assert canonical({"compilation": True, "track": 5}) == {"track": ["5"], "compilation": ["1"]}


def test_the_hash_is_short_and_stable() -> None:
    """The manifest's column: sixteen hex characters, the same every run."""
    assert digest(STATEMENT) == digest(dict(reversed(STATEMENT.items())))
    assert len(digest(STATEMENT)) == 16
    assert int(digest(STATEMENT), 16) >= 0, "hex, so it sorts and diffs as text"
