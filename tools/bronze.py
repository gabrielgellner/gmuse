#!/usr/bin/env python3
"""What an authority said, kept so it need not be asked twice — issue #58.

**A request cache, and deliberately not library state.** The library is the
source of truth about your collection; this is a record of what MusicBrainz or
Discogs happened to answer, on a day, to a question a tool asked. Those are
different kinds of thing, and the test for which is which is simple:

> Deleting this may cost time. It must never cost a fact.

Anything you would be sorry to lose has to be **extracted into the library**
before the cache goes — which is what `apply` and the proposal files are for.
If a claim only exists here, it is not yet known.

## Where it lives, and why that settles the persistence question

`$XDG_CACHE_HOME/gmuse`, or `~/Library/Caches/gmuse` on macOS. Not the
library's `data_dir`, which syncs between machines and would make one
machine's lookups into the other's facts. Not `out/`, which is the repository.

A cache directory is "exactly what a backup tool skips", which was the
argument *against* putting proposals in one. Here it is the argument *for*: the
only thing at risk is wall-clock.

**Nothing is evicted automatically.** Keeping it for a year and dropping it
tomorrow are both correct, and neither is a decision this module makes. That is
how it can be as persistent as you like without being coupled to anything —
`prune` exists, and only ever runs because a person ran it.

## Keyed by the request, not by the answer we wanted

The key is the method, the URL and the sorted parameters. Not `(artist,
title)`, which is what `identify.py` used and paid for twice in one afternoon:
changing how a query is *built* changed every key, and a cache of a superseded
guess is worth nothing. Keyed by request, a better query simply adds rows and
the old ones stay readable.

## A negative answer is an answer

"Discogs has never heard of this album" costs four requests to learn and was
being re-learned on every run, because `None` was printed and not written down.
It is stored here with its status, so the second run is free — and so that
"nobody knows" is distinguishable from "nobody asked".

## Secrets never land in it

Discogs takes its token as a query parameter. It is stripped from the key and
from the stored URL, so a cache file can be read, copied or attached to a bug
report without leaking a credential.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import platform
import sqlite3
import time
import urllib.parse
from pathlib import Path
from typing import Final

import requests

#: Parameters that authenticate rather than describe. Stripped from the cache
#: key so that rotating a token does not invalidate every entry, and from the
#: stored URL so the file holds no credential.
SECRET_PARAMS: Final = frozenset({"token", "client", "key", "apikey", "api_key"})

#: MusicBrainz asks for no more than one request a second, and means it. Paid
#: only on a real request — a cache hit does not sleep, which is what makes a
#: replay instant rather than an hour.
COURTESY_DELAY: Final = 1.05
TIMEOUT: Final = 30
HTTP_OK: Final = 200
HTTP_BUSY: Final = 503
#: A 503 is a throttle, not an answer. Treating it as "no data" is how the
#: genre pass cached a release *with* genres as having none.
RETRIES: Final = 4


def cache_dir() -> Path:
    """Where the cache lives on this machine.

    A device fact, like `data_dir` is not: two machines keep their own, and a
    hit on one says nothing about the other.
    """
    if xdg := os.environ.get("XDG_CACHE_HOME"):
        return Path(xdg) / "gmuse"
    if platform.system() == "Darwin":
        return Path.home() / "Library" / "Caches" / "gmuse"
    return Path.home() / ".cache" / "gmuse"


def open_cache(path: Path | None = None) -> sqlite3.Connection:
    """Open the cache, creating it if this is the first run."""
    path = path or (cache_dir() / "bronze.sqlite")
    path.parent.mkdir(parents=True, exist_ok=True)
    con = sqlite3.connect(path)
    con.execute(
        "CREATE TABLE IF NOT EXISTS response ("
        "  key TEXT PRIMARY KEY,"
        "  url TEXT NOT NULL,"
        "  fetched_at TEXT NOT NULL,"
        "  status INTEGER NOT NULL,"
        "  body TEXT NOT NULL)"
    )
    con.commit()
    return con


def public(params: dict[str, str] | None) -> dict[str, str]:
    """The parameters minus anything that authenticates."""
    return {k: v for k, v in (params or {}).items() if k.lower() not in SECRET_PARAMS}


def key_of(url: str, params: dict[str, str] | None) -> str:
    """The identity of a request.

    Sorted, so two callers spelling the same question in a different order ask
    it once. Secrets excluded, so rotating a token is not a cache flush.
    """
    query = urllib.parse.urlencode(sorted(public(params).items()))
    return f"{url}?{query}" if query else url


def age_days(fetched_at: str) -> float:
    """How old an entry is, in days, or infinity if the stamp will not parse."""
    try:
        then = dt.datetime.fromisoformat(fetched_at)
    except ValueError:
        return float("inf")
    return (dt.datetime.now(tz=dt.UTC) - then).total_seconds() / 86_400


def get(
    con: sqlite3.Connection,
    url: str,
    params: dict[str, str] | None = None,
    *,
    headers: dict[str, str],
    max_age: float | None = None,
) -> tuple[int, object | None]:
    """Ask, or recall having asked.

    Returns the HTTP status and the decoded body, which is `None` for anything
    that was not a 200 or would not parse as JSON.

    `max_age` in days re-asks an entry older than that. The default is to keep
    what is stored however old it is: staleness is a judgement about the
    question, not about the cache, and a release from 2016 has not changed.
    """
    key = key_of(url, params)
    row = con.execute("SELECT fetched_at, status, body FROM response WHERE key = ?", (key,)).fetchone()
    if row is not None and (max_age is None or age_days(row[0]) <= max_age):
        return int(row[1]), _decode(row[2])

    status, body = _ask(url, params, headers)
    # Belt as well as braces. A token reaching the key would be written to a
    # file on disk and stay there, which is a different class of mistake from a
    # wasted request — so it is checked at the moment of writing rather than
    # trusted to `SECRET_PARAMS` being complete.
    for name, value in (params or {}).items():
        if name.lower() in SECRET_PARAMS and value and value in key:
            msg = f"refusing to cache: the {name} parameter reached the key"
            raise ValueError(msg)
    con.execute(
        "INSERT OR REPLACE INTO response (key, url, fetched_at, status, body) VALUES (?, ?, ?, ?, ?)",
        (key, key, dt.datetime.now(tz=dt.UTC).isoformat(), status, body),
    )
    con.commit()
    return status, _decode(body)


def _decode(body: str) -> object | None:
    """The body as JSON, or None for an empty or unparseable one."""
    try:
        return json.loads(body) if body else None
    except json.JSONDecodeError:
        return None


def _ask(url: str, params: dict[str, str] | None, headers: dict[str, str]) -> tuple[int, str]:
    """One request, retried while the server says it is busy.

    A 404 is final and worth recording. A 503 means wait and ask again, and a
    run of them ends with the status stored rather than a silent zero — an
    entry that says "we were throttled" is something a later run can retry on
    purpose, where one that said "no data" could not be told from the truth.
    """
    delay = COURTESY_DELAY
    status = 0
    for _ in range(RETRIES):
        response = requests.get(url, headers=headers, params=params, timeout=TIMEOUT)
        time.sleep(delay)
        status = response.status_code
        if status == HTTP_OK:
            return status, response.text
        if status != HTTP_BUSY:
            return status, ""
        delay *= 2
    return status, ""


def stats(con: sqlite3.Connection) -> dict[str, int]:
    """What is in here, for a person deciding whether to keep it."""
    rows = con.execute("SELECT status, COUNT(*), SUM(LENGTH(body)) FROM response GROUP BY status").fetchall()
    out = {"entries": 0, "bytes": 0, "answered": 0, "empty": 0}
    for status, count, size in rows:
        out["entries"] += count
        out["bytes"] += size or 0
        if status == HTTP_OK:
            out["answered"] += count
        else:
            out["empty"] += count
    return out


def prune(con: sqlite3.Connection, older_than: float) -> int:
    """Drop entries older than `older_than` days.

    **Only ever called by a person.** Nothing expires on its own, because the
    right lifetime is a judgement about the question rather than a property of
    the cache — and an entry quietly disappearing is how a re-run becomes an
    hour without anyone deciding it should.
    """
    doomed = [
        key
        for key, fetched_at in con.execute("SELECT key, fetched_at FROM response")
        if age_days(fetched_at) > older_than
    ]
    con.executemany("DELETE FROM response WHERE key = ?", [(k,) for k in doomed])
    con.commit()
    return len(doomed)


def main() -> None:
    """Report what is cached, or prune it. The tools use `get` directly."""
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("stats", help="what is cached, and how big")
    pr = sub.add_parser("prune", help="drop entries older than N days")
    pr.add_argument("--older-than", type=float, required=True)
    args = parser.parse_args()

    con = open_cache()
    if args.command == "stats":
        s = stats(con)
        print(f"{cache_dir() / 'bronze.sqlite'}")
        print(f"  {s['entries']} responses, {s['bytes'] / 1_048_576:.1f} MiB")
        print(f"  {s['answered']} answered, {s['empty']} asked and got nothing")
        print("\nSafe to delete. It costs time to rebuild, never a fact.")
    else:
        print(f"dropped {prune(con, args.older_than)} entries")


if __name__ == "__main__":
    main()
