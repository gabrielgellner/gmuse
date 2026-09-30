"""The album's front cover, out of the tags and onto the disk.

`music-library-conventions` asks for one `cover.jpg` per album folder, and
before this pass **46 of 1055 album folders had one** while 965 carried the
image inside every track. The art was there; nothing had ever written it out.

## Not whichever picture comes first

ID3 gives a picture a type, and this library's files use more than one. Beside
964 `COVER_FRONT` pictures there are **494 `FISH` and 88
`PUBLISHER_LOGOTYPE`** — an artist shot and a label logo. Taking the first
picture found would have filed those as covers on hundreds of albums, and a
wrong cover is harder to notice than a missing one.

So only `COVER_FRONT` and `OTHER` are eligible, and where several tracks carry
one the **largest** is taken: they are usually the same image at different
sizes, and the biggest is the one worth keeping.

## Album level only

A folder holding audio directly under an artist is the singles case (#20),
which has no layout yet. A first run wrote `Jeremie Albino/cover.jpg` and
`TAISUN/cover.jpg` beside two stray singles — inventing exactly the ad-hoc
layout #20 says not to invent. Those folders are reported and skipped.

## The bytes are checked before they are written

A picture frame holds whatever the tagger put there. `image_suffix` reads the
magic bytes and refuses anything that is not a JPEG or a PNG, so a broken
frame becomes a row in the review rather than a `cover.jpg` that no viewer
will open.

    uv run gmuse-tools covers survey ~/MusicLibrary          # -> out/covers-review.tsv
    uv run gmuse-tools covers apply  ~/MusicLibrary          # dry run
    uv run gmuse-tools covers apply  ~/MusicLibrary --write
"""

from __future__ import annotations

import collections
import contextlib
import csv
import struct
from pathlib import Path
from typing import Final

import typer
from mutagen import File as MutagenFile
from mutagen import MutagenError
from mutagen.id3 import PictureType

from gmuse_tools.genre.common import out_path, walk, write_tsv

TRUTHY: Final = frozenset({"y", "yes", "1"})

#: A cover already on disk, under any spelling this library uses.
COVER_NAMES: Final = frozenset({"cover.jpg", "cover.jpeg", "cover.png"})

#: The picture types that are an album's front. **`FISH` and
#: `PUBLISHER_LOGOTYPE` are deliberately absent** — 494 and 88 of them here,
#: and neither is a cover.
FRONT: Final = (PictureType.COVER_FRONT, PictureType.OTHER)

#: What an image looks like from the outside, and the only two written.
MAGIC: Final = {b"\xff\xd8\xff": ".jpg", b"\x89PNG\r\n\x1a\n": ".png"}

#: JPEG frame headers that carry the dimensions. The other `FF Cx` markers are
#: Huffman tables and restarts, which do not.
JPEG_SOF: Final = frozenset({0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF})
#: Every JPEG marker starts with this byte.
JPEG_MARK: Final = 0xFF
#: Markers that stand alone, carrying no length to skip over: start and end of
#: image, and the eight restart markers.
JPEG_BARE: Final = frozenset({0xD8, 0xD9, *range(0xD0, 0xD8)})

#: Below this on the long side an image is a thumbnail rather than a cover. It
#: is reported, not refused: the same picture is already what a player reads
#: out of the tag, so writing it costs nothing and naming it lets somebody
#: find the 24 worth replacing.
THUMBNAIL_PX: Final = 300


def pictures(path: Path) -> list[tuple[int, bytes]]:
    """Every embedded picture in one file, as `(type, bytes)`.

    Three containers keep art three ways — ID3 `APIC`, FLAC's own picture
    block, and MP4's `covr` atom, which has no type at all and is a front
    cover by construction.
    """
    try:
        audio = MutagenFile(path)
    except (MutagenError, OSError, ValueError):
        return []
    if audio is None or audio.tags is None:
        return []
    found: list[tuple[int, bytes]] = []
    if hasattr(audio.tags, "getall"):
        found += [(getattr(p, "type", PictureType.OTHER), p.data) for p in audio.tags.getall("APIC")]
    if hasattr(audio, "pictures"):
        found += [(getattr(p, "type", PictureType.OTHER), p.data) for p in audio.pictures]
    with contextlib.suppress(KeyError, TypeError, ValueError):
        found += [(PictureType.COVER_FRONT, bytes(p)) for p in audio.tags["covr"]]
    return found


def front_cover(files: list[Path]) -> bytes | None:
    """The biggest front cover across an album's tracks, or None."""
    best: bytes | None = None
    for path in sorted(files):
        for kind, data in pictures(path):
            if kind in FRONT and (best is None or len(data) > len(best)):
                best = data
    return best


def image_suffix(data: bytes) -> str | None:
    """`.jpg`, `.png`, or None when the bytes are not an image at all."""
    for magic, suffix in MAGIC.items():
        if data.startswith(magic):
            return suffix
    return None


def dimensions(data: bytes) -> tuple[int, int] | None:
    """`(width, height)` read from the header, or None if it cannot be."""
    try:
        if data.startswith(b"\x89PNG\r\n\x1a\n"):
            width, height = struct.unpack(">II", data[16:24])
            return width, height
        at = 2
        while at < len(data) - 9:
            if data[at] != JPEG_MARK:
                at += 1
                continue
            marker = data[at + 1]
            if marker in JPEG_SOF:
                height, width = struct.unpack(">HH", data[at + 5 : at + 9])
                return width, height
            if marker in JPEG_BARE:
                at += 2
                continue
            at += 2 + struct.unpack(">H", data[at + 2 : at + 4])[0]
    except (struct.error, IndexError):
        return None
    return None


def has_cover(folder: Path) -> bool:
    """Whether a cover file already sits in this folder."""
    try:
        return any(p.name.lower() in COVER_NAMES for p in folder.iterdir() if p.is_file())
    except OSError:
        return False


def album_folders(root: Path) -> dict[Path, list[Path]]:
    """Every folder holding audio directly, and what it holds."""
    folders: dict[Path, list[Path]] = collections.defaultdict(list)
    for path in walk(root):
        folders[path.parent].append(path)
    return dict(folders)


def is_album_level(root: Path, folder: Path) -> bool:
    """Whether a folder is deep enough to be an album rather than an artist.

    `Artist/Album` and `Artist/Album/CD1` are; `Artist` holding a loose single
    is not — that is #20, and a cover written there invents a layout.
    """
    try:
        return len(folder.relative_to(root).parts) >= 2  # noqa: PLR2004 — artist/album
    except ValueError:
        return False


def plan(root: Path, folder: Path, files: list[Path]) -> dict[str, object] | None:
    """What this folder needs, or None when it already has a cover."""
    if has_cover(folder):
        return None
    art = front_cover(files)
    if art is None:
        return {"apply": "", "cover": "", "px": "", "kb": "", "why": "no front cover embedded", "path": str(folder)}
    suffix = image_suffix(art)
    if suffix is None:
        return {
            "apply": "",
            "cover": "",
            "px": "",
            "kb": f"{len(art) / 1024:.0f}",
            "why": "embedded bytes are not an image",
            "path": str(folder),
        }
    if not is_album_level(root, folder):
        return {
            "apply": "",
            "cover": "",
            "px": "",
            "kb": f"{len(art) / 1024:.0f}",
            "why": "loose audio under an artist — no layout for a cover here (#20)",
            "path": str(folder),
        }
    size = dimensions(art)
    why = ""
    if size and max(size) < THUMBNAIL_PX:
        why = f"thumbnail — {max(size)}px, worth replacing"
    return {
        "apply": "y",
        "cover": f"cover{suffix}",
        "px": f"{size[0]}x{size[1]}" if size else "",
        "kb": f"{len(art) / 1024:.0f}",
        "why": why,
        "path": str(folder),
    }


def _by_path(row: dict[str, object]) -> str:
    """Sort key for the review file, so a re-run diffs cleanly."""
    return str(row["path"])


def survey(root: Path, out: Path) -> None:
    """Look at every album and write the plan. Changes nothing."""
    rows: list[dict[str, object]] = []
    tally: collections.Counter[str] = collections.Counter()
    for folder, files in sorted(album_folders(root).items()):
        row = plan(root, folder, files)
        if row is None:
            tally["already has a cover file"] += 1
            continue
        rows.append(row)
        if row["apply"]:
            tally[f"would write {row['cover']}"] += 1
            if row["why"]:
                tally["  of those, a thumbnail"] += 1
        else:
            tally[str(row["why"])] += 1
    rows.sort(key=_by_path)
    write_tsv(out, rows)
    for key, n in sorted(tally.items()):
        print(f"  {n:6d}  {key}")
    print(f"\n{len(rows)} folder(s) -> {out}")


def write_one(folder: Path, files: list[Path]) -> str:
    """Write one folder's cover. Returns what happened."""
    if has_cover(folder):
        return "already has a cover file"
    art = front_cover(files)
    if art is None:
        return "no front cover embedded"
    suffix = image_suffix(art)
    if suffix is None:
        return "embedded bytes are not an image"
    target = folder / f"cover{suffix}"
    if target.exists():
        return "already has a cover file"
    target.write_bytes(art)
    return f"wrote cover{suffix}"


def apply(review: Path, *, write: bool) -> None:
    """Write the covers for the rows marked `y`. Dry run unless `--write`."""
    if not review.exists():
        print(f"no {review.name} — run `survey` first")
        return
    with review.open(encoding="utf-8") as handle:
        rows = [r for r in csv.DictReader(handle, delimiter="\t") if r["apply"].strip().lower() in TRUTHY]

    tally: collections.Counter[str] = collections.Counter()
    for row in rows:
        folder = Path(row["path"])
        if not write:
            tally[f"would write {row['cover']}"] += 1
            continue
        files = [p for p in walk(folder) if p.parent == folder]
        try:
            tally[write_one(folder, files)] += 1
        except OSError as err:
            tally[f"failed: {type(err).__name__}"] += 1
    for key, n in sorted(tally.items()):
        print(f"  {n:6d}  {key}")
    if not write:
        print("dry run — pass --write to write the files")


LIB = typer.Argument(Path("~/MusicLibrary"), help="library root")
WRITE = typer.Option(default=False, help="actually write the files; without this, a dry run")


def register(app: typer.Typer) -> None:
    """Attach the cover extractor."""
    covers_app = typer.Typer(no_args_is_help=True, help=__doc__)
    app.add_typer(covers_app, name="covers")

    @covers_app.command("survey")
    def survey_cmd(root: Path = LIB) -> None:
        """Report which albums have art in the tags and none on disk."""
        survey(root.expanduser(), out_path("covers-review.tsv"))

    @covers_app.command("apply")
    def apply_cmd(root: Path = LIB, *, write: bool = WRITE) -> None:
        """Write `cover.jpg` for the rows marked `y`, taking nothing away."""
        _ = root
        apply(out_path("covers-review.tsv"), write=write)
