"""Shared helpers for the library tools: tag reading, album identity, credentials.

None of this is gmuse. The design brief puts tag editing out of scope, and the
pattern these follow is the one rsgain and bliss already set — an external tool
writes, gmuse reads.
"""

from __future__ import annotations

import csv
import json
import os
import subprocess
from pathlib import Path
from typing import Final

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
KEYCHAIN: Final[dict[str, str]] = {"DISCOGS_TOKEN": "gmuse-discogs"}


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


def walk(root: Path) -> list[Path]:
    """Return every audio file under `root`, sorted, skipping gmuse's own data directory."""
    return sorted(p for p in root.rglob("*") if p.suffix.lower() in AUDIO and ".gmuse" not in p.parts)


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
    """Normalise a genre term's capitalisation, then apply the exceptions in `CASE`."""
    titled = " ".join(word.capitalize() if not word.isupper() else word for word in term.split())
    return CASE.get(titled, titled)


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


def compare_key(title: str) -> str:
    """A track title reduced to what two sources can be expected to agree on.

    Case, punctuation and spacing all differ between a tag and a Discogs
    tracklist, and none of that
    difference means the records are different — a curly apostrophe against a
    straight one is the commonest case. Everything that is not a letter or a
    digit goes.
    """
    return "".join(c for c in title.lower() if c.isalnum())


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


def write_tsv(path: Path, rows: list[dict[str, object]]) -> None:
    """Write rows as TSV, refusing rather than emitting a row that cannot be read back.

    TSV has no escape mechanism: a tab inside a field silently shifts every
    column right of it. Downstream that is a *dropped* row rather than a wrong
    one — ``apply.py`` gates on a column that must read ``y`` — but a row that
    vanishes without saying so is the failure worth spending three lines on.

    This is the boundary, so the check belongs here and nothing below it needs
    to wonder. CSV would escape instead of refusing, at the cost of quoting the
    23-in-504 rows that hold a comma; these files are read by eye and by ``cut``,
    so plain columns win and the guard covers the hazard.
    """
    for row in rows:
        for field, value in row.items():
            if "\t" in str(value) or "\n" in str(value):
                raise TabInFieldError(path.name, field, value)
    with path.open("w", newline="", encoding="utf-8") as handle:
        writer = csv.DictWriter(handle, fieldnames=list(rows[0]), delimiter="\t")
        writer.writeheader()
        writer.writerows(rows)
