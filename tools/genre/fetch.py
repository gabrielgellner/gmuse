#!/usr/bin/env python3
"""Fetch genres for every MusicBrainz release in the library, and cache them.

Touches no audio and writes no tags. Re-running is free: anything already in
the cache is skipped, which matters because MusicBrainz asks for one request
per second.

**MusicBrainz is the bridge, not the source.** It had genres for 3 of 40
releases here; 33 of those releases carry a Discogs URL relationship, and
Discogs' Genre/Style split *is* this library's broad-first convention.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import sqlite3
import time
from pathlib import Path
from typing import Any, Final

import requests

from genre.common import secret, tags_of, walk

USER_AGENT: Final = {"User-Agent": "gmuse-genre-tool/0.1 ( https://gitlab.com/gabrielgellner/gmuse )"}
MB_RELEASE: Final = "https://musicbrainz.org/ws/2/release/{}"
DISCOGS_RELEASE: Final = "https://api.discogs.com/releases/{}"
#: MusicBrainz asks for no more than one request a second, and means it.
COURTESY_DELAY: Final = 1.05
TIMEOUT: Final = 30
HTTP_OK: Final = 200


def open_cache(path: Path) -> sqlite3.Connection:
    """Open the lookup cache, creating it if this is the first run."""
    con = sqlite3.connect(path)
    con.execute("CREATE TABLE IF NOT EXISTS lookup (mbid TEXT PRIMARY KEY, fetched_at TEXT, payload TEXT)")
    con.commit()
    return con


def mb_release(mbid: str) -> dict[str, Any] | None:
    """Fetch one release from MusicBrainz, with its genres, tags and URL relationships."""
    response = requests.get(
        MB_RELEASE.format(mbid),
        headers=USER_AGENT,
        timeout=TIMEOUT,
        params={"inc": "genres+tags+url-rels+release-groups", "fmt": "json"},
    )
    time.sleep(COURTESY_DELAY)
    return response.json() if response.status_code == HTTP_OK else None


def discogs_id(release: dict[str, Any]) -> str | None:
    """Find the Discogs release id in a MusicBrainz release's URL relationships.

    Discogs is reached *through* MusicBrainz, so nothing has to be matched by
    name — the relationship is already curated.
    """
    for relation in release.get("relations") or []:
        url = str((relation.get("url") or {}).get("resource", ""))
        if "discogs.com/release/" in url:
            return url.rstrip("/").split("/")[-1].split("-")[0]
    return None


def discogs_release(release_id: str, token: str) -> dict[str, Any] | None:
    """Fetch one release from Discogs, for its genres and styles."""
    response = requests.get(
        DISCOGS_RELEASE.format(release_id),
        headers=USER_AGENT,
        timeout=TIMEOUT,
        params={"token": token},
    )
    time.sleep(COURTESY_DELAY)
    return response.json() if response.status_code == HTTP_OK else None


def payload_for(release: dict[str, Any], token: str | None) -> dict[str, Any]:
    """Reduce a MusicBrainz release, and its Discogs counterpart, to what we store."""
    payload: dict[str, Any] = {
        "title": release.get("title"),
        "mb_genres": [(g["name"], g.get("count", 0)) for g in release.get("genres", [])],
        "mb_tags": [(t["name"], t.get("count", 0)) for t in release.get("tags", [])],
    }
    release_id = discogs_id(release)
    if release_id:
        payload["discogs_id"] = release_id
        if token and (found := discogs_release(release_id, token)):
            payload["discogs_genres"] = found.get("genres", [])
            payload["discogs_styles"] = found.get("styles", [])
    return payload


def releases_in(root: Path) -> set[str]:
    """Collect every distinct MusicBrainz release id tagged in the library."""
    return {mbid for path in walk(root) if (mbid := (tags_of(path).get("musicbrainz album id") or "").strip())}


def main() -> None:
    """Fetch what the cache is missing, and report what each release yielded."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", default="~/MusicLibrary", help="library root to scan")
    args = parser.parse_args()

    here = Path(__file__).parent
    con = open_cache(here / "genre-cache.sqlite")
    token = secret("DISCOGS_TOKEN")

    cached = {row[0] for row in con.execute("SELECT mbid FROM lookup")}
    wanted = releases_in(Path(args.root).expanduser())
    todo = sorted(wanted - cached)

    print(f"{len(wanted)} distinct releases, {len(cached)} cached, {len(todo)} to fetch")
    print(f"discogs: {'token set' if token else 'NO TOKEN — MusicBrainz only, which has genres for very few'}")

    for i, mbid in enumerate(todo, 1):
        release = mb_release(mbid)
        if release is None:
            print(f"  [{i}/{len(todo)}] {mbid} — not found")
            continue
        payload = payload_for(release, token)
        con.execute(
            # Columns named, not positional: an unnamed INSERT breaks the
            # moment the schema changes, which is how this failed once already
            # against a cache written by an earlier version.
            "INSERT OR REPLACE INTO lookup (mbid, fetched_at, payload) VALUES (?, ?, ?)",
            (mbid, dt.datetime.now(tz=dt.UTC).isoformat(), json.dumps(payload)),
        )
        con.commit()
        got = payload.get("discogs_styles") or [name for name, _ in payload["mb_genres"]] or "—"
        print(f"  [{i}/{len(todo)}] {str(payload['title'])[:40]:42} {got}")


if __name__ == "__main__":
    main()
