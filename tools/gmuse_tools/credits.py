#!/usr/bin/env python3
"""Normalise multi-artist credits: one key spelling, real lists, no slash-joining.

The problem is not missing data, it is data written four different ways. This
library carries **four spellings of the MusicBrainz artist id field at once**
(`TXXX:MusicBrainz Artist Id`, `TXXX:musicbrainz_artistid`, the Vorbis
`musicbrainz_artistid`, and the iTunes freeform atom), and they disagree
*within a single file*. `ARTISTS` has three spellings and two shapes: a true
multi-value list in Vorbis, and a `/`-joined single string in ID3.

That last shape is the reason this is a pass and not a one-liner. **`/` is
ambiguous**: `AC/DC` is one artist and this library owns eight of its tracks.
Splitting on the separator would invent a band called DC.

The arbiter is the MusicBrainz id count. Two names and two ids agree, so the
slash is a separator; two names and one id do not, so it is a character. Where
they disagree the row is **not** proposed — it goes to a review file, on the
same discipline the genre pipeline uses (#52).

Phases, and `scan` writes no tags:

    python -m credits scan  ~/MusicLibrary      # -> out/credits-review.tsv
    python -m credits apply ~/MusicLibrary      # dry run
    python -m credits apply ~/MusicLibrary --write
"""

from __future__ import annotations

import collections
import csv
from pathlib import Path
from typing import TYPE_CHECKING, Final, Protocol

import typer

if TYPE_CHECKING:
    from collections.abc import Iterable

from mutagen import File as MutagenFile
from mutagen import FileType, MutagenError
from mutagen.id3 import ID3, TXXX
from mutagen.mp4 import MP4, MP4FreeForm

from gmuse_tools.genre.common import out_path, walk, write_tsv


class TagDict(Protocol):
    """What every concrete tag container gives us, which `mutagen.Tags` does not.

    `Tags` is a marker base class with no mapping methods, so annotating with it
    fails the type check. ID3, `VCommentDict` and `MP4Tags` all behave like a
    mapping of key to frame; this names that and nothing more.
    """

    def keys(self) -> Iterable[str]:
        """Every key in the container."""

    def __getitem__(self, key: str) -> object:
        """The frame stored under `key`."""

    def __delitem__(self, key: str) -> None:
        """Remove the frame stored under `key`."""


#: Every spelling of the multi-value artist-name field, bare (no `TXXX:` prefix).
ARTISTS: Final = frozenset({"artists"})
#: Every spelling of the per-artist MusicBrainz id field.
ARTIST_IDS: Final = frozenset({"musicbrainz artist id", "musicbrainz_artistid"})

#: The one spelling we write, per container. Picard's own, and the one lofty
#: maps to `ItemKey::TrackArtists` in all three formats.
CANONICAL: Final = {"id3": "Artists", "vorbis": "ARTISTS", "mp4": "ARTISTS"}

TRUTHY: Final = frozenset({"y", "yes", "1"})

#: `Collection/Artist/Album/track.ext` — the parts that name the album.
ALBUM_PARTS: Final = slice(1, 3)

#: Below this a "split" would not be a split.
PAIR: Final = 2


def kind_of(audio: FileType) -> str:
    """Which tag dialect this file speaks."""
    if isinstance(audio.tags, ID3):
        return "id3"
    if isinstance(audio, MP4):
        return "mp4"
    return "vorbis"


def values(frame: object) -> list[str]:
    """A tag frame's values, whatever container it came from."""
    if hasattr(frame, "text"):
        return [str(x) for x in frame.text]
    if isinstance(frame, list):
        return [x.decode("utf-8", "replace") if isinstance(x, bytes) else str(x) for x in frame]
    return [str(frame)]


def tag_keys(tags: TagDict) -> list[str]:
    """The container's keys.

    Not `for key in tags`: Vorbis's `VCommentDict` iterates as `(key, value)`
    pairs while ID3 and MP4 iterate as keys, so only `keys()` means the same
    thing in all three.
    """
    return list(tags.keys())


def spellings(tags: TagDict, names: frozenset[str]) -> list[tuple[str, list[str]]]:
    """Every key matching `names`, with its values.

    Deliberately returns *all* of them rather than the first. Taking the first
    is what made an earlier count wrong: the two id spellings on a single file
    held one id and two ids respectively, and which one you saw depended on
    dict order.
    """
    out: list[tuple[str, list[str]]] = []
    for key in tag_keys(tags):
        if key.split(":")[-1].lower() in names:
            vals = [v for v in values(tags[key]) if v.strip()]
            if vals:
                out.append((key, vals))
    return out


def widest(found: list[tuple[str, list[str]]]) -> list[str]:
    """The longest true multi-value list across every spelling, or []."""
    best: list[str] = []
    for _key, vals in found:
        if len(vals) > len(best):
            best = vals
    return best if len(best) > 1 else []


def joined(found: list[tuple[str, list[str]]]) -> str | None:
    """A single value containing `/`, if that is all we have."""
    for _key, vals in found:
        if len(vals) == 1 and "/" in vals[0]:
            return vals[0]
    return None


def decide(tags: TagDict) -> tuple[list[str], str, str]:
    """Work out this file's artist list.

    Returns `(artists, verdict, note)`. Verdicts:

    - ``list``      — a real multi-value list already exists; adopt it
    - ``split``     — slash-joined, and the id count agrees; splitting is safe
    - ``ambiguous`` — slash-joined, and the id count does not agree. **Not
      proposed.** `AC/DC` (2 parts, 1 id) and `Janet/Q-Tip/Joni Mitchell`
      (3 parts, 1 id) both land here, and they want opposite answers.
    - ``single``    — nothing to do
    """
    names = spellings(tags, ARTISTS)
    ids = spellings(tags, ARTIST_IDS)

    real = widest(names)
    if real:
        return real, "list", ""

    text = joined(names)
    if text is None:
        return [], "single", ""

    parts = [p.strip() for p in text.split("/") if p.strip()]
    # An id field may itself be slash-joined; count its parts the same way.
    id_counts = {len(v) if len(v) > 1 else len([x for x in v[0].split("/") if x.strip()]) for _k, v in ids}
    if not id_counts:
        return parts, "ambiguous", f"{len(parts)} names, no ids to corroborate"
    if id_counts == {len(parts)}:
        return parts, "split", f"{len(parts)} names, {len(parts)} ids"
    return parts, "ambiguous", f"{len(parts)} names vs {sorted(id_counts)} ids"


def id_parts(vals: list[str]) -> tuple[str, ...]:
    """One id per element, whether the field was a list or slash-joined."""
    out: list[str] = []
    for v in vals:
        out.extend(x.strip() for x in v.split("/") if x.strip())
    return tuple(out)


def conflicts(tags: TagDict) -> str:
    """Describe two spellings of the id field that disagree.

    Reports the **values**, not their counts. An earlier version printed
    `musicbrainz_artistid=1; MusicBrainz Artist Id=1` for a Beyonce track whose
    two spellings held two *different* ids — a row that flagged a real problem
    and said nothing about it.

    Two kinds, and they want opposite responses:

    - ``subset``   — one spelling holds the lead artist, the other the full
      credit. Not a contradiction, just an older tag written before the
      features were known. `decide()` already takes the wider one.
    - ``different`` — the spellings name *different artists*. On this library
      that is a **stale id**: MusicBrainz merged one away and only the older
      spelling still points at it. Fixing it needs a lookup, not a rule.
    """
    seen = {key: id_parts(vals) for key, vals in spellings(tags, ARTIST_IDS)}
    if len(set(seen.values())) <= 1:
        return ""
    sets = [set(v) for v in seen.values()]
    nested = any(a < b for a in sets for b in sets if a is not b)
    kind = "subset" if nested else "different"
    detail = " | ".join(f"{k.split(':')[-1]}={','.join(v)}" for k, v in seen.items())
    return f"{kind}: {detail}"


def scan(root: Path, out: Path) -> None:
    """Survey every file and write the proposal. Writes no tags."""
    rows: list[dict[str, object]] = []
    tally: collections.Counter[str] = collections.Counter()
    for path in walk(root):
        try:
            audio = MutagenFile(path)
        except (MutagenError, OSError):
            # A corrupt or unreadable file must not stop the survey.
            tally["unreadable"] += 1
            continue
        if audio is None or audio.tags is None:
            tally["no tags"] += 1
            continue
        artists, verdict, note = decide(audio.tags)
        tally[verdict] += 1
        clash = conflicts(audio.tags)
        if clash:
            tally[f"id spellings {clash.split(':')[0]}"] += 1
            if not note:
                note = "two spellings of the id field disagree; nothing to split"
        if verdict in {"list", "single"} and not clash:
            continue
        rel = path.relative_to(root)
        rows.append(
            {
                # Column order is for reading, not for the writer's convenience.
                # `path` is the longest field and the least scannable, so it goes
                # last: an editor showing a wide row would otherwise push the
                # reason this row exists off the right-hand edge, which is
                # exactly how a populated `note` came to look like an empty one.
                "apply": "y" if verdict == "split" else "",
                "verdict": verdict,
                "note": note,
                "artists": "; ".join(artists),
                "album": " — ".join(rel.parts[ALBUM_PARTS]),
                "id_conflict": clash,
                "path": str(rel),
            }
        )
    # **Written even when it is empty** — a survey that found nothing used to
    # leave the last one that found something standing, so a clean library went
    # on being described by a stale file listing work already done.
    write_tsv(out, rows)
    print(f"{sum(tally.values())} files")
    for k, v in sorted(tally.items()):
        print(f"  {v:6d}  {k}")
    proposed = sum(1 for r in rows if r["apply"] == "y")
    print(f"\n{len(rows)} row(s) -> {out}")
    print(f"  {proposed} proposed, {len(rows) - proposed} need a look (apply column left empty)")


def write_artists(path: Path, artists: list[str], *, upgrade_id3: bool) -> str:
    """Write the list as a real multi-value field. Returns what happened.

    **ID3v2.3 cannot hold a multi-value TXXX frame**, which is why the
    slash-joining exists in the first place — it is Picard working around the
    format, not a mistake. 612 files here are v2.3, and fixing them means
    rewriting the tag as v2.4. That is a real format change, so it needs
    `--upgrade-id3` rather than happening quietly.
    """
    audio = MutagenFile(path)
    if audio is None or audio.tags is None:
        return "unreadable"
    dialect = kind_of(audio)

    if dialect == "id3":
        tag = audio.tags
        if tag.version < (2, 4, 0):
            if not upgrade_id3:
                return "skipped (ID3v2.3)"
            tag.update_to_v24()
        for key, _ in spellings(tag, ARTISTS):
            del tag[key]
        tag.add(TXXX(encoding=3, desc=CANONICAL["id3"], text=artists))
        tag.save(path, v2_version=4)
        return "written"

    if dialect == "mp4":
        tag = audio.tags
        for key, _ in spellings(tag, ARTISTS):
            del tag[key]
        atom = f"----:com.apple.iTunes:{CANONICAL['mp4']}"
        tag[atom] = [MP4FreeForm(a.encode()) for a in artists]
        audio.save()
        return "written"

    tag = audio.tags
    for key, _ in spellings(tag, ARTISTS):
        del tag[key]
    tag[CANONICAL["vorbis"]] = artists
    audio.save()
    return "written"


def snapshot_row(root: Path, path: Path, becomes: list[str]) -> dict[str, object] | None:
    """What this file's artist fields hold now, and what they are about to become.

    `restore` needs only `was_value`. `becomes` is here for the reader: without
    it the file is an opaque list of strings with no indication of what happened
    to them, which is a poor thing to hand someone whose undo depends on it.
    Reading a row left to right should say *this field held this, and I made it
    that* — so the snapshot doubles as the record of what the apply did.
    """
    try:
        audio = MutagenFile(path)
    except (MutagenError, OSError):
        return None
    if audio is None or audio.tags is None:
        return None
    found = spellings(audio.tags, ARTISTS)
    return {
        "path": str(path.relative_to(root)),
        "was_field": " | ".join(k for k, _ in found),
        "was_value": " | ".join("; ".join(v) for _, v in found),
        "became": "; ".join(becomes),
    }


def put_back(path: Path, keys: list[str], vals: list[str]) -> str:
    """Rewrite one file's artist fields exactly as the snapshot recorded them."""
    try:
        audio = MutagenFile(path)
    except (MutagenError, OSError):
        return "unreadable"
    if audio is None or audio.tags is None:
        return "unreadable"
    tag = audio.tags
    for key, _ in spellings(tag, ARTISTS):
        del tag[key]
    for key, joined_values in zip(keys, vals, strict=False):
        parts = [x for x in joined_values.split("; ") if x]
        if kind_of(audio) == "id3":
            tag.add(TXXX(encoding=3, desc=key.split(":")[-1], text=parts))
        else:
            tag[key] = parts
    audio.save()
    return "restored"


def restore(root: Path, snap: Path, *, write: bool) -> None:
    """Put the snapshot's artist fields back, key spelling and all."""
    if not snap.exists():
        print(f"no {snap.name}")
        return
    tally: collections.Counter[str] = collections.Counter()
    with snap.open(encoding="utf-8") as handle:
        for row in csv.DictReader(handle, delimiter="\t"):
            if not write:
                tally["would restore"] += 1
                continue
            keys = [k for k in row["was_field"].split(" | ") if k]
            vals: list[str] = row["was_value"].split(" | ") if row["was_value"] else []
            tally[put_back(root / row["path"], keys, vals)] += 1
    for k, v in sorted(tally.items()):
        print(f"  {v:6d}  {k}")
    if not write:
        print("dry run — pass --write to actually restore")


def apply(root: Path, review: Path, *, write: bool, upgrade_id3: bool) -> None:
    """Apply the rows marked `y`. Dry run unless `--write`.

    A snapshot is taken **before** the first write, because §1's 2026-09-01
    amendment admits tag editing on the condition that an edit is undoable and
    this is the only record of what the fields held.
    """
    if not review.exists():
        print(f"no {review.name} — run `scan` first")
        return
    with review.open(encoding="utf-8") as handle:
        rows = [r for r in csv.DictReader(handle, delimiter="\t") if r["apply"].strip().lower() in TRUTHY]

    if write:
        snap = out_path("credits-snapshot.tsv")
        taken = [
            r
            for r in (
                snapshot_row(root, root / row["path"], [a.strip() for a in row["artists"].split(";") if a.strip()])
                for row in rows
            )
            if r
        ]
        if taken:
            write_tsv(snap, taken)
            print(f"snapshot of {len(taken)} file(s) -> {snap}")

    tally: collections.Counter[str] = collections.Counter()
    for row in rows:
        artists = [a.strip() for a in row["artists"].split(";") if a.strip()]
        if len(artists) < PAIR:
            tally["nothing to split"] += 1
            continue
        path = root / row["path"]
        if not write:
            print(f"  would set {row['path']}\n      -> {artists}")
            tally["would write"] += 1
            continue
        tally[write_artists(path, artists, upgrade_id3=upgrade_id3)] += 1

    for k, v in sorted(tally.items()):
        print(f"  {v:6d}  {k}")
    if not write:
        print("dry run — pass --write to actually edit")


LIB = typer.Argument(Path("~/MusicLibrary"), help="library root")
WRITE = typer.Option(default=False, help="actually edit files; without this, a dry run")


def register(app: typer.Typer) -> None:
    """Attach the credits pass. A group of three, because it is three phases."""
    credits_app = typer.Typer(no_args_is_help=True, help=__doc__)
    app.add_typer(credits_app, name="credits")

    @credits_app.command("scan")
    def scan_cmd(root: Path = LIB) -> None:
        """Survey and propose. Writes no tags."""
        scan(root.expanduser(), out_path("credits-review.tsv"))

    @credits_app.command("apply")
    def apply_cmd(
        root: Path = LIB,
        *,
        write: bool = WRITE,
        upgrade_id3: bool = typer.Option(default=False, help="rewrite ID3v2.3 tags as v2.4"),
    ) -> None:
        """Write the rows marked `y` in the review."""
        apply(
            root.expanduser(),
            out_path("credits-review.tsv"),
            write=write,
            upgrade_id3=upgrade_id3,
        )

    @credits_app.command("restore")
    def restore_cmd(root: Path = LIB, *, write: bool = WRITE) -> None:
        """Undo an apply, from the snapshot it took."""
        restore(root.expanduser(), out_path("credits-snapshot.tsv"), write=write)
