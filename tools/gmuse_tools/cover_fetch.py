"""Album art from the Cover Art Archive, by the release the library names — #352.

#348 carries `cover.jpg` into a rendering, so an album without one is blank in
Plexamp and a thumbnail shows as one. Measured 2026-10-24: 32 album folders
with no cover whose release is identified, 25 not identified yet, and 78
covers under 500 px. The Cover Art Archive holds MusicBrainz's art keyed by
the release MBIDs the library already states for every copy.

```
uv run gmuse-tools covers fetch                 # -> out/covers-fetch.tsv, out/covers-fetch/index.html
uv run gmuse-tools covers fetch-apply           # dry run
uv run gmuse-tools covers fetch-apply --write
```

## Into the masters, beside the album

Where `covers apply` writes, and for the same reason: from there the archive
copies it and every rendering carries it. Art that lived only in a rendering
could not be rebuilt from the masters and the log.

## A person looks first

The art is an authority's answer about **the release the library names**, and
the library is sometimes wrong about which release that is
(`identification-lessons.md`). So `fetch` writes nothing into the library: it
downloads each offer into `out/covers-fetch/`, writes a review file with an
empty `apply` column, and a contact sheet showing what is there now beside
what is offered. `fetch-apply` writes the rows marked `y`.

## An upgrade keeps what it replaces

The old cover becomes `cover-small.<ext>`, beside the new one. A low-resolution
cover may be the right art for that pressing and the archive's another
edition's; renaming costs nothing and deleting would cost the only copy. The
rendering carries `cover.*` only (#348), so the kept one stays home.

## The pressing, then its group

A release with no art in the archive falls back to its release group's front,
found through MusicBrainz — a reissue often has none of its own. Every answer
goes through `bronze`, so a second survey asks nothing it asked before, and
under the project's User-Agent: the repository's URL, never a person's address.
"""

from __future__ import annotations

import collections
import csv
import html
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Final

import requests
import typer

from gmuse_tools import bronze
from gmuse_tools.covers import COVER_NAMES, TRUTHY, dimensions, image_suffix, is_album_level
from gmuse_tools.genre.common import out_path, write_tsv
from gmuse_tools.library import open_library

if TYPE_CHECKING:
    import sqlite3
    from collections.abc import Callable, Iterable

    import gmuse_core

USER_AGENT: Final = {"User-Agent": "gmuse-covers/0.1 ( https://github.com/gabrielgellner/gmuse )"}
CAA: Final = "https://coverartarchive.org"
MB_RELEASE: Final = "https://musicbrainz.org/ws/2/release/{}"

#: Below this on the long side a cover is offered an upgrade. Plexamp draws an
#: album at well over this on a phone, and the archive's sizes are 250, 500
#: and 1,200.
UPGRADE_PX: Final = 500

#: The archive's thumbnail sizes, best first: 1,200 is plenty for any screen
#: here, and an original can be a multi-megabyte scan.
SIZES: Final = ("1200", "large", "500")

#: How long a download may take before it is a failure rather than slow.
TIMEOUT_S: Final = 60

#: The review file's columns, so a survey that finds nothing still writes them
#: — and does not leave the last run's rows standing as if they were current.
COLUMNS: Final = ["path", "action", "apply", "current", "offered", "release", "via", "image", "why"]


@dataclass(frozen=True)
class Album:
    """One master album folder: where it is, which release it is, what cover it has."""

    folder: Path
    release: str
    cover: Path | None


@dataclass(frozen=True)
class Offer:
    """What the archive has for a release: an image's address, and whether it came by the group."""

    url: str
    via: str


def albums(lib: gmuse_core.Library, root: Path) -> list[Album]:
    """Every master album folder holding a copy whose release the library names.

    **The release most of its copies are on.** A folder can hold copies tagged
    from two pressings (Orbital's *Diversions* here); the art is the album's,
    and the majority is the album.
    """
    rows = [r for r in lib.query(None, every_copy=True) if r.get("locator") and r.get("encoding")]
    said = lib.statement([r["encoding"] for r in rows])
    votes: dict[Path, collections.Counter[str]] = collections.defaultdict(collections.Counter)
    for r, s in zip(rows, said, strict=True):
        folder = Path(r["locator"]).parent
        if s.get("musicbrainz_release") and is_album_level(root, folder):
            votes[folder][s["musicbrainz_release"]] += 1
    out = []
    for folder, counted in sorted(votes.items()):
        cover = next((folder / n for n in sorted(COVER_NAMES) if (folder / n).is_file()), None)
        out.append(Album(folder, counted.most_common(1)[0][0], cover))
    return out


def wants_art(album: Album) -> str | None:
    """`add`, `upgrade`, or `None` where the cover it has is good enough."""
    if album.cover is None:
        return "add"
    size = dimensions(album.cover.read_bytes())
    return "upgrade" if size is not None and max(size) < UPGRADE_PX else None


def front(body: object) -> str | None:
    """The best front image in an archive listing."""
    images = body.get("images", []) if isinstance(body, dict) else []
    for image in images:
        if image.get("front"):
            thumbs = image.get("thumbnails") or {}
            return next((thumbs[s] for s in SIZES if thumbs.get(s)), image.get("image"))
    return None


def lookup(con: sqlite3.Connection, release: str) -> Offer | None:
    """The archive's front for this release, or failing that its release group's."""
    status, body = bronze.get(con, f"{CAA}/release/{release}", headers=USER_AGENT)
    if status == requests.codes.ok and (url := front(body)):
        return Offer(url, "release")
    status, body = bronze.get(
        con, MB_RELEASE.format(release), {"inc": "release-groups", "fmt": "json"}, headers=USER_AGENT
    )
    group = (body.get("release-group") or {}).get("id") if isinstance(body, dict) else None
    if status != requests.codes.ok or not group:
        return None
    status, body = bronze.get(con, f"{CAA}/release-group/{group}", headers=USER_AGENT)
    if status == requests.codes.ok and (url := front(body)):
        return Offer(url, "release-group")
    return None


def download(url: str) -> bytes:
    """An image's bytes; the archive redirects to where it is stored."""
    got = requests.get(url, headers=USER_AGENT, timeout=TIMEOUT_S)
    got.raise_for_status()
    return got.content


def survey(
    found: Iterable[Album],
    out: Path,
    staged: Path,
    *,
    look: Callable[[str], Offer | None],
    fetch: Callable[[str], bytes],
) -> list[dict[str, object]]:
    """Find an offer for every album that wants art, download it, and write the review. Changes no master."""
    staged.mkdir(parents=True, exist_ok=True)
    rows: list[dict[str, object]] = []
    tally: collections.Counter[str] = collections.Counter()
    for album in found:
        action = wants_art(album)
        if action is None:
            continue
        row = review_row(album, action, staged, look=look, fetch=fetch)
        tally[f"{action}: {row['why'] or 'offered'}"] += 1
        rows.append(row)
    write_tsv(out, rows, COLUMNS)
    contact_sheet(staged / "index.html", rows)
    for key, n in sorted(tally.items()):
        print(f"  {n:6d}  {key}")
    print(f"\n{len(rows)} album(s) -> {out}\nlook at {staged / 'index.html'}, mark `apply` with y, then fetch-apply")
    return rows


def review_row(
    album: Album,
    action: str,
    staged: Path,
    *,
    look: Callable[[str], Offer | None],
    fetch: Callable[[str], bytes],
) -> dict[str, object]:
    """One album's line in the review: what it has, what the archive offers, and why not if nothing."""
    current = dimensions(album.cover.read_bytes()) if album.cover else None
    row: dict[str, object] = {
        "path": str(album.folder),
        "action": action,
        "apply": "",
        "current": f"{current[0]}x{current[1]}" if current else "",
        "offered": "",
        "release": album.release,
        "via": "",
        "image": "",
        "why": "",
    }
    offer = look(album.release)
    if offer is None:
        row["why"] = "the archive has no front for the release or its group"
        return row
    # **Downloaded once.** A second survey is all `bronze` hits for the
    # lookups, and the image it already staged is the same answer.
    kept = next((p for p in (staged / f"{album.release}{s}" for s in (".jpg", ".png")) if p.is_file()), None)
    try:
        art = kept.read_bytes() if kept else fetch(offer.url)
    except requests.RequestException as e:
        row["why"] = f"download failed: {type(e).__name__}"
        return row
    suffix, size = image_suffix(art), dimensions(art)
    if suffix is None or size is None:
        row["why"] = "the archive's image is not a JPEG or PNG"
        return row
    if current is not None and max(size) <= max(current):
        row["why"] = f"the archive's is no larger ({size[0]}x{size[1]})"
        return row
    at = staged / f"{album.release}{suffix}"
    at.write_bytes(art)
    row.update(offered=f"{size[0]}x{size[1]}", via=offer.via, image=str(at))
    return row


def contact_sheet(at: Path, rows: Iterable[dict[str, object]]) -> None:
    """A page showing each album's cover now beside what is offered — the review is of pictures."""
    cells = []
    for r in rows:
        if not r["image"]:
            continue
        now = next(
            (Path(str(r["path"])) / n for n in sorted(COVER_NAMES) if (Path(str(r["path"])) / n).is_file()), None
        )
        before = f'<img src="{html.escape(now.as_uri())}">' if now else "<div class=none>none</div>"
        after = f'<img src="{html.escape(Path(str(r["image"])).as_uri())}">'
        cells.append(
            f"<figure>{before}{after}<figcaption>{html.escape(str(r['path']))}<br>"
            f"{html.escape(str(r['action']))} · {html.escape(str(r['current']) or '-')} → "
            f"{html.escape(str(r['offered']))} · via {html.escape(str(r['via']))}</figcaption></figure>"
        )
    at.write_text(
        "<!doctype html><meta charset=utf-8><title>Covers offered</title><style>"
        "body{font:14px system-ui;background:#fff;color:#111;margin:16px}"
        "figure{display:inline-block;margin:8px;width:420px;vertical-align:top}"
        "img,.none{width:200px;height:200px;object-fit:contain;background:#eee;margin-right:8px}"
        ".none{display:inline-flex;align-items:center;justify-content:center;color:#888}"
        "@media (prefers-color-scheme:dark){body{background:#111;color:#eee}img,.none{background:#222}}"
        "</style>" + "".join(cells),
        encoding="utf-8",
    )


def apply(review: Path, *, write: bool) -> collections.Counter[str]:
    """Write the covers for the rows marked `y`. Dry run unless `--write`."""
    with review.open(encoding="utf-8") as handle:
        marked = [r for r in csv.DictReader(handle, delimiter="\t") if r["apply"].strip().lower() in TRUTHY]
    tally: collections.Counter[str] = collections.Counter()
    for row in marked:
        tally[put(row, write=write)] += 1
    for key, n in sorted(tally.items()):
        print(f"  {n:6d}  {key}")
    if not write:
        print("dry run — pass --write to write the files")
    return tally


def put(row: dict[str, str], *, write: bool) -> str:
    """Write one marked row's cover, or say why not. The one place that touches a master."""
    folder, image = Path(row["path"]), Path(row["image"]) if row["image"] else None
    if image is None or not image.is_file():
        return "nothing was downloaded for it"
    art = image.read_bytes()
    suffix = image_suffix(art)
    if suffix is None:
        return "the downloaded image is not a JPEG or PNG"
    there = [folder / n for n in sorted(COVER_NAMES) if (folder / n).is_file()]
    if row["action"] == "add" and there:
        return "a cover appeared since the survey; left alone"
    if row["action"] == "upgrade":
        if len(there) != 1:
            return "expected one cover to keep; left alone"
        kept = folder / f"cover-small{there[0].suffix}"
        if kept.exists():
            return f"{kept.name} is already there; left alone"
        if write:
            there[0].rename(kept)
    if write:
        tmp = folder / f".cover{suffix}.fetched"
        tmp.write_bytes(art)
        tmp.replace(folder / f"cover{suffix}")
    return f"{'' if write else 'would '}{'add' if row['action'] == 'add' else 'upgrade'} cover{suffix}"


def register(covers_app: typer.Typer) -> None:
    """Attach `fetch` and `fetch-apply` to the `covers` group."""

    @covers_app.command("fetch")
    def fetch_cmd() -> None:
        """Offer the Cover Art Archive's front for albums with no cover or a small one. Changes no master."""
        lib = open_library()
        con = bronze.open_cache()
        root = lib.roots[0]
        survey(
            albums(lib, root),
            out_path("covers-fetch.tsv"),
            out_path("covers-fetch"),
            look=lambda release: lookup(con, release),
            fetch=download,
        )

    @covers_app.command("fetch-apply")
    def fetch_apply_cmd(
        *, write: bool = typer.Option(default=False, help="actually write; without this, a dry run")
    ) -> None:
        """Write the archive's cover for each row marked y; an upgrade keeps the old one as cover-small."""
        review = out_path("covers-fetch.tsv")
        if not review.exists():
            print(f"no {review.name} — run `covers fetch` first")
            raise typer.Exit(1)
        apply(review, write=write)
