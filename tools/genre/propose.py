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
import json
import sqlite3
from dataclasses import dataclass, field
from pathlib import Path
from typing import Final

from genre.common import album_key, fix_case, tags_of, walk, write_tsv

#: The terms this library treats as broad — Discogs' 15 Genres, **after**
#: `RENAME`. Both spellings are in here on purpose: a source says
#: `Folk, World, & Country` and the library says `Folk`, and if only the
#: source's spelling were listed then a value already renamed would not be
#: recognised as broad. That bug put `Folk` in twice — once from `IMPLIES`,
#: once from the term itself — on every folk record in the library.
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
        "Funk",
        "Folk, World, & Country",
        "Folk",
        "Reggae",
        "Latin",
        "Stage & Screen",
        "Non-Music",
        "Brass & Military",
        "Children's",
        # Not Discogs genres, but this library's own broad-first vocabulary uses
        # them and they must not be given a second broad term on top.
        "Country",
        "R&B",
        "Americana",
    }
)
#: This library's spellings, where they differ from Discogs'.
RENAME: Final[dict[str, str]] = {
    "Funk / Soul": "Funk",
    "Folk, World, & Country": "Folk",
    # A source term that is a category, not a genre.
    "General Country": "Country",
}

#: Terms that are not genres at all. `Other` says nothing, and an **artist name
#: in a genre field** is a tagger's mistake — dropped by rule rather than by
#: list, since the rule generalises and a list of artist names never would.
DROP: Final[frozenset[str]] = frozenset({"other", "unknown", "misc", "miscellaneous", "none"})
#: Bare values already in the library that are styles, not genres. These are the
#: files that need a broad term *added* rather than replaced.
IMPLIES: Final[dict[str, str]] = {
    # ── rock ──────────────────────────────────────────────────────────────
    "Alternative Rock": "Rock",
    "Alternative": "Rock",
    "Hard Rock": "Rock",
    "Grunge": "Rock",
    "Thrash Metal": "Rock",
    "Doom Metal": "Rock",
    "Heavy Metal": "Rock",
    "Indie": "Rock",
    "Punk": "Rock",
    "Post-punk": "Rock",
    "Art Rock": "Rock",
    "Psychedelic Rock": "Rock",
    # ── folk ──────────────────────────────────────────────────────────────
    "Bluegrass": "Folk",
    "Anti-folk": "Folk",
    "Indie Folk": "Folk",
    "Celtic": "Folk",
    "Singer-songwriter": "Folk",
    "Folk Rock": "Folk",
    # ── country ───────────────────────────────────────────────────────────
    "Alternative Country": "Country",
    "Country Rock": "Country",
    # ── pop ───────────────────────────────────────────────────────────────
    # New Age is deliberately **not** Electronic. Discogs files Enya there, but
    # much of it is acoustic — and her own self-titled record already reads
    # `Pop; Folk; New Age`, so Pop is what this library already says.
    "New Age": "Pop",
    "Christmas Music": "Pop",
    "French Pop": "Pop",
    "Ballad": "Pop",
    "Synth-pop": "Pop",
    "Indie Pop": "Pop",
    # ── the rest ──────────────────────────────────────────────────────────
    "Ska": "Reggae",
    "Dub": "Reggae",
    "Contemporary Jazz": "Jazz",
    "Jazz-rock": "Jazz",
    "Free Jazz": "Jazz",
    "Contemporary R&B": "R&B",
    "Neo Soul": "R&B",
    "Gangsta": "Hip Hop",
    "Gangsta Rap": "Hip Hop",
    "Alternative Dance": "Electronic",
    "Dance": "Electronic",
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


def compose(terms: list[str], *, reorder: bool, artist: str = "") -> str:
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
        artist: The album artist, so a genre that merely repeats it is dropped.

    Returns:
        The composed value, e.g. ``Electronic; Techno; Ambient``.
    """
    out: list[str] = []
    seen: set[str] = set()
    for raw in terms:
        term = RENAME.get(fix_case(raw), fix_case(raw))
        # `Other` says nothing, and a genre equal to the album artist is a
        # tagger's mistake — `Other; Tom Waits` was in this library.
        if not term or term.lower() in DROP or term.lower() == artist.lower():
            continue
        if term.lower() not in seen:
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


def searched_terms(found: dict[str, object] | None) -> tuple[list[str], str]:
    """Terms from the name-search fallback — **only when the tracklist agrees**.

    Picard's discipline, and the reason is that a name search returns plausible
    wrong answers: searching for *Let It Be* offers a Korean pressing before the
    deluxe edition this library holds. An `exact` tracklist match is the cheapest
    reliable signal that the record is the same one, and it is the only verdict
    used unattended. Anything less is written to `uncertain.tsv` for a person to
    look at, one album at a time.
    """
    if not found or found.get("verdict") != "exact":
        return [], "current-only"
    terms = _strings(found, "discogs_genres") + _strings(found, "discogs_styles")
    if not terms:
        return [], "current-only"
    return terms, "discogs-search (tracklist matches exactly)"


def write_uncertain(
    path: Path,
    albums: dict[tuple[str, str], Album],
    searched: dict[tuple[str, str], dict[str, object]],
) -> int:
    """Write the candidates that need a person, and say how many there were."""
    rows = [
        {
            "artist": artist,
            "album": album,
            "files": held.files,
            "current": " | ".join(sorted(held.genres)),
            "verdict": found.get("verdict", "?"),
            "shared": found.get("share", 0),
            "our_tracks": found.get("ours", 0),
            "their_tracks": found.get("theirs", 0),
            "matched": found.get("matched", ""),
            "year": found.get("year", ""),
            "candidate": "; ".join(_strings(found, "discogs_genres") + _strings(found, "discogs_styles")),
        }
        for (artist, album), held in sorted(albums.items())
        if (found := searched.get((artist, album))) and found.get("verdict") != "exact"
    ]
    if not rows:
        path.unlink(missing_ok=True)
        return 0
    write_tsv(path, rows)
    return len(rows)


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


def row_for(
    artist: str,
    album_name: str,
    album: Album,
    lookup: dict[str, dict[str, object]],
    searched: dict[tuple[str, str], dict[str, object]],
) -> dict[str, object]:
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
    if not terms:
        # Nothing a link reached; fall back to what a name search found.
        terms, source = searched_terms(searched.get((artist, album_name)))
    # A value already in the convention's `; ` form has been through a human.
    human = len(current_values) == 1 and ";" in current_values[0] and source == "current-only"
    terms = current_terms if (human or not terms) else merge(terms, current_terms)

    proposed = compose(terms, reorder=not human, artist=artist)
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
    searched = {(a, b): json.loads(p) for a, b, p in con.execute("SELECT artist, album, payload FROM search")}

    surveyed = survey(Path(args.root).expanduser())
    rows = [row_for(artist, name, album, lookup, searched) for (artist, name), album in sorted(surveyed.items())]

    out_path = here / "review.tsv"
    write_tsv(out_path, rows)

    uncertain = write_uncertain(here / "uncertain.tsv", surveyed, searched)
    changing = sum(1 for r in rows if r["apply"] == "y")
    by_source: dict[str, int] = {}
    for row in rows:
        by_source[str(row["source"])] = by_source.get(str(row["source"]), 0) + 1
    print(f"{len(rows)} albums -> {out_path}")
    print(f"  {changing} would change, {len(rows) - changing} already correct")
    print(f"  sources: {by_source}")
    if uncertain:
        print(
            f"\n  {uncertain} album(s) matched a Discogs release whose tracklist differs.\n"
            f"  They are in uncertain.tsv and are **not** proposed — look at each one."
        )
    print("\nEdit the `proposed` column, clear `apply` to skip a row, then run apply.py.")


if __name__ == "__main__":
    main()
