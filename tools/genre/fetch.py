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
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Final

import requests

from genre.common import album_key, out_path, secret, tags_of, tracklist_match, walk

USER_AGENT: Final = {"User-Agent": "gmuse-genre-tool/0.1 ( https://gitlab.com/gabrielgellner/gmuse )"}
MB_RELEASE: Final = "https://musicbrainz.org/ws/2/release/{}"
MB_RELEASE_GROUP: Final = "https://musicbrainz.org/ws/2/release-group/{}"
DISCOGS_RELEASE: Final = "https://api.discogs.com/releases/{}"
DISCOGS_SEARCH: Final = "https://api.discogs.com/database/search"
#: How many search hits are worth fetching a tracklist for. Each costs a call,
#: and a match beyond the third is not one worth trusting anyway.
CANDIDATES: Final = 3
#: MusicBrainz asks for no more than one request a second, and means it.
COURTESY_DELAY: Final = 1.05
TIMEOUT: Final = 30
HTTP_OK: Final = 200
HTTP_BUSY: Final = 503
#: MusicBrainz answers 503 when it thinks you are going too fast. Treating that
#: as "no data" is how a release with genres was cached as having none — the
#: run reported `—` and looked like a gap in the source rather than a throttle.
RETRIES: Final = 4


def open_cache(path: Path) -> sqlite3.Connection:
    """Open the lookup cache, creating it if this is the first run."""
    con = sqlite3.connect(path)
    con.execute("CREATE TABLE IF NOT EXISTS lookup (mbid TEXT PRIMARY KEY, fetched_at TEXT, payload TEXT)")
    # The fallback's results live in their own table because they are keyed on
    # a *name*, not on an id, and are a weaker kind of evidence — see `search`.
    con.execute(
        "CREATE TABLE IF NOT EXISTS search "
        "(artist TEXT, album TEXT, fetched_at TEXT, payload TEXT, PRIMARY KEY (artist, album))"
    )
    con.commit()
    return con


def search_discogs(artist: str, album: str, token: str) -> list[dict[str, Any]]:
    """Find a release on Discogs by name, when no link led to one.

    **The fallback, and it is deliberately second.** Going through
    MusicBrainz's URL relationship means nothing is matched by name, so a wrong
    match is impossible. The cost, which only showed once the library grew, is
    that a release carrying no such relationship is unreachable even when
    Discogs plainly has the album — and a store-bought file with no MBID at all
    never gets as far as MusicBrainz.

    So this runs only where the linked path found nothing, and what it returns
    is marked as a name match in the review, because a name match **can be
    wrong**: searching for *Let It Be* offers a Korean pressing before the one
    you own.
    """
    response = requests.get(
        DISCOGS_SEARCH,
        headers=USER_AGENT,
        timeout=TIMEOUT,
        params={"artist": artist, "release_title": album, "type": "release", "token": token, "per_page": 5},
    )
    time.sleep(COURTESY_DELAY)
    if response.status_code != HTTP_OK:
        return []
    results = response.json().get("results")
    return list(results) if isinstance(results, list) else []


def get_json(url: str, params: dict[str, str]) -> dict[str, Any] | None:
    """One request, retried while the server says it is busy.

    **A 503 is not an answer.** Treating every non-200 as "no data" cached a
    release *with* genres as having none, and the run printed `—`, which read
    as a gap in MusicBrainz rather than as a throttle we caused. A 404 is
    final; a 503 means wait and ask again.
    """
    delay = COURTESY_DELAY
    for _ in range(RETRIES):
        response = requests.get(url, headers=USER_AGENT, timeout=TIMEOUT, params=params)
        time.sleep(delay)
        if response.status_code == HTTP_OK:
            body = response.json()
            return body if isinstance(body, dict) else None
        if response.status_code != HTTP_BUSY:
            return None
        delay *= 2
    return None


def mb_release(mbid: str) -> dict[str, Any] | None:
    """Fetch one release from MusicBrainz, with its genres, tags and URL relationships."""
    return get_json(MB_RELEASE.format(mbid), {"inc": "genres+tags+url-rels+release-groups", "fmt": "json"})


def mb_release_group(rgid: str) -> dict[str, Any] | None:
    """Fetch a release *group*, which is where MusicBrainz actually keeps genres.

    **A release is a pressing; a release group is the album.** Genre is a
    property of the album, so that is where the folksonomy lands — and querying
    only the release makes MusicBrainz look empty when it is not. Measured on
    this library: **374 of 429 releases carry no release-level genres**, while
    the same records' groups carry `breaks, downtempo, electronic, idm,
    leftfield, trip hop` and the like.

    This was the reason the name-search fallback existed at all, and most of
    what it was guessing at was reachable from the id the file already had.
    """
    return get_json(MB_RELEASE_GROUP.format(rgid), {"inc": "genres+tags", "fmt": "json"})


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


def verify(artist: str, album: str, ours: list[str], token: str) -> dict[str, Any] | None:
    """Search Discogs by name, then **check the tracklist before believing it**.

    Picard's discipline. A name search returns plausible wrong answers — *Let It
    Be* offers a Korean pressing before the deluxe edition this library holds —
    so each candidate's tracklist is fetched and compared, and only an `exact`
    agreement is something a tool should act on unattended. Anything less is
    recorded with its candidates and left for a person.
    """
    best: dict[str, Any] | None = None
    best_share = -1.0
    for hit in search_discogs(artist, album, token)[:CANDIDATES]:
        release = discogs_release(str(hit.get("id")), token)
        if release is None:
            continue
        theirs = [str(t.get("title", "")) for t in release.get("tracklist") or []]
        verdict, share = tracklist_match(ours, theirs)
        candidate: dict[str, Any] = {
            "matched": hit.get("title"),
            "year": hit.get("year"),
            "verdict": verdict,
            "share": round(share, 3),
            "ours": len(ours),
            "theirs": len(theirs),
            "discogs_genres": release.get("genres") or [],
            "discogs_styles": release.get("styles") or [],
        }
        if verdict == "exact":
            return candidate
        if share > best_share:
            best, best_share = candidate, share
    return best


def payload_for(release: dict[str, Any], token: str | None) -> dict[str, Any]:
    """Reduce a MusicBrainz release, and its Discogs counterpart, to what we store."""
    payload: dict[str, Any] = {
        "title": release.get("title"),
        "mb_genres": [(g["name"], g.get("count", 0)) for g in release.get("genres", [])],
        "mb_tags": [(t["name"], t.get("count", 0)) for t in release.get("tags", [])],
    }
    # The release almost never has genres; its group usually does.
    group = release.get("release-group") or {}
    if not payload["mb_genres"] and (rgid := group.get("id")) and (found := mb_release_group(str(rgid))):
        payload["mb_genres"] = [(g["name"], g.get("count", 0)) for g in found.get("genres", [])]
        payload["mb_tags"] = payload["mb_tags"] or [(t["name"], t.get("count", 0)) for t in found.get("tags", [])]
    release_id = discogs_id(release)
    if release_id:
        payload["discogs_id"] = release_id
        if token and (found := discogs_release(release_id, token)):
            payload["discogs_genres"] = found.get("genres", [])
            payload["discogs_styles"] = found.get("styles", [])
    return payload


@dataclass
class Album:
    """One album as the library holds it: its release id, and its tracklist."""

    mbid: str = ""
    titles: list[str] = field(default_factory=list)


def albums_in(root: Path) -> dict[tuple[str, str], Album]:
    """Every album, with the MusicBrainz release id it carries and its tracks.

    The tracklist is collected here because it is what decides whether a name
    search found the same record — see `tracklist_match`.
    """
    out: dict[tuple[str, str], Album] = {}
    for path in walk(root):
        tags = tags_of(path)
        album = out.setdefault(album_key(tags), Album())
        album.mbid = album.mbid or (tags.get("musicbrainz album id") or "").strip()
        album.titles.append((tags.get("title") or path.stem).strip())
    return out


def has_genres(payload: dict[str, Any] | None) -> bool:
    """Whether a cached lookup actually yielded something to use."""
    if payload is None:
        return False
    return bool(payload.get("discogs_genres") or payload.get("discogs_styles") or payload.get("mb_genres"))


def main() -> None:
    """Fetch what the cache is missing, and report what each release yielded."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", default="~/MusicLibrary", help="library root to scan")
    args = parser.parse_args()

    con = open_cache(out_path("genre-cache.sqlite"))
    token = secret("DISCOGS_TOKEN")

    albums = albums_in(Path(args.root).expanduser())
    cached = {row[0] for row in con.execute("SELECT mbid FROM lookup")}
    wanted = {a.mbid for a in albums.values() if a.mbid}
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

    # ── the fallback ──────────────────────────────────────────────────────
    if not token:
        return
    looked = {m: json.loads(p) for m, p in con.execute("SELECT mbid, payload FROM lookup")}
    searched = {(a, b) for a, b in con.execute("SELECT artist, album FROM search")}
    stranded = [
        (key, held)
        for key, held in sorted(albums.items())
        if not has_genres(looked.get(held.mbid)) and key not in searched
    ]
    print(f"\n{len(stranded)} album(s) no link reached; searching Discogs by name")
    for i, ((artist, album), held) in enumerate(stranded, 1):
        found = verify(artist, album, held.titles, token)
        if found is None:
            print(f"  [{i}/{len(stranded)}] {artist[:18]:20} {album[:24]:26} no hit")
            continue
        con.execute(
            "INSERT OR REPLACE INTO search (artist, album, fetched_at, payload) VALUES (?, ?, ?, ?)",
            (artist, album, dt.datetime.now(tz=dt.UTC).isoformat(), json.dumps(found)),
        )
        con.commit()
        print(
            f"  [{i}/{len(stranded)}] {artist[:18]:20} {album[:24]:26} "
            f"{found['verdict']:6} {found['ours']}v{found['theirs']} tracks  {found['discogs_genres']}"
        )


if __name__ == "__main__":
    main()
