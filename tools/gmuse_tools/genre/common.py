"""Shared helpers for the library tools: tag reading, album identity, credentials.

None of this is gmuse. The design brief puts tag editing out of scope, and the
pattern these follow is the one rsgain and bliss already set — an external tool
writes, gmuse reads.
"""

from __future__ import annotations

import csv
import json
import os
import re
import subprocess
from pathlib import Path
from typing import Final

from gmuse_tools.text import fold_accents

AUDIO: Final = (".mp3", ".flac", ".ogg", ".opus", ".m4a", ".wav", ".aac", ".wma")

#: Terms whose casing no source gets right, applied to the final term.
CASE: Final[dict[str, str]] = {
    "Idm": "IDM",
    "Edm": "EDM",
    "Ebm": "EBM",
    "Uk Garage": "UK Garage",
    "Uk Bass": "UK Bass",
    "Uk Funky": "UK Funky",
    "Dj": "DJ",
    "R&b": "R&B",
    "Rnb": "R&B",
    "Ukg": "UKG",
    "Nyc": "NYC",
    # Discogs writes "Drum n Bass"; capitalising each word gives "Drum N Bass",
    # which is nobody's spelling. One canonical form, because the filter
    # language matches substrings and two spellings would split the results.
    "Drum N Bass": "Drum & Bass",
    "Drum And Bass": "Drum & Bass",
    "Dnb": "Drum & Bass",
    "Psy-trance": "Psytrance",
    "Nu-disco": "Nu Disco",
}

#: Keychain service names, by environment-variable name. See `secret`.
KEYCHAIN: Final[dict[str, str]] = {
    "DISCOGS_TOKEN": "gmuse-discogs",
    "ACOUSTID_KEY": "gmuse-acoustid",
}


#: Where generated files go: review tables, snapshots, and the API cache.
#:
#: Beside the code they were indistinguishable from it — six .tsv files mixed
#: in with the modules that write them. One folder, and every name prefixed by
#: the pass that owns it, so `ls out/` reads as a list of results.
#: **Three parents, not two**, and the count is load-bearing: this file moved
#: down a level when the scripts became a package, which silently pointed `out`
#: at `gmuse_tools/out` and orphaned every result already written. Anchored to
#: the package's own parent so it cannot drift again with the next move.
OUT: Final = Path(__file__).resolve().parents[2] / "out"


def out_path(name: str) -> Path:
    """Path to a generated file, creating the folder on first use."""
    OUT.mkdir(exist_ok=True)
    return OUT / name


def tags_of(path: Path) -> dict[str, str]:
    """Read every tag on a file, lowercased.

    Reads ``stream_tags`` as well as ``format_tags``. Vorbis comments live on
    the *stream*, so reading only the latter reports every Opus file as
    completely untagged — which looked like a real finding until it turned out
    to be the same five files already flagged as missing ReplayGain.

    Args:
        path: The audio file to read.

    Returns:
        Tag name (lowercased) to value. Empty if the file cannot be read.
    """
    proc = subprocess.run(
        ["ffprobe", "-v", "quiet", "-show_entries", "format_tags:stream_tags", "-of", "json", str(path)],
        capture_output=True,
        text=True,
        check=False,
    )
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError:
        return {}
    tags = {k.lower(): v for k, v in (data.get("format", {}).get("tags") or {}).items()}
    for stream in data.get("streams") or []:
        for key, value in (stream.get("tags") or {}).items():
            tags.setdefault(key.lower(), value)
    # Vorbis spells two of these differently from ID3.
    if "albumartist" in tags:
        tags.setdefault("album_artist", tags["albumartist"])
    if "tracknumber" in tags:
        tags.setdefault("track", tags["tracknumber"])
    return tags


#: Directories under the library root that are not the library.
#:
#: `.gmuse` is the log. `.departed` is music somebody retired — kept so a
#: mistake is reversible (`storage-layout.md` §6) and **not** a place any pass
#: should propose work on: an audit that lists it asks you to identify records
#: you removed on purpose, and a retag that touches it edits the copy you kept
#: precisely so it would stay as it was.
NOT_THE_LIBRARY: Final = (".gmuse", ".departed")


def walk(root: Path) -> list[Path]:
    """Return every audio file under `root`, sorted, skipping what is not the library."""
    return sorted(
        p for p in root.rglob("*") if p.suffix.lower() in AUDIO and not any(part in NOT_THE_LIBRARY for part in p.parts)
    )


#: What a MusicBrainz release id is called, by tag format.
#:
#: **ID3 spells it with spaces and Vorbis with underscores**, and reading only
#: the first meant every FLAC album reported no release id at all — so the
#: MusicBrainz link was never followed and each one fell through to a Discogs
#: *name* search. `AFX/Chosen Lords` and `AFX/Analogue Bubblebath` came back
#: `no hit` and got `Electronic; Electronica` off their existing tags, while
#: MusicBrainz held a Discogs link for both all along.
#:
#: The same shape as `ingest`'s FLAC gap: a rule written against one
#: serialisation, silently answering a narrower question on the other.
ALBUM_MBID: Final = ("musicbrainz album id", "musicbrainz_albumid")


def album_mbid(tags: dict[str, str]) -> str:
    """The MusicBrainz release id a file carries, under whichever name."""
    for key in ALBUM_MBID:
        if found := (tags.get(key) or "").strip():
            return found
    return ""


def album_key(tags: dict[str, str]) -> tuple[str, str]:
    """Identify an album by its tags, which is what the library tree files by.

    Keyed on tags rather than on the folder deliberately: comparing folder
    *names* is how the same UMFANG record was imported twice — once as ``_Ok_``
    and once with the fullwidth quotes the cross-platform naming rule requires
    — when the tags said plainly they were one album.

    Args:
        tags: The file's tags, as returned by `tags_of`.

    Returns:
        The album artist and album title.
    """
    artist = (tags.get("album_artist") or tags.get("artist") or "?").strip()
    return artist, (tags.get("album") or "?").strip()


def fix_case(term: str) -> str:
    """Normalise a genre term's capitalisation, then apply the exceptions in `CASE`.

    **The table is consulted per word as well as whole.** It was whole only,
    and most of its entries are words — so `R&b` was corrected to `R&B` when it
    was the entire term and never inside one, and this library carried
    `Contemporary R&B` on 125 files and `Contemporary R&b` on 45. The bare
    `R&B` was right the whole time, which is what kept it hidden.

    Whole first, because two entries mean something only as a phrase: `Uk
    Garage` needs the pair, and `Drum N Bass` becomes `Drum & Bass` rather than
    three words separately corrected.

    **`capitalize()` lowercasing after a hyphen is deliberate, not a gap.**
    This library spells 27 terms that way — `Hip-hop`, `Post-punk`, `Lo-fi`,
    `Avant-garde` — so it is the convention, and the one deviation
    (`Non-Music`) is what wants fixing rather than the rule.
    """
    titled = " ".join(word.capitalize() if not word.isupper() else word for word in term.split())
    if titled in CASE:
        return CASE[titled]
    return " ".join(CASE.get(word, word) for word in titled.split())


def _from_keychain(service: str) -> str | None:
    """Read a generic password from the macOS Keychain, or None if absent."""
    proc = subprocess.run(
        ["security", "find-generic-password", "-a", os.environ.get("USER", ""), "-s", service, "-w"],
        capture_output=True,
        text=True,
        check=False,
    )
    return proc.stdout.strip() if proc.returncode == 0 and proc.stdout.strip() else None


def _from_envfile(path: Path, name: str) -> str | None:
    """Read one `NAME=value` line from a dotenv-style file, or None if absent."""
    if not path.exists():
        return None
    for raw in path.read_text(encoding="utf-8").splitlines():
        line = raw.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        if key.strip() == name:
            return value.strip().strip("'\"")
    return None


def secret(name: str) -> str | None:
    """Read a credential from the least leaky place that has it.

    Three sources, in order, because they fail differently:

    1. The environment, for CI and a one-off ``NAME=... ./script``.
    2. The macOS Keychain, which is the recommended place — nothing on disk to
       commit by accident and nothing in shell history::

           security add-generic-password -a "$USER" -s gmuse-discogs -w

    3. A ``.env`` beside the scripts. Last, because it is the one that gets
       committed by mistake; the directory carries a ``.gitignore`` for it.

    Args:
        name: The environment-variable name, e.g. ``DISCOGS_TOKEN``.

    Returns:
        The credential, or None if no source has it.
    """
    if value := os.environ.get(name):
        return value.strip()
    service = KEYCHAIN.get(name)
    if service and (found := _from_keychain(service)):
        return found
    return _from_envfile(Path(__file__).parent / ".env", name)


#: A featured-artist suffix, which is a *place* two sources disagree about
#: rather than a difference between two records.
FEATURING: Final = re.compile(r"\s*[\(\[]\s*(?:feat|ft|featuring)\b[^)\]]*[\)\]]\s*$", re.IGNORECASE)


def compare_key(title: str) -> str:
    """A track title reduced to what two sources can be expected to agree on.

    Case, punctuation and spacing all differ between a tag and a Discogs
    tracklist, and none of that
    difference means the records are different — a curly apostrophe against a
    straight one is the commonest case. Everything that is not a letter or a
    digit goes.

    **A trailing `(feat. …)` goes too**, because MusicBrainz keeps featured
    artists in the *artist credit* and a downloaded tag usually keeps them in
    the title. That is a documented convention, not noise: *Malibu* agreed on
    11 of 16 titles and every one of the five that differed did so only this
    way, so a certain match scored 69% and was refused.

    Only a **trailing** parenthesis, and only that word. `Water Fall
    (Interluuube)` and `Sleepless (Stephan Bodzin Remix)` keep their suffixes,
    which is the line worth holding: a remix is a different recording and a
    guest is not.

    **Accents are folded, not kept** — #296. `\u00e9`.isalnum() is `True`, so
    `Electric Caf\u00e9` and `Electric Cafe` used to key differently, and
    [`tracklist_match`] below intersects *sets* of these — no threshold, so one
    accent is a hard miss worth a whole track. Measured on the Kraftwerk 2009
    remasters: seven tracks, every duration within 2s, one accented character,
    and the verdict came back `named (86%)` rather than `exact`. `exact` is the
    only verdict a tool acts on unattended, so on an unpinned run that is the
    difference between accepting a correct release and refusing it. 53 titles
    across 33 album folders in this library key differently for this reason.

    **The alphabet policy stays `str.isalnum`**, which is why this does not
    just call `identify.folded`: that keeps `[a-z0-9]`, and a title in a
    non-Latin script would fold to the empty string — whereupon every such
    title keys alike and the set intersection calls them the same track. No
    title here is entirely non-Latin today; the guard is for the library this
    one is growing into.
    """
    return "".join(c for c in fold_accents(FEATURING.sub("", title)).lower() if c.isalnum())


#: Below this share of matching titles a candidate is not worth offering.
CLOSE_ENOUGH: Final = 0.8


def tracklist_match(ours: list[str], theirs: list[str]) -> tuple[str, float]:
    """How well two tracklists agree, and a one-word verdict.

    **`exact` is the only verdict a tool should act on unattended.** Picard's
    rule, and the reason is that a name search returns plausible wrong answers:
    searching for *Let It Be* offers a Korean pressing before the deluxe
    edition. A different tracklist is the cheapest reliable signal that the
    match is a different record.

    Args:
        ours: Track titles from the library.
        theirs: Track titles from the candidate release.

    Returns:
        A verdict — `exact`, `close` or `weak` — and the fraction shared.
    """
    if not ours or not theirs:
        return "weak", 0.0
    a = {compare_key(t) for t in ours}
    b = {compare_key(t) for t in theirs}
    shared = len(a & b) / max(len(a), len(b))
    if a == b and len(ours) == len(theirs):
        return "exact", 1.0
    return ("close" if shared >= CLOSE_ENOUGH else "weak"), shared


class TabInFieldError(ValueError):
    """A field held a tab or newline, which TSV cannot escape."""

    def __init__(self, name: str, field: str, value: object) -> None:
        """Name the file, the column, and the offending value."""
        super().__init__(f"{name}: {field}={value!r} holds a tab or newline")


def write_tsv(path: Path, rows: list[dict[str, object]], columns: list[str] | None = None) -> None:
    """Write rows as TSV, refusing rather than emitting a row that cannot be read back.

    **An empty result is an answer and gets written.** This used to need a row
    to know its own columns, so every caller guarded with `if rows:` and a pass
    that found nothing left the last one that found something standing. `tags
    survey` reported `0 file(s) -> tags-review.tsv` and did not touch the
    107-row file already there, so the folder went on claiming 107 files needed
    work months after they had all been done — and half of `tools/out` had gone
    stale the same way. Pass `columns` and a run with nothing to report writes
    a header and no rows, which is what "nothing to do" looks like.

    TSV has no escape mechanism: a tab inside a field silently shifts every
    column right of it. Downstream that is a *dropped* row rather than a wrong
    one — ``apply.py`` gates on a column that must read ``y`` — but a row that
    vanishes without saying so is the failure worth spending three lines on.

    This is the boundary, so the check belongs here and nothing below it needs
    to wonder. CSV would escape instead of refusing, at the cost of quoting the
    23-in-504 rows that hold a comma; these files are read by eye and by ``cut``,
    so plain columns win and the guard covers the hazard.
    """
    names = list(rows[0]) if rows else columns
    if names is None:
        # Nothing to write and no columns to write it under. Truncating is
        # still better than leaving yesterday's findings in place, because a
        # file nobody rewrote reads exactly like a file nobody has dealt with.
        path.write_text("")
        return
    for row in rows:
        for field, value in row.items():
            if "\t" in str(value) or "\n" in str(value):
                raise TabInFieldError(path.name, field, value)
    with path.open("w", newline="", encoding="utf-8") as handle:
        # `lineterminator` is not a default worth taking: csv writes CRLF, and a
        # CRLF TSV on this platform means the last column of every row ends in a
        # stray \r. `awk -F'\t' '$8=="y"'` then matches nothing, silently, which
        # is exactly the kind of wrong-but-plausible shell result the tab
        # separator was chosen to avoid.
        writer = csv.DictWriter(handle, fieldnames=names, delimiter="\t", lineterminator="\n")
        writer.writeheader()
        writer.writerows(rows)
