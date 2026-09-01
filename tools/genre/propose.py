#!/usr/bin/env python3
"""Turn the cache and the current tags into a review file. Writes no tags.

The convention is compound, ``; ``-separated, **broad term first**, so
``genre="*electronic*"`` finds every electronic record. That is the whole
reason this exists: half the library was bare single words.

**No cap on the number of terms.** Discogs assigns styles deliberately, the
filter language matches substrings so more terms strictly improve recall, and
length is a *rendering* problem the format language already solves with
``{genre:.40}``. Browsing wants the broad term as its level, not the styles.
"""

from __future__ import annotations

import argparse
import csv
import json
import sqlite3
from dataclasses import dataclass, field
from pathlib import Path
from typing import Final

from genre.common import album_key, fix_case, tags_of, walk

#: Discogs' broad Genres — the vocabulary the convention's first term comes from.
BROAD: Final[frozenset[str]] = frozenset(
    {
        "Electronic",
        "Rock",
        "Pop",
        "Jazz",
        "Blues",
        "Classical",
        "Hip Hop",
        "Funk / Soul",
        "Folk, World, & Country",
        "Reggae",
        "Latin",
        "Stage & Screen",
        "Non-Music",
        "Brass & Military",
        "Children's",
    }
)
#: This library's spellings, where they differ from Discogs'.
RENAME: Final[dict[str, str]] = {"Funk / Soul": "Funk", "Folk, World, & Country": "Folk"}
#: Bare values already in the library that are styles, not genres. These are the
#: files that need a broad term *added* rather than replaced.
IMPLIES: Final[dict[str, str]] = {
    "House": "Electronic",
    "Techno": "Electronic",
    "Breakbeat": "Electronic",
    "IDM": "Electronic",
    "Leftfield": "Electronic",
    "Downtempo": "Electronic",
    "Dubstep": "Electronic",
    "Breaks": "Electronic",
    "Tech House": "Electronic",
    "Big Beat": "Electronic",
    "Trance": "Electronic",
    "Electronica": "Electronic",
    "Drum & Bass": "Electronic",
    "Melodic House & Techno": "Electronic",
    "Ambient": "Electronic",
    "Minimal": "Electronic",
}
COMPILATION: Final = "compilation — per track, skipped"


@dataclass
class Album:
    """What one album looks like across its files."""

    files: int = 0
    genres: set[str] = field(default_factory=set)
    artists: set[str] = field(default_factory=set)
    mbid: str = ""


def split_terms(value: str) -> list[str]:
    """Parse any of the three dialects in the library into a term list.

    The library holds ``A; B`` (the convention), ``A/B/C`` (a Discogs export)
    and bare single words, so the separator is whichever is present.
    """
    if not value:
        return []
    separator = ";" if ";" in value else ("/" if "/" in value else None)
    parts = value.split(separator) if separator else [value]
    return [fix_case(p.strip()) for p in parts if p.strip()]


def compose(terms: list[str], *, reorder: bool) -> str:
    """Join terms into one value, de-duplicated, with a broad term leading.

    `reorder` is the whole subtlety. The slash form is **machine output sorted
    alphabetically**, so its order carries no meaning and the broad term should
    lead. The ``; `` form is a **human's judgement**, and an earlier version
    reordered the curated ``R&B; Hip Hop; Funk`` into ``Hip Hop; R&B; Funk`` to
    satisfy a taxonomy that does not agree with it. Same operation, opposite
    verdict, decided by where the value came from.

    Args:
        terms: Genre terms, in the order the source gave them.
        reorder: Whether the order is machine-made and may be rearranged.

    Returns:
        The composed value, e.g. ``Electronic; Techno; Ambient``.
    """
    out: list[str] = []
    seen: set[str] = set()
    for raw in terms:
        term = RENAME.get(fix_case(raw), fix_case(raw))
        if term and term.lower() not in seen:
            seen.add(term.lower())
            out.append(term)

    broad = [t for t in out if t in BROAD]
    if not broad:
        for term in out:
            if implied := IMPLIES.get(term):
                out.insert(0, implied)
                break
    elif reorder:
        out = [broad[0], *(t for t in out if t != broad[0])]
    return "; ".join(out)


def survey(root: Path) -> dict[tuple[str, str], Album]:
    """Gather every album under `root`, with its genres, artists and release id."""
    albums: dict[tuple[str, str], Album] = {}
    for path in walk(root):
        tags = tags_of(path)
        album = albums.setdefault(album_key(tags), Album())
        album.files += 1
        album.artists.add((tags.get("artist") or "").strip())
        if genre := (tags.get("genre") or "").strip():
            album.genres.add(genre)
        album.mbid = album.mbid or (tags.get("musicbrainz album id") or "").strip()
    return albums


def _strings(payload: dict[str, object], key: str) -> list[str]:
    """Read a list-of-strings field out of a cached payload, defensively.

    The cache holds decoded JSON, so every value is `object` until checked. An
    earlier version fed `.get(...) or []` straight to `list()`, which is only
    sound if the value really is a list — and says nothing about what is in it.
    """
    value = payload.get(key)
    return [str(item) for item in value] if isinstance(value, list) else []


def source_terms(payload: dict[str, object]) -> tuple[list[str], str]:
    """Pick the best available source for an album, and say which it was.

    Discogs first: its Genre/Style split *is* this convention. MusicBrainz
    second, and its genres are ranked so the order is meaningful.
    """
    discogs = _strings(payload, "discogs_genres") + _strings(payload, "discogs_styles")
    if discogs:
        return discogs, "discogs"
    ranked = payload.get("mb_genres")
    mb: list[str] = []
    if isinstance(ranked, list):
        # MusicBrainz genres arrive as `(name, count)` pairs, already ranked.
        mb = [str(entry[0]) for entry in ranked if isinstance(entry, (list, tuple)) and entry]
    if mb:
        return mb, "musicbrainz"
    return [], "current-only"


def merge(found: list[str], current: list[str]) -> list[str]:
    """Union a source's terms with the ones already on the files.

    **Never replace.** Discogs dropped ``Leftfield`` from Wagon Christ's *Tally
    Ho!* and MusicBrainz replaced ``Minimal; Techno`` with ``House; Leftfield``
    on a minimal techno record. A source knows things the tags do not; it does
    not know better than them. More terms strictly improve recall, which is the
    same reason there is no cap.

    Args:
        found: What the source said, in its own order.
        current: What the files already say.

    Returns:
        The source's terms, then anything the files add.
    """
    return found + [t for t in current if t not in found]


def row_for(artist: str, album_name: str, album: Album, lookup: dict[str, dict[str, object]]) -> dict[str, object]:
    """Build one review row: what the album says now, and what it should say."""
    current_values = sorted(album.genres)
    current_terms: list[str] = []
    for value in current_values:
        current_terms.extend(t for t in split_terms(value) if t not in current_terms)

    # A compilation's tracks legitimately differ, and unioning them would flatten
    # every track of `YouTube Sessions` to one value — replacing four correct
    # per-track genres with one wrong shared one.
    #
    # Tested on the **albumartist** alone. An earlier version also treated any
    # album whose `artist` tags varied as a compilation, and that fires on a
    # guest credit: `Amon Tobin feat. MC Decimal R` on one track of *Out From
    # Out Where* left the whole album skipped and still reading bare
    # `Breakbeat`. Six albums were wrongly exempted that way. `Various Artists`
    # is the explicit marker this library uses, and it is reserved for a
    # compilation owned in full.
    if artist == "Various Artists":
        return {
            "artist": artist,
            "album": album_name,
            "files": album.files,
            "current": " | ".join(current_values),
            "proposed": "",
            "source": COMPILATION,
            "mbid": album.mbid,
            "apply": "",
        }

    terms, source = source_terms(lookup.get(album.mbid, {}))
    # A value already in the convention's `; ` form has been through a human.
    human = len(current_values) == 1 and ";" in current_values[0] and source == "current-only"
    terms = current_terms if (human or not terms) else merge(terms, current_terms)

    proposed = compose(terms, reorder=not human)
    current = current_values[0] if len(current_values) == 1 else " | ".join(current_values)
    return {
        "artist": artist,
        "album": album_name,
        "files": album.files,
        "current": current,
        "proposed": proposed,
        "source": source,
        "mbid": album.mbid,
        "apply": "" if proposed == current else "y",
    }


def main() -> None:
    """Write the review file and summarise what it proposes."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", default="~/MusicLibrary", help="library root to scan")
    args = parser.parse_args()

    here = Path(__file__).parent
    con = sqlite3.connect(here / "genre-cache.sqlite")
    lookup = {mbid: json.loads(payload) for mbid, payload in con.execute("SELECT mbid, payload FROM lookup")}

    rows = [
        row_for(artist, name, album, lookup)
        for (artist, name), album in sorted(survey(Path(args.root).expanduser()).items())
    ]

    out_path = here / "review.tsv"
    with out_path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]), delimiter="\t")
        writer.writeheader()
        writer.writerows(rows)

    changing = sum(1 for r in rows if r["apply"] == "y")
    by_source: dict[str, int] = {}
    for row in rows:
        by_source[str(row["source"])] = by_source.get(str(row["source"]), 0) + 1
    print(f"{len(rows)} albums -> {out_path}")
    print(f"  {changing} would change, {len(rows) - changing} already correct")
    print(f"  sources: {by_source}")
    print("\nEdit the `proposed` column, clear `apply` to skip a row, then run apply.py.")


if __name__ == "__main__":
    main()
