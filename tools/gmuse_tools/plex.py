"""Plays and ratings read back from Plex, as proposals — #338.

`renderings.md` §6 and #337: **Plex is one of your devices.** A play on the
phone is recorded as the Plex server's own — in its log, under its time — and
a rating made in Plexamp has the standing of one made at the laptop. This pass
reads the server and writes those as proposals; `gmuse shelf apply` records
them. A tool proposes and the library applies (`layers.md`).

```
uv run gmuse-tools plex read                       # -> out/plex-read.jsonl
gmuse shelf apply tools/out/plex-read.jsonl --write
```

## Found through the manifest, never by name

A Plex item is a file, and a file in the rendering is one the renderer wrote,
so its path is a manifest row and the row names the encoding (§4). A path the
manifest does not name — an old library Plex still serves, a file somebody put
there — is counted and left: #339 is how the old libraries' history comes in.

## The stamp is built from Plex's own data

`(viewedAt, the history entry's id, the server)` for a play, and `(lastRatedAt,
the item's id, the server)` for a rating. The same on every read, so a second
read proposes the same records and `apply` recognises them (#337). Nothing
needs remembering between runs.

## A rating only where it differs

A rating gmuse pushes to Plex comes back re-stamped. So a Plex rating is
proposed only where it differs from what the library holds now — a pushed
rating does not echo back, and a re-rate to the same stars is nothing to say.
A **half star** (Plex counts to ten) is not one the library can hold: reported,
not rounded, because which way to round is a judgement about one of your
ratings.

## The token

Read from the Keychain as `gmuse-plex` (`genre.common.secret`), sent only to
the server named here, and never written or printed.
"""

from __future__ import annotations

import collections
import json
import re
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any, Final

import requests
import typer

from gmuse_tools.bronze import cache_dir
from gmuse_tools.destination import parse as parse_target
from gmuse_tools.genre.common import out_path, secret, write_tsv
from gmuse_tools.library import open_library

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable, Mapping

    import gmuse_core

#: Where the server is, as this machine reaches it.
SERVER: Final = "http://asnas:32400"

#: The rendering Plex reads (#336), and where Plex sees it: Plex runs with the
#: NAS's `/volume1/Media` mounted at `/share/Media` (measured, 2026-10-24).
TARGET: Final = "asnas:/volume1/Media/PlexMusic"
PLEX_ROOT: Final = "/share/Media/PlexMusic"

#: Where Plex keeps the NAS's `/volume1/Media`, and where this machine mounts it.
PLEX_SHARE_MOUNT: Final = re.compile(r"^/share/Media")
MOUNT: Final = "/Volumes/Media"

#: How many items to ask for at once.
PAGE: Final = 500

#: Plex counts a rating to ten; the library to five.
PLEX_SCALE: Final = 2

#: A stamp's counter is 32 bits.
SEQ_LIMIT: Final = 2**32

#: The Plex library types that hold music.
MUSIC: Final = "artist"


class PlexError(RuntimeError):
    """The server could not be asked, or answered nothing usable."""


@dataclass
class Server:
    """A Plex server, reached with a token. `get` is the one door to it."""

    url: str
    token: str

    def get(self, path: str, params: Mapping[str, str] | None = None, *, start: int = 0) -> ET.Element:
        """One page of an answer, as XML."""
        headers = {
            "X-Plex-Token": self.token,
            "Accept": "application/xml",
            "X-Plex-Container-Start": str(start),
            "X-Plex-Container-Size": str(PAGE),
        }
        got = requests.get(f"{self.url}{path}", headers=headers, params=dict(params or {}), timeout=60)
        if got.status_code != requests.codes.ok:
            # Never the URL with its headers: the token is in them.
            msg = f"{path}: HTTP {got.status_code}"
            raise PlexError(msg)
        return ET.fromstring(got.text)  # noqa: S314 — the user's own server


def every(get: Callable[..., ET.Element], path: str, params: Mapping[str, str] | None = None) -> list[ET.Element]:
    """Every item an endpoint has, a page at a time."""
    out: list[ET.Element] = []
    while True:
        page = get(path, params, start=len(out))
        items = list(page)
        out.extend(items)
        total = int(page.get("totalSize") or page.get("size") or 0)
        if not items or len(out) >= total:
            return out


def device_of(get: Callable[..., ET.Element]) -> str:
    """The name the server's device id is derived from: `plex:<machineIdentifier>`."""
    ident = get("/identity", None).get("machineIdentifier")
    if not ident:
        msg = "the server did not say who it is"
        raise PlexError(msg)
    return f"plex:{ident}"


def music_sections(get: Callable[..., ET.Element]) -> list[str]:
    """The keys of the server's music libraries."""
    return [d.get("key", "") for d in get("/library/sections", None) if d.get("type") == MUSIC]


def files_of(get: Callable[..., ET.Element], keys: Iterable[str]) -> dict[str, str]:
    """`{ratingKey: the file Plex plays for it}`, asked a batch at a time."""
    # A play of an item Plex has since deleted has no `ratingKey`; there is no
    # file to ask for.
    keys = sorted({k for k in keys if k})
    out: dict[str, str] = {}
    for i in range(0, len(keys), 100):
        batch = ",".join(keys[i : i + 100])
        for item in get(f"/library/metadata/{batch}", None):
            part = item.find("./Media/Part")
            if part is not None and part.get("file"):
                out[item.get("ratingKey", "")] = part.get("file", "")
    return out


@dataclass(frozen=True)
class Copy:
    """What a Plex file is, in the library: its encoding and the recording that is of."""

    encoding: str
    recording: str
    stars: int | None


@dataclass
class Read:
    """What a read found: the proposals, and everything it would not propose and why."""

    proposals: list[dict[str, Any]] = field(default_factory=list)
    plays: int = 0
    ratings: int = 0
    #: `why → how many` for what was left: not in the rendering, a half star, unchanged…
    left: collections.Counter[str] = field(default_factory=collections.Counter)
    #: `file → [why, plays, Plex's rating]` for every file whose history was left
    #: because it could not be placed — the review, with the history attached,
    #: because for a file the library does not hold that history is its only record.
    unplaced: dict[str, list[Any]] = field(default_factory=dict)

    def leave(self, file: str, why: str, *, plays: int = 0, rating: str = "") -> None:
        """Count what was left, and keep its history for the review."""
        row = self.unplaced.setdefault(file or "(no file)", [why, 0, ""])
        row[1] += plays
        row[2] = rating or row[2]


#: What a resolver says for a file it cannot place: why, in a person's words.
Why = str


def resolver(
    manifest: Mapping[str, Mapping[str, str]],
    copies: Mapping[str, Copy],
    plex_root: str,
    by_audio: Callable[[str], str | Why] | None = None,
) -> Callable[[str], Copy | Why]:
    """A Plex file path → the copy it is, or why not.

    **The manifest first**, for a file the rendering made (#338): its row names
    the encoding. **Then its audio** (#339), for a file in a library Plex still
    serves that no manifest describes: `by_audio` hashes the bytes and gives the
    encoding they are. Never a name — a path is not an identity.
    """
    prefix = plex_root.rstrip("/") + "/"

    def resolve(file: str) -> Copy | Why:
        if not file:
            return "an item Plex no longer has"
        if file.startswith(prefix):
            row = manifest.get(file.removeprefix(prefix))
            if not row:
                return "a file in the rendering its manifest does not name"
            return copies.get(row["encoding"]) or "a rendered copy the library no longer holds"
        if by_audio is None:
            return "a file no rendering made"
        encoding = by_audio(file)
        if encoding is None or encoding.startswith("!"):
            return encoding[1:] if encoding else "a file that could not be read"
        return copies.get(encoding) or "audio the library does not hold"

    return resolve


@dataclass
class Audio:
    """A Plex file's audio, hashed through the share and remembered — #339.

    `encoding_at` is the library's own hash (`gmuse_core`), the one the log
    records, so a match is the same bytes and never the same name. Remembered
    by path, size and mtime in the tools' cache: the first read hashes ~1,900
    files (~7 minutes through the mount, measured), and a later one none.
    """

    lib: gmuse_core.Library
    #: Where Plex's `/share/Media` is on this machine.
    mount: str
    cache: Path
    seen: dict[str, list[Any]] = field(default_factory=dict)

    def __post_init__(self) -> None:
        """Load what earlier reads hashed."""
        if self.cache.is_file():
            self.seen = json.loads(self.cache.read_text(encoding="utf-8"))

    def __call__(self, file: str) -> str | None:
        """The encoding the file's bytes are, or `!why` where there is none."""
        local = Path(PLEX_SHARE_MOUNT.sub(self.mount, file, count=1))
        try:
            st = local.stat()
        except OSError:
            # **Not "gone".** Three DanceMusic files list over SMB and will not
            # open (`nas-music-sources`): exists() is False for a path the share
            # itself returned. Plex has the file, so this is a reading failure.
            return "!a file that could not be read through the share"
        key = [st.st_size, st.st_mtime_ns]
        if (hit := self.seen.get(str(local))) and hit[:2] == key:
            return hit[2]
        encoding = self.lib.encoding_at(local)
        self.seen[str(local)] = [*key, encoding or "!a file whose audio could not be read"]
        return self.seen[str(local)][2]

    def save(self) -> None:
        """Write what this read hashed, for the next."""
        self.cache.parent.mkdir(parents=True, exist_ok=True)
        self.cache.write_text(json.dumps(self.seen), encoding="utf-8")


def stamp(device: str, seconds: str | None, key: str | None) -> dict[str, Any] | None:
    """The `elsewhere` a proposal carries: Plex's time, Plex's id, the server — or `None` if Plex gave none."""
    if not seconds or not key or not key.isdigit() or int(key) >= SEQ_LIMIT:
        return None
    return {"device": device, "at_ms": int(float(seconds)) * 1000, "seq": int(key)}


def history_key(entry: ET.Element) -> str | None:
    """A history entry's own id: `/status/sessions/history/12345` → `12345`."""
    return (entry.get("historyKey") or "").rsplit("/", 1)[-1] or None


def read(get: Callable[..., ET.Element], resolve: Callable[[str], Copy | Why]) -> Read:
    """Read the server's plays and ratings into proposals."""
    out = Read()
    device = device_of(get)
    sections = music_sections(get)
    history = [h for s in sections for h in every(get, "/status/sessions/history/all", {"librarySectionID": s})]
    rated = [t for s in sections for t in every(get, f"/library/sections/{s}/all", {"type": "10", "userRating>>": "0"})]
    files = files_of(get, [h.get("ratingKey", "") for h in history] + [t.get("ratingKey", "") for t in rated])

    for entry in history:
        file = files.get(entry.get("ratingKey", ""), "")
        copy = resolve(file)
        elsewhere = stamp(device, entry.get("viewedAt"), history_key(entry))
        if isinstance(copy, str):
            out.left[f"a play: {copy}"] += 1
            out.leave(file, copy, plays=1)
        elif elsewhere is None:
            out.left["a play Plex gave no time or id for"] += 1
        else:
            out.proposals.append(
                {"fact": "played", "recording": copy.recording, "encoding": copy.encoding, "elsewhere": elsewhere}
            )
            out.plays += 1

    for track in rated:
        file = files.get(track.get("ratingKey", ""), "")
        copy = resolve(file)
        if isinstance(copy, str):
            out.left[f"a rating: {copy}"] += 1
            out.leave(file, copy, rating=track.get("userRating", ""))
        else:
            rate(track, device, copy, out)
    return out


def rate(track: ET.Element, device: str, copy: Copy, out: Read) -> None:
    """One Plex rating: a proposal where it says something new, else a reason it does not."""
    value = float(track.get("userRating") or 0)
    stars = round(value / PLEX_SCALE)
    elsewhere = stamp(device, track.get("lastRatedAt"), track.get("ratingKey"))
    if value != stars * PLEX_SCALE:
        out.left["a half star, which the library cannot hold"] += 1
    elif copy.stars == stars:
        out.left["a rating the library already holds"] += 1
    elif elsewhere is None:
        out.left["a rating Plex gave no time for"] += 1
    else:
        out.proposals.append({"fact": "rated", "recording": copy.recording, "stars": stars, "elsewhere": elsewhere})
        out.ratings += 1


def copies_of(lib: gmuse_core.Library) -> dict[str, Copy]:
    """Every copy this library holds, with its recording and the stars that recording has now."""
    rows = [r for r in lib.query(None, every_copy=True) if r.get("encoding") and r.get("recording")]
    said = lib.statement([r["encoding"] for r in rows])
    return {r["encoding"]: Copy(r["encoding"], r["recording"], s.get("stars")) for r, s in zip(rows, said, strict=True)}


def register(app: typer.Typer) -> None:
    """Attach `plex read`."""
    plex_app = typer.Typer(no_args_is_help=True, help=__doc__)
    app.add_typer(plex_app, name="plex")

    @plex_app.command("read")
    def read_cmd(
        server: str = typer.Option(SERVER, help="the Plex server, as this machine reaches it"),
        target: str = typer.Option(TARGET, help="the rendering Plex reads, whose manifest names its files"),
        plex_root: str = typer.Option(PLEX_ROOT, help="where Plex sees that rendering"),
        mount: str = typer.Option(MOUNT, help="where this machine mounts the NAS's Media share"),
        *,
        by_audio: bool = typer.Option(
            default=True, help="place files outside the rendering by their audio (#339); ~7 min the first time"
        ),
    ) -> None:
        """Read Plex's plays and ratings into proposals for `gmuse shelf apply`. Writes nothing to the library."""
        token = secret("PLEX_TOKEN")
        if not token:
            print('no Plex token: security add-generic-password -a "$USER" -s gmuse-plex -w')
            raise typer.Exit(1)
        lib = open_library()
        text = parse_target(target).read_manifest()
        manifest = json.loads(text)["files"] if text else {}
        audio = Audio(lib, mount, cache_dir() / "plex-audio.json") if by_audio else None
        try:
            found = read(Server(server, token).get, resolver(manifest, copies_of(lib), plex_root, audio))
        finally:
            if audio:
                audio.save()
        out = out_path("plex-read.jsonl")
        review = out_path("plex-unplaced.tsv")
        rows = [{"file": f, "why": w, "plays": n, "plex_rating": r} for f, (w, n, r) in sorted(found.unplaced.items())]
        write_tsv(review, rows, ["file", "why", "plays", "plex_rating"])
        lib.propose([json.dumps(p) for p in found.proposals], out)
        print(f"  {found.plays} play(s) and {found.ratings} rating(s) proposed")
        for why, n in sorted(found.left.items()):
            print(f"  {n:6d}  left: {why}")
        print(f"\n-> {out}\n   gmuse shelf apply {out} --write")
        print(f"-> {review}: {len(rows)} file(s) whose history could not be placed, with it attached")
