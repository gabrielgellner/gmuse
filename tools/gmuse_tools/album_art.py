"""The cover beside each rendered album — #348.

`renderings.md` §5 keeps a file's embedded pictures and gives a transcode its
master's. This library's art mostly lives *beside* the album instead:
`covers apply` wrote a `cover.jpg` into 1,006 of 1,055 album folders, and many
masters embed none — #335's live run rendered Orbital's *Diversions* with no
art at all. So a rendering carries the cover too.

## The cover, and only the cover

`storage-layout.md` §9 names a release's assets: art, what a rip leaves, a
playlist over one release. A rendering is for playing, so it carries **the
cover** — `cover.jpg` or `cover.png`, the names `covers apply` writes and Plex
reads — and not rip logs or cue sheets, which help nobody listening. Measured
2026-10-24: 1,064 `cover.jpg` and 16 `cover.png` at album level. Another name
(13 `folder.*` here) is not a cover by this convention and is not guessed at.

## Where it comes from

A rendered album folder is filled from master folders — usually one, but a
choice of copy can draw on two: a FLAC from one folder and an MP3 from
another, or the library's `Enya` and `The Celts` copies of one track. Each
master folder may offer a cover. **One answer, or the same bytes from every
folder, is the cover. Two different covers is a question**, reported and not
settled — which art an album should wear is a judgement, and picking by path
order would make one silently.

## Only an album folder

A rendered path with an album level, `Artist/Album/01 Track`, has a folder a
cover can sit in. A track on no release lands directly under its artist, and
a cover there would be the artist's — `covers.py` refuses that for the same
reason, so this does too.
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING, Final

if TYPE_CHECKING:
    from collections.abc import Iterable

#: The names a cover goes by: what `covers apply` writes and Plex reads.
NAMES: Final = ("cover.jpg", "cover.png")

#: `Artist/Album/Track`: a rendered path this deep has an album folder.
ALBUM_DEPTH: Final = 3


@dataclass
class Covers:
    """Each rendered album folder's cover, and the folders that could not agree on one."""

    #: `rendered cover path → master cover file`.
    chosen: dict[str, Path] = field(default_factory=dict)
    #: `(rendered folder, the master covers that disagree)`.
    contested: list[tuple[str, list[Path]]] = field(default_factory=list)


def offered(folder: Path) -> Path | None:
    """The cover a master folder holds, if it holds one by a cover's name."""
    for name in NAMES:
        at = folder / name
        if at.is_file():
            return at
    return None


def same_bytes(paths: list[Path]) -> bool:
    """Whether these files are one picture — by size first, and only then by content."""
    if len({p.stat().st_size for p in paths}) > 1:
        return False
    return len({hashlib.sha256(p.read_bytes()).digest() for p in paths}) == 1


def covers_for(placed: Iterable[tuple[str, Path]]) -> Covers:
    """Find the cover for every rendered album folder, from the masters its files came from.

    `placed` is `(rendered path, master it was made from)` for every file the
    rendering holds.
    """
    sources: dict[str, set[Path]] = {}
    for rendered, master in placed:
        path = PurePosixPath(rendered)
        if len(path.parts) < ALBUM_DEPTH:
            continue
        sources.setdefault(str(path.parent), set()).add(master.parent)

    out = Covers()
    for folder, masters in sorted(sources.items()):
        found = sorted({c for m in masters if (c := offered(m)) is not None})
        if not found:
            continue
        if len(found) > 1 and not same_bytes(found):
            out.contested.append((folder, found))
            continue
        out.chosen[str(PurePosixPath(folder) / found[0].name)] = found[0]
    return out


def signature(cover: Path) -> str:
    """What the manifest records of a cover: its size and mtime, so a replaced cover is rewritten.

    Not a hash: a run would read a thousand pictures to learn that none of
    them changed, and the masters' covers are replaced by writing a new file.
    """
    st = cover.stat()
    return f"{st.st_size}-{st.st_mtime_ns}"
