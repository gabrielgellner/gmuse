"""Render a query of the library into a target directory — #332.

`docs/design/renderings.md` is the design. A rendering is **made from the
masters and the log and can be made again**: the tree Plex reads, a laptop's
few albums. This is the first step of it, and it only copies — no transcode,
no tags — so that the bookkeeping is right before anything expensive rides on
it.

```
gmuse-tools render TARGET [--query FILTER] [--write]
```

## The manifest is what makes a directory a rendering

`TARGET/.gmuse-rendering.json` records, for every path the renderer wrote,
which copy it was made from. Three things follow from it and nothing else
could give them:

- **Incremental.** A path whose manifest row already says what this run would
  write is not written again.
- **Deletes are safe.** Only a path the manifest names is ever removed, so a
  rendering can shrink without the renderer touching a file it did not make.
- **A directory is not adopted.** A target holding files and no manifest is
  refused: it is somebody's music, not a rendering.

## Never inside the library

§1: a rendering is never read back. A target under one of the library's roots
would be scanned, and the copies — later, the library's own tags — would come
back as evidence. So that is refused before anything is planned.

## What it does not do

It appends no facts. The manifest is the target's record, not the library's,
which is why this is a tool and not an operation (#200).
"""

from __future__ import annotations

import json
import shutil
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING, Any, Final

import typer

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable, Mapping

    import gmuse_core

#: The file in a target that makes it a rendering.
MANIFEST: Final = ".gmuse-rendering.json"

#: The manifest's own shape. A later step adds columns; a reader meeting a
#: version it does not know refuses rather than guessing.
VERSION: Final = 1

#: How often a long write saves the manifest, in files. An interrupted run
#: loses at most this many rows; the files themselves are complete, because
#: each lands by rename, and the next run reports them as not its own.
SAVE_EVERY: Final = 50


class Refused(RuntimeError):
    """A target this renderer will not write into, and why."""


@dataclass(frozen=True)
class Wanted:
    """One file the rendering should hold: where, made from what."""

    path: str
    #: The manifest row this file would have, compared whole: a later step's
    #: encoder key and tag hash join it, and any difference is a rewrite.
    row: dict[str, str]
    source: Path


@dataclass
class Plan:
    """Everything a run would do, decided before anything is touched."""

    writes: list[Wanted] = field(default_factory=list)
    unchanged: int = 0
    deletes: list[str] = field(default_factory=list)
    #: A wanted path where a file stands that the manifest does not name.
    strangers: list[str] = field(default_factory=list)
    #: `(encoding, why)` for a copy the layout gave no path.
    refused: list[tuple[str, str]] = field(default_factory=list)
    #: Recordings in the query this device holds no copy of.
    elsewhere: int = 0
    #: `(path, chosen file, other files)` where only an id chose between held copies —
    #: a pick, not a judgement, so it is reported and the library is not told.
    ties: list[tuple[str, str, list[str]]] = field(default_factory=list)


@dataclass
class Done:
    """What carrying a plan out did."""

    written: int = 0
    deleted: int = 0
    failed: list[tuple[str, str]] = field(default_factory=list)


def manifest_path(target: Path) -> Path:
    return target / MANIFEST


def load_manifest(target: Path) -> dict[str, Any] | None:
    """The target's manifest, `None` if it has none.

    **A target with files and no manifest is refused**, because that is a
    directory somebody else filled — the renderer never adopts what it did
    not make. An empty or missing directory is a new rendering.
    """
    at = manifest_path(target)
    if not at.is_file():
        if target.is_dir() and any(target.iterdir()):
            msg = f"{target} holds files and no {MANIFEST}: not a rendering, so not written into"
            raise Refused(msg)
        return None
    loaded = json.loads(at.read_text(encoding="utf-8"))
    if loaded.get("version") != VERSION:
        msg = f"{at} is manifest version {loaded.get('version')!r}; this renderer reads {VERSION}"
        raise Refused(msg)
    return loaded


def save_manifest(target: Path, query: str | None, files: Mapping[str, Mapping[str, str]]) -> None:
    """Write the manifest by rename, so a reader never sees half of one."""
    at = manifest_path(target)
    body = {"version": VERSION, "query": query, "files": dict(sorted(files.items()))}
    tmp = at.with_name(at.name + ".tmp")
    tmp.write_text(json.dumps(body, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    tmp.replace(at)


def check_outside(target: Path, roots: Iterable[Path]) -> None:
    """Refuse a target that is inside a library root, or holds one.

    Either way a scan would read the rendering, and a rendering read back is
    the library citing itself (`renderings.md` §1).
    """
    here = target.expanduser().resolve()
    for root in roots:
        lib = root.expanduser().resolve()
        if here == lib or here.is_relative_to(lib) or lib.is_relative_to(here):
            msg = f"{target} overlaps the library root {root}: a rendering is never scanned"
            raise Refused(msg)


def place(recording: str, path: str) -> tuple[str, str]:
    """The one place in a rendering that copies of a recording compete for.

    **The rendered path without its extension.** A FLAC and an MP3 of one
    track on one album differ only there, so they are one place and the
    better is kept. Two track numbers are two places even on one album — a
    disc really can play one recording twice (`Furious Angels` holds one
    instrumental at 1-08 and 2-06) — and grouping by folder dropped one.
    """
    return recording, str(PurePosixPath(path).with_suffix(""))


def wanted(
    rows: Iterable[Mapping[str, Any]],
    layout: Iterable[Mapping[str, Any]],
    choose: Callable[[list[list[str]]], list[dict[str, Any]]],
) -> tuple[list[Wanted], Plan]:
    """Join the query, the layout and the choice of copy: what this rendering should hold.

    `rows` is every copy the query matched (`every_copy=True`), and `layout`
    is where each held one would land.

    **What competes is one place, not one recording** (#333). The copies of a
    recording that would land in one [`place`] — a FLAC and an MP3 of one
    track — are one appearance, and `choose` keeps the best this device
    holds. The same recording on an album and on a compilation, or twice on
    one album, is two places, and both are rendered: an album with a track
    missing is not a rendering of that album.

    Returns the files wanted, and a plan carrying only what the join itself
    found — refusals, ties, and recordings held elsewhere.
    """
    out = Plan()
    recording: dict[str, str] = {}
    held: set[str] = set()
    for r in rows:
        enc = r.get("encoding")
        if enc and r.get("recording"):
            recording[enc] = r["recording"]
            if r.get("locator"):
                held.add(enc)
    unheld = {recording[e] for e in recording} - {recording[e] for e in held}
    out.elsewhere = len(unheld) + sum(1 for r in rows if not r.get("encoding"))

    places: dict[tuple[str, str], list[str]] = {}
    at: dict[str, str] = {}
    contested: dict[str, list[str]] = {}
    disagree = 0
    for placed in layout:
        enc = placed["encoding"]
        if placed.get("contested"):
            contested.setdefault(placed["path"], []).append(enc)
        elif "path" in placed:
            at[enc] = placed["path"]
            places.setdefault(place(recording[enc], placed["path"]), []).append(enc)
        elif "refused" in placed:
            out.refused.append((enc, placed["refused"]))
        else:
            # The query located it here and the layout says it is not.
            disagree += 1

    # **A contest between copies of one recording is the choice, not a
    # conflict.** Two of its copies wanting one path are one appearance — the
    # library's `Enya` and `The Celts` copies of one track, here, fourteen
    # times — and the better is rendered. Copies of two recordings wanting
    # one path are a real collision, and both stay refused.
    for path, encs in contested.items():
        if len({recording[e] for e in encs}) == 1:
            for e in encs:
                at[e] = path
            places.setdefault(place(recording[encs[0]], path), []).extend(encs)
        else:
            out.refused.extend((e, f"two recordings want {path}") for e in encs)

    groups = list(places.values())
    files: list[Wanted] = []
    # `strict`: one answer per group, or the pairing below is a guess.
    for _, c in zip(groups, choose(groups), strict=True):
        enc, locator = c.get("encoding"), c.get("locator")
        if not enc or not locator:
            # Every copy in the group was located here, so "none held" is the
            # library contradicting itself.
            disagree += 1
            continue
        path = at[enc]
        files.append(Wanted(path, {"encoding": enc}, Path(locator)))
        if c.get("tied"):
            others = [where or e for where, e in zip(c.get("tied_at") or [], c["tied"], strict=False)]
            out.ties.append((path, locator, others or list(c["tied"])))

    # **Three routes to "is it here"**, so they can disagree, and when they do
    # the rendering is wrong in silence: the first `layout` built its librarian
    # without naming this device and called every copy on the machine "held
    # elsewhere" — an empty rendering, and a `--write` that would have deleted
    # all of a full one.
    if disagree:
        msg = (
            f"the library says {disagree} copies are on this device and then that they are not; "
            "a rendering built on that would be missing them. Rebuild gmuse_core (`just wheel-dev`)"
        )
        raise Refused(msg)
    return files, out


def plan(target: Path, files: Iterable[Wanted], held: Mapping[str, Mapping[str, str]], base: Plan) -> Plan:
    """Compare what the rendering should hold with what its manifest says it does."""
    out = base
    keep: set[str] = set()
    for f in files:
        keep.add(f.path)
        at = target / f.path
        if held.get(f.path) == f.row and at.is_file():
            out.unchanged += 1
        elif f.path not in held and at.exists():
            out.strangers.append(f.path)
        else:
            out.writes.append(f)
    out.deletes = sorted(p for p in held if p not in keep)
    return out


def carry_out(
    target: Path, p: Plan, held: dict[str, dict[str, str]], query: str | None, *, write: bool
) -> Done:
    """Do the plan. Deletes first, then writes; the manifest last, and as it goes.

    **Deletes first** because a case-only rename on a case-folding
    filesystem is one file: writing `a/x` and then deleting `a/X` would delete
    the file just written.
    """
    out = Done()
    if not write:
        out.deleted = len(p.deletes)
        out.written = len(p.writes)
        return out

    target.mkdir(parents=True, exist_ok=True)
    try:
        for path in p.deletes:
            at = target / path
            try:
                at.unlink(missing_ok=True)
            except OSError as e:
                out.failed.append((path, str(e)))
                continue
            held.pop(path, None)
            out.deleted += 1
            prune(at.parent, target)

        for n, f in enumerate(p.writes, 1):
            at = target / f.path
            try:
                at.parent.mkdir(parents=True, exist_ok=True)
                # **Through a temporary and a rename**, so the path holds the
                # old file or the new one and never part of either.
                tmp = at.with_name(f".{at.name}.rendering")
                shutil.copy2(f.source, tmp)
                tmp.replace(at)
            except OSError as e:
                out.failed.append((f.path, str(e)))
                continue
            held[f.path] = f.row
            out.written += 1
            if n % SAVE_EVERY == 0:
                save_manifest(target, query, held)
    finally:
        save_manifest(target, query, held)
    return out


def prune(folder: Path, target: Path) -> None:
    """Take away directories a delete left empty, up to the target."""
    while folder != target and folder.is_relative_to(target):
        try:
            folder.rmdir()
        except OSError:
            return
        folder = folder.parent


def render(lib: gmuse_core.Library, target: Path, query: str | None, *, write: bool) -> Done:
    """Plan a rendering of `query` into `target`, say what it would do, and do it if told."""
    check_outside(target, lib.roots)
    manifest = load_manifest(target)
    held: dict[str, dict[str, str]] = dict(manifest["files"]) if manifest else {}

    # **A target remembers its query**, so rendering a work laptop's subset
    # again does not need it retyped — and changing it is said out loud,
    # because narrowing a query is deleting from that device.
    was = manifest["query"] if manifest else None
    if query is None:
        query = was
    elif manifest and query != was:
        print(f"query changes: {was!r} → {query!r}")

    rows = lib.query(query, every_copy=True)
    on_device = list(dict.fromkeys(r["encoding"] for r in rows if r.get("encoding") and r.get("locator")))
    files, base = wanted(rows, lib.layout(on_device), lib.choose)
    p = plan(target, files, held, base)

    print(f"rendering {query or 'the whole library'} into {target}")
    print(f"  {p.unchanged} unchanged, {len(p.writes)} to write, {len(p.deletes)} to delete")
    if p.elsewhere:
        print(f"  {p.elsewhere} in the query with no copy on this device, left out")
    for path in p.deletes:
        print(f"  delete  {path}")
    for path in p.strangers:
        print(f"  not ours, left alone  {path}")
    for enc, why in p.refused:
        print(f"  refused  {enc}  {why}")
    if p.ties:
        # **Said, not settled.** Which of two equal copies to keep is a
        # question for a person (`CLAUDE.md`, reported, never decided); the
        # rendering needs one and takes the id's, and neither is retired.
        print(f"  {len(p.ties)} chosen by id alone among equal copies held here:")
        for path, took, others in p.ties:
            print(f"  tie  {path}\n         took  {took}")
            for other in others:
                print(f"         over  {other}")

    done = carry_out(target, p, held, query, write=write)
    for path, why in done.failed:
        print(f"  failed  {path}  {why}")
    if not write:
        print("dry run — nothing written. Read the deletes above, then --write.")
    else:
        print(f"wrote {done.written}, deleted {done.deleted}, {len(done.failed)} failed")
    return done


def register(app: typer.Typer) -> None:
    """Attach the renderer."""

    @app.command("render")
    def render_cmd(
        target: Path = typer.Argument(..., help="the directory to render into"),
        query: str | None = typer.Option(
            None, help="a filter, as the `:` line takes one; defaults to the target's own, else everything"
        ),
        *,
        write: bool = typer.Option(default=False, help="actually write; without this, a dry run"),
    ) -> None:
        """Render a query of the library into a directory, copying only."""
        from gmuse_tools.library import open_library

        try:
            done = render(open_library(), target.expanduser(), query, write=write)
        except Refused as e:
            print(f"refused: {e}")
            raise typer.Exit(1) from e
        if done.failed:
            raise typer.Exit(1)
