"""Write what the library states into a rendered file's tags — #335.

`renderings.md` §5. A rendering's tags are **the library's statement and
nothing else's**: the values `gmuse_core.Library.statement` hands out, read
from the projection and never from a file. The masters are not touched — only
the file a rendering is about to land.

## What is written, and what goes

- **Every field the library states is written**, and **every field it does
  not is removed.** A stale tag left beside the library's value is a second
  answer, and a stale one where the library has none is a claim it never made.
  An album the library cannot name is no album tag, not "No Album" (§9).
- **Kept: pictures, lyrics and comments**, because the library does not model
  them and clearing them would lose them, not correct them.
- **An MP3 ends with one ID3v2.4 tag.** This library's carry an ID3v1 block,
  an APEv2 tag whose ReplayGain is stale, and — 39% of the staging tree — a
  second ID3v2 tag behind the first that can name a different recording
  (`tags.py`). A rendering inherits none of it. The cuts are `tags.py`'s own,
  which put the file back if the duration changes.
- **A transcode carries its source's pictures**, because ffmpeg encodes the
  audio stream only and the art would otherwise be gone from it.

## Why not `lofty`

#335 said `lofty`. `src/python.rs` keeps tag writing in Python, and the tools
already write with mutagen; a second tag writer behind the binding would be a
second answer to one question. The library states, the tool writes.
"""

from __future__ import annotations

import hashlib
import json
from typing import TYPE_CHECKING, Any, Final

from mutagen import File as MutagenFile
from mutagen.flac import FLAC, Picture
from mutagen.id3 import (
    APIC,
    ID3,
    TALB,
    TCMP,
    TCON,
    TDRC,
    TIT2,
    TPE1,
    TPE2,
    TPOS,
    TPUB,
    TRCK,
    TXXX,
    UFID,
    ID3NoHeaderError,
)
from mutagen.mp4 import MP4, MP4FreeForm
from mutagen.oggopus import OggOpus
from mutagen.oggvorbis import OggVorbis

from gmuse_tools.tags import strip_ape_bytes, strip_extra_id3_bytes

if TYPE_CHECKING:
    from collections.abc import Mapping
    from pathlib import Path

#: What a rendering keeps of a file's own tags: what the library does not model.
ID3_KEPT: Final = ("APIC", "USLT", "COMM")
VORBIS_KEPT: Final = frozenset({"METADATA_BLOCK_PICTURE", "LYRICS", "UNSYNCEDLYRICS", "COMMENT", "DESCRIPTION"})
MP4_KEPT: Final = frozenset({"covr", "\xa9lyr", "\xa9cmt"})

#: Where MusicBrainz's own tagger puts a recording id in ID3.
MUSICBRAINZ: Final = "http://musicbrainz.org"

#: An MP4 freeform atom's namespace, as iTunes and Picard write them.
ITUNES: Final = "----:com.apple.iTunes:"


class UntaggableError(ValueError):
    """A file whose format this writer does not know how to tag."""


def _text(v: object) -> list[str]:
    return [str(v)]


def _number(v: object) -> list[str]:
    return [str(int(v))]  # type: ignore[call-overload]


def _gain(v: object) -> list[str]:
    return [f"{float(v):+.2f} dB"]  # type: ignore[arg-type]


def _peak(v: object) -> list[str]:
    return [f"{float(v):.6f}"]  # type: ignore[arg-type]


#: How each field the library states becomes the strings a tag holds.
FORMATS: Final = {
    "title": _text,
    "artist": _text,
    "album": _text,
    "album_artist": _text,
    "label": _text,
    "musicbrainz_recording": _text,
    "musicbrainz_release": _text,
    "track": _number,
    "disc": _number,
    "discs": _number,
    "year": _number,
    "genres": lambda v: [str(g) for g in v],
    "compilation": lambda _: ["1"],
    "track_gain_db": _gain,
    "album_gain_db": _gain,
    "track_peak": _peak,
    "album_peak": _peak,
}


def canonical(statement: Mapping[str, Any]) -> dict[str, list[str]]:
    """The statement as the strings a tag holds, one list per field.

    **What the manifest hashes and what every format writes**, so a value
    that would be written identically hashes identically: a gain rounded to
    the hundredth a ReplayGain tag holds, not the `f32` the projection keeps.
    An empty value, a `False` and a field this writer does not know are not
    written — the library said nothing a tag could hold.
    """
    return {k: fmt(statement[k]) for k, fmt in FORMATS.items() if statement.get(k) not in (None, "", [], False)}


def digest(statement: Mapping[str, Any]) -> str:
    """A short hash of what would be written — the manifest's `tags` column."""
    body = json.dumps(canonical(statement), sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(body.encode()).hexdigest()[:16]


def write(path: Path, statement: Mapping[str, Any], *, pictures_from: Path | None = None) -> None:
    """Make `path`'s tags the library's statement.

    `pictures_from` is the master a transcode was made from, whose pictures
    the transcode lacks.
    """
    tags = canonical(statement)
    pictures = pictures_of(pictures_from) if pictures_from is not None else []
    probe = MutagenFile(path)
    if probe is None:
        msg = f"{path.name}: not a format mutagen reads"
        raise UntaggableError(msg)
    if path.suffix.lower() == ".mp3":
        write_id3(path, tags, pictures)
    elif isinstance(probe, (FLAC, OggVorbis, OggOpus)):
        write_vorbis(probe, tags, pictures)
    elif isinstance(probe, MP4):
        write_mp4(probe, tags)
    else:
        msg = f"{path.name}: no tag writer for {type(probe).__name__}"
        raise UntaggableError(msg)


def pictures_of(path: Path) -> list[Picture]:
    """The pictures a master carries, in FLAC's form — what every writer here can take."""
    f = MutagenFile(path)
    if isinstance(f, FLAC):
        return list(f.pictures)
    return []


#: The ID3 text frame each plain field is written as.
ID3_TEXT: Final = {
    "title": TIT2,
    "artist": TPE1,
    "album": TALB,
    "album_artist": TPE2,
    "label": TPUB,
    "track": TRCK,
    "year": TDRC,
    "genres": TCON,
    "compilation": TCMP,
}


def write_id3(path: Path, tags: Mapping[str, list[str]], pictures: list[Picture]) -> None:
    """One ID3v2.4 tag, holding the statement and what it keeps."""
    strip_ape_bytes(path)
    strip_extra_id3_bytes(path)
    id3 = kept_id3(path, pictures)
    for key, frame in ID3_TEXT.items():
        if key in tags:
            id3.add(frame(encoding=3, text=tags[key]))
    if "disc" in tags:
        of = f"/{tags['discs'][0]}" if "discs" in tags else ""
        id3.add(TPOS(encoding=3, text=[tags["disc"][0] + of]))
    for frame in identity_frames(tags):
        id3.add(frame)
    # `v1=0` removes an ID3v1 block; v2.4 holds a multi-value frame, which
    # v2.3 cannot (`tags.py`).
    id3.save(path, v1=0, v2_version=4)


def kept_id3(path: Path, pictures: list[Picture]) -> ID3:
    """The file's ID3 tag with only what a rendering keeps, and a transcode's pictures added."""
    try:
        id3 = ID3(path)
    except ID3NoHeaderError:
        id3 = ID3()
    kept = [frame for key in ID3_KEPT for frame in id3.getall(key)]
    id3.clear()
    for frame in kept:
        id3.add(frame)
    for p in pictures:
        id3.add(APIC(encoding=3, mime=p.mime, type=p.type, desc=p.desc, data=p.data))
    return id3


def identity_frames(tags: Mapping[str, list[str]]) -> list[UFID | TXXX]:
    """The MusicBrainz ids and the ReplayGain, as MusicBrainz's tagger and rsgain write them."""
    frames: list[UFID | TXXX] = []
    if "musicbrainz_recording" in tags:
        frames.append(UFID(owner=MUSICBRAINZ, data=tags["musicbrainz_recording"][0].encode()))
    if "musicbrainz_release" in tags:
        frames.append(TXXX(encoding=3, desc="MusicBrainz Album Id", text=tags["musicbrainz_release"]))
    frames.extend(TXXX(encoding=3, desc=desc, text=tags[key]) for key, desc in GAIN_KEYS.items() if key in tags)
    return frames


#: ReplayGain's own tag names, which every format here spells the same way.
GAIN_KEYS: Final = {
    "track_gain_db": "REPLAYGAIN_TRACK_GAIN",
    "track_peak": "REPLAYGAIN_TRACK_PEAK",
    "album_gain_db": "REPLAYGAIN_ALBUM_GAIN",
    "album_peak": "REPLAYGAIN_ALBUM_PEAK",
}

#: The Vorbis comment each field is written as.
VORBIS_KEYS: Final = {
    "title": "TITLE",
    "artist": "ARTIST",
    "album": "ALBUM",
    "album_artist": "ALBUMARTIST",
    "label": "LABEL",
    "track": "TRACKNUMBER",
    "disc": "DISCNUMBER",
    "discs": "DISCTOTAL",
    "year": "DATE",
    "genres": "GENRE",
    "compilation": "COMPILATION",
    "musicbrainz_recording": "MUSICBRAINZ_TRACKID",
    "musicbrainz_release": "MUSICBRAINZ_ALBUMID",
    **GAIN_KEYS,
}


def write_vorbis(f: FLAC | OggVorbis | OggOpus, tags: Mapping[str, list[str]], pictures: list[Picture]) -> None:
    """Vorbis comments holding the statement and what they keep.

    A FLAC's pictures are their own metadata blocks and are untouched by
    clearing the comments; an Ogg's live in a comment, which is kept.
    """
    if f.tags is None:
        f.add_tags()
    comments = f.tags
    kept = [(k, v) for k, v in comments.items() if k.upper() in VORBIS_KEPT]
    comments.clear()
    for k, v in kept:
        comments[k] = v
    for key, name in VORBIS_KEYS.items():
        if key in tags:
            comments[name] = tags[key]
    if isinstance(f, FLAC) and not f.pictures:
        for p in pictures:
            f.add_picture(p)
    f.save()


#: The MP4 atom each field is written as; freeform ones carry the iTunes namespace.
MP4_KEYS: Final = {
    "title": "\xa9nam",
    "artist": "\xa9ART",
    "album": "\xa9alb",
    "album_artist": "aART",
    "year": "\xa9day",
    "genres": "\xa9gen",
}
MP4_FREEFORM: Final = {
    "label": "LABEL",
    "musicbrainz_recording": "MusicBrainz Track Id",
    "musicbrainz_release": "MusicBrainz Album Id",
    **{k: v.lower() for k, v in GAIN_KEYS.items()},
}


def write_mp4(f: MP4, tags: Mapping[str, list[str]]) -> None:
    """MP4 atoms holding the statement and what they keep."""
    if f.tags is None:
        f.add_tags()
    atoms = f.tags
    kept = {k: v for k, v in atoms.items() if k in MP4_KEPT}
    atoms.clear()
    atoms.update(kept)
    for key, atom in MP4_KEYS.items():
        if key in tags:
            atoms[atom] = tags[key]
    if "track" in tags:
        atoms["trkn"] = [(int(tags["track"][0]), 0)]
    if "disc" in tags:
        atoms["disk"] = [(int(tags["disc"][0]), int(tags["discs"][0]) if "discs" in tags else 0)]
    if "compilation" in tags:
        atoms["cpil"] = True
    for key, name in MP4_FREEFORM.items():
        if key in tags:
            atoms[ITUNES + name] = [MP4FreeForm(v.encode()) for v in tags[key]]
    f.save()
