"""Plex reading the rendering: checked, then given the library's ratings and playlists — #340.

`renderings.md` §6. Once Plex serves the rendering, two things are true of it
that this module checks and acts on:

- **It should say what the library says.** `verify` compares every track's
  title in Plex with the library's statement, through the manifest, and lists
  the fields Plex has **locked** — an edit in Plex's UI locks a field, and a
  locked field ignores the tags from then on. A lock is reported, never undone
  (`CLAUDE.md`, *reported, never decided*).
- **It holds none of the library's state yet.** `push` sets each track's rating
  to the library's where they differ, and creates the library's playlists from
  the rendering's items — the two #349 adopted, whose old Plex copies point at
  libraries the switch removes.

```
uv run gmuse-tools plex verify
uv run gmuse-tools plex push            # dry run
uv run gmuse-tools plex push --write
```

## A playlist pushed says where it came from

Its summary names gmuse as its home. That is the mark a later mirror reads to
tell a playlist gmuse owns from one made in Plexamp (#349), so neither is ever
taken for the other.
"""

from __future__ import annotations

import collections
import json
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from typing import TYPE_CHECKING, Any, Final

import requests
import typer

from gmuse_tools.destination import parse as parse_target
from gmuse_tools.genre.common import secret
from gmuse_tools.library import open_library
from gmuse_tools.plex import GMUSE_SUMMARY, PLEX_ROOT, PLEX_SCALE, SERVER, TARGET, Server, device_of, every

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable, Mapping

    import gmuse_core

#: What a pushed playlist's summary says, so a mirror knows whose it is — one
#: definition, the mirror's (`plex.GMUSE_SUMMARY`), so the two cannot drift.
SUMMARY: Final = GMUSE_SUMMARY

#: Plex's item types: an artist, an album, a track.
TYPES: Final = {"8": "artist", "9": "album", "10": "track"}


class PushError(RuntimeError):
    """Plex refused a write."""


@dataclass(frozen=True)
class Track:
    """A track as Plex holds it in the rendering's library."""

    key: str
    title: str
    stars: int | None
    #: The rendered path, relative to the rendering's root; `None` for a file outside it.
    path: str | None


def section_of(get: Callable[..., ET.Element], plex_root: str) -> str:
    """The key of the library that reads the rendering."""
    for d in get("/library/sections", None):
        if any(loc.get("path", "").rstrip("/") == plex_root.rstrip("/") for loc in d.findall("Location")):
            return d.get("key", "")
    msg = f"no Plex library reads {plex_root}"
    raise PushError(msg)


def tracks_of(get: Callable[..., ET.Element], section: str, plex_root: str) -> list[Track]:
    """Every track in the rendering's library, with the rendered path each plays."""
    prefix = plex_root.rstrip("/") + "/"
    out = []
    for t in every(get, f"/library/sections/{section}/all", {"type": "10"}):
        part = t.find("./Media/Part")
        file = part.get("file", "") if part is not None else ""
        rating = float(t.get("userRating") or 0)
        out.append(
            Track(
                t.get("ratingKey", ""),
                t.get("title", ""),
                round(rating / PLEX_SCALE) if rating else None,
                file.removeprefix(prefix) if file.startswith(prefix) else None,
            )
        )
    return out


def locked(get: Callable[..., ET.Element], section: str) -> collections.Counter[str]:
    """`"<type> <field>" → how many` for every field Plex has locked in the library."""
    out: collections.Counter[str] = collections.Counter()
    for kind, name in TYPES.items():
        for item in every(get, f"/library/sections/{section}/all", {"type": kind}):
            for f in item.findall("Field"):
                if f.get("locked") == "1":
                    out[f"{name} {f.get('name')}"] += 1
    return out


@dataclass
class Checked:
    """What `verify` found."""

    tracks: int = 0
    agree: int = 0
    #: `(rendered path, Plex's title, the library's)` where they differ.
    differ: list[tuple[str, str, str]] = field(default_factory=list)
    #: Tracks whose file the manifest does not name.
    stray: int = 0
    locks: collections.Counter[str] = field(default_factory=collections.Counter)


def verify(tracks: Iterable[Track], titled: Mapping[str, str]) -> Checked:
    """Compare Plex's titles with the library's, by rendered path."""
    out = Checked()
    for t in tracks:
        out.tracks += 1
        want = titled.get(t.path or "")
        if want is None:
            out.stray += 1
        elif want == t.title:
            out.agree += 1
        else:
            out.differ.append((t.path or "", t.title, want))
    return out


@dataclass(frozen=True)
class Placed:
    """What the library says about a rendered path: the recording, its stars, whether it is on a compilation."""

    recording: str
    stars: int | None
    compilation: bool


def ratings_to_push(tracks: Iterable[Track], placed: Mapping[str, Placed]) -> list[tuple[str, int | None]]:
    """`(Plex key, stars)` for each track whose rating in Plex is not the library's."""
    out = []
    for t in tracks:
        p = placed.get(t.path or "")
        if p is not None and p.stars != t.stars:
            out.append((t.key, p.stars))
    return out


def keys_by_recording(tracks: Iterable[Track], placed: Mapping[str, Placed]) -> dict[str, str]:
    """The Plex key a playlist entry for each recording points at.

    **The album's appearance over a compilation's**, then the first path: a
    recording on an album and on a sampler is two items in Plex, and a
    playlist means the song, which is the album's.
    """
    best: dict[str, tuple[tuple[bool, str], str]] = {}
    for t in tracks:
        p = placed.get(t.path or "")
        if p is None:
            continue
        rank = (p.compilation, t.path or "")
        if p.recording not in best or rank < best[p.recording][0]:
            best[p.recording] = (rank, t.key)
    return {r: key for r, (_, key) in best.items()}


def placed_of(lib: gmuse_core.Library, manifest: Mapping[str, Mapping[str, str]]) -> dict[str, Placed]:
    """`rendered path → what the library says of it`, for every track in the manifest."""
    tracks = {p: row["encoding"] for p, row in manifest.items() if "cover" not in row}
    # **A copy nothing has named has no recording** (#160): listed, and not one
    # a rating or a playlist entry can be about.
    rows = lib.query(None, every_copy=True)
    recording = {r["encoding"]: r["recording"] for r in rows if r.get("encoding") and r.get("recording")}
    paths = [p for p, e in tracks.items() if e in recording]
    said = lib.statement([tracks[p] for p in paths])
    return {
        p: Placed(recording[tracks[p]], s.get("stars"), bool(s.get("compilation")))
        for p, s in zip(paths, said, strict=True)
    }


@dataclass(frozen=True)
class Held:
    """A playlist Plex holds: its key, whether gmuse made it, and its items in order."""

    key: str
    ours: bool
    items: list[str]


def plan_playlists(wanted: Mapping[str, list[str]], held: Mapping[str, Held]) -> list[tuple[str, str, list[str]]]:
    """`(action, name, keys)` for each of the library's playlists.

    `create` where Plex has none by that name; `same` where gmuse's copy
    already holds these items in this order; `resync` where gmuse's copy has
    drifted — a renamed file is a new Plex item, and the old one's entry
    drops out of every playlist that held it; `leave` where a playlist of
    that name is not gmuse's, because it is somebody's Plexamp playlist.
    """
    out = []
    for name, keys in wanted.items():
        have = held.get(name)
        if have is None:
            out.append(("create", name, keys))
        elif not have.ours:
            out.append(("leave", name, keys))
        elif have.items == keys:
            out.append(("same", name, keys))
        else:
            out.append(("resync", name, keys))
    return out


@dataclass
class Writer:
    """The two writes `push` makes, to the user's own server."""

    url: str
    token: str

    def rate(self, key: str, stars: int | None) -> None:
        """Set an item's rating; `None` clears it."""
        value = str(stars * PLEX_SCALE) if stars else "-1"
        params = {"key": key, "identifier": "com.plexapp.plugins.library", "rating": value}
        self._send("PUT", "/:/rate", params)

    def playlist(self, machine: str, title: str, keys: list[str]) -> None:
        """Create an audio playlist of these items, in this order, marked as gmuse's."""
        uri = f"server://{machine}/com.plexapp.plugins.library/library/metadata/{','.join(keys)}"
        params = {"type": "audio", "title": title, "smart": "0", "uri": uri}
        created = self._send("POST", "/playlists", params)
        key = next((p.get("ratingKey") for p in ET.fromstring(created)), None)  # noqa: S314 — the user's own server
        if key:
            self._send("PUT", f"/playlists/{key}", {"summary": SUMMARY})

    def refresh(self, section: str) -> None:
        """Ask Plex to scan one library."""
        self._send("GET", f"/library/sections/{section}/refresh", {})

    def resync(self, machine: str, key: str, keys: list[str]) -> None:
        """Replace a playlist's items with these, in this order, keeping the playlist."""
        self._send("DELETE", f"/playlists/{key}/items", {})
        uri = f"server://{machine}/com.plexapp.plugins.library/library/metadata/{','.join(keys)}"
        self._send("PUT", f"/playlists/{key}/items", {"uri": uri})
        # An adopted playlist had no mark; it is gmuse's now, and says so.
        self._send("PUT", f"/playlists/{key}", {"summary": SUMMARY})

    def _send(self, method: str, path: str, params: Mapping[str, str]) -> str:
        headers = {"X-Plex-Token": self.token, "Accept": "application/xml"}
        got = requests.request(method, f"{self.url}{path}", headers=headers, params=dict(params), timeout=60)
        if not got.ok:
            # Never the URL or the headers: the token is in them.
            msg = f"{method} {path}: HTTP {got.status_code}"
            raise PushError(msg)
        return got.text


def connect() -> tuple[str, Server]:
    """The token and the server, or a way to set the token."""
    token = secret("PLEX_TOKEN")
    if not token:
        print('no Plex token: security add-generic-password -a "$USER" -s gmuse-plex -w')
        raise typer.Exit(1)
    return token, Server(SERVER, token)


def manifest_of() -> dict[str, Any]:
    """The rendering's manifest, read from where it lives."""
    text = parse_target(TARGET).read_manifest()
    return json.loads(text)["files"] if text else {}


def run_verify() -> Checked:
    """`plex verify`: Plex's titles against the library's, and the locked fields."""
    _, server = connect()
    lib = open_library()
    tracks_in = {p: row["encoding"] for p, row in manifest_of().items() if "cover" not in row}
    said = dict(zip(tracks_in, lib.statement(list(tracks_in.values())), strict=True))
    section = section_of(server.get, PLEX_ROOT)
    found = verify(tracks_of(server.get, section, PLEX_ROOT), {p: s.get("title", "") for p, s in said.items()})
    found.locks = locked(server.get, section)
    print(f"  {found.tracks} track(s) in Plex, {len(tracks_in)} in the manifest")
    print(f"  {found.agree} titled as the library says, {len(found.differ)} not, {found.stray} not in the manifest")
    for path, plex, want in found.differ[:20]:
        print(f"  differs  {path}\n           Plex {plex!r}, library {want!r}")
    print(
        f"  {sum(found.locks.values())} locked field(s)" + "".join(f"\n    {n:5d}  {k}" for k, n in found.locks.items())
    )
    return found


def run_push(*, write: bool) -> None:
    """`plex push`: the library's ratings onto the rendering's items, and its playlists from them."""
    token, server = connect()
    lib = open_library()
    placed = placed_of(lib, manifest_of())
    section = section_of(server.get, PLEX_ROOT)
    tracks = tracks_of(server.get, section, PLEX_ROOT)
    ratings = ratings_to_push(tracks, placed)
    keys = keys_by_recording(tracks, placed)
    keepers = lib.keepers()
    held = {
        p.get("title", ""): Held(
            p.get("ratingKey", ""),
            # **gmuse's**: one `push` made, or one the library adopted (#349).
            p.get("summary") == SUMMARY or keepers.get(p.get("title", "")) == lib.device,
            [i.get("ratingKey", "") for i in every(server.get, f"/playlists/{p.get('ratingKey')}/items")],
        )
        for p in server.get("/playlists", {"playlistType": "audio"})
        if p.get("smart") == "0"
    }
    # **Never one Plex keeps**: that is the mirror's, read from Plex, not pushed to it.
    wanted = {n: recs for n, recs in lib.playlists().items() if keepers.get(n, lib.device) == lib.device}
    lists = {name: [keys[r] for r in recs if r in keys] for name, recs in wanted.items()}
    plan = plan_playlists(lists, held)
    said = {
        "create": "to create",
        "same": "already as the library has it",
        "resync": "drifted — to re-sync",
        "leave": "a Plex playlist of that name is not gmuse's — left",
    }
    print(f"  {len(ratings)} rating(s) to set")
    for action, name, ks in plan:
        print(f"  playlist {name!r}: {len(ks)} of {len(wanted[name])} entries in the rendering, {said[action]}")
    if not write:
        print("dry run — pass --write")
        return
    out = Writer(SERVER, token)
    for key, stars in ratings:
        out.rate(key, stars)
    machine = device_of(server.get).removeprefix("plex:")
    for action, name, ks in plan:
        if action == "create" and ks:
            out.playlist(machine, name, ks)
        elif action == "resync":
            out.resync(machine, held[name].key, ks)
    changed = sum(1 for a, _, ks in plan if (a == "create" and ks) or a == "resync")
    print(f"wrote {len(ratings)} rating(s); {changed} playlist(s) created or re-synced")


def run_refresh() -> None:
    """`plex refresh`: tell Plex to scan the rendering's library now.

    **Its own watcher does not see a push** (measured, 2026-10-25): after
    `just plex write` renamed 75 files over ssh, Plex sat idle with the old
    paths until asked. So the push asks.
    """
    token, server = connect()
    section = section_of(server.get, PLEX_ROOT)
    Writer(SERVER, token).refresh(section)
    print(f"  asked Plex to scan library {section}")


def register(plex_app: typer.Typer) -> None:
    """Attach `plex verify`, `plex push` and `plex refresh` to the `plex` group."""

    @plex_app.command("refresh")
    def refresh_cmd() -> None:
        """Ask Plex to scan the rendering's library now — what `just plex write` ends with."""
        run_refresh()

    @plex_app.command("verify")
    def verify_cmd() -> None:
        """Check Plex says what the library says about the rendering, and report locked fields."""
        run_verify()

    @plex_app.command("push")
    def push_cmd(*, write: bool = typer.Option(default=False, help="actually write; without this, a dry run")) -> None:
        """Give Plex the library's ratings, and create the library's playlists from the rendering's items."""
        run_push(write=write)
