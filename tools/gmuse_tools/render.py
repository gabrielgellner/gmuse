"""Render a query of the library into a target directory — #332, #333, #334.

`docs/design/renderings.md` is the design. A rendering is **made from the
masters and the log and can be made again**: the tree Plex reads, a laptop's
few albums. It copies the best copy of each track this device holds, and
transcodes the lossless ones when the target asks for it. It writes no tags
yet (#335).

```
gmuse-tools render TARGET [--query FILTER] [--encode mp3-320|mp3-v0|copy] [--write]
```

## The manifest is what makes a directory a rendering

`TARGET/.gmuse-rendering.json` records, for every path the renderer wrote,
which copy it was made from and how. Three things follow from it and nothing
else could give them:

- **Incremental.** A path whose manifest row already says what this run would
  write is not written again.
- **Deletes are safe.** Only a path the manifest names is ever removed, so a
  rendering can shrink without the renderer touching a file it did not make.
- **A directory is not adopted.** A target holding files and no manifest is
  refused: it is somebody's music, not a rendering.

## Lossless is transcoded on render, and only lossless

A target names an encode policy, and remembers it like its query. A lossless
copy is encoded to it; a lossy one is copied as it is, because a Vorbis file
made into an MP3 loses twice for nothing. Each encode is kept in a cache keyed
by the source's audio id and the encoder's settings, so a FLAC is encoded once
however many targets want it, and changing the setting changes the key.

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
import os
import shutil
import subprocess
import unicodedata
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING, Any, Final

import typer

from gmuse_tools.bronze import cache_dir
from gmuse_tools.library import open_library

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable, Mapping

    import gmuse_core

#: The file in a target that makes it a rendering.
MANIFEST: Final = ".gmuse-rendering.json"

#: The manifest's own shape. A reader meeting a version it does not know
#: refuses rather than guessing; a column added later reads as absent.
VERSION: Final = 1

#: How often a long write saves the manifest, in files. An interrupted run
#: loses at most this many rows; the files themselves are complete, because
#: each lands by rename, and the next run reports them as not its own.
SAVE_EVERY: Final = 50


class Refused(RuntimeError):  # noqa: N818 — `storage::Refused`'s word, and this is the same refusal
    """A target this renderer will not write into, and why."""


@dataclass(frozen=True)
class Encode:
    """An encode policy: what a lossless copy becomes in a rendering."""

    name: str
    #: **The encoder and its settings, and nothing else** — the cache key and
    #: the manifest's `encoder` column. Not the encoder's version: an upgraded
    #: LAME makes the same kind of file, and keying on it would re-render a
    #: library on every `brew upgrade`.
    key: str
    suffix: str
    args: tuple[str, ...]


#: The policies a target can name. 320 kbps CBR is what Plexamp was asked for;
#: V0 is smaller at the same transparency. Either is only a key, so moving
#: between them costs one re-render and nothing else.
ENCODES: Final = {
    e.name: e
    for e in (
        Encode("mp3-320", "libmp3lame-cbr320", ".mp3", ("-c:a", "libmp3lame", "-b:a", "320k")),
        Encode("mp3-v0", "libmp3lame-v0", ".mp3", ("-c:a", "libmp3lame", "-q:a", "0")),
    )
}

#: What a target that names no policy does: copies, as before #334.
COPY: Final = "copy"

#: How far an encode's length may stray from its source's before it is a
#: broken file rather than a different one. LAME pads and trims a frame or two.
LENGTH_SLACK_MS: Final = 1000


@dataclass(frozen=True)
class Settings:
    """What a target remembers between runs: which recordings, and what lossless becomes."""

    query: str | None = None
    encode: str = COPY


@dataclass(frozen=True)
class Wanted:
    """One file the rendering should hold: where, made from what."""

    path: str
    #: The manifest row this file would have, compared whole: the encoder key
    #: and, later, a tag hash join it, and any difference is a rewrite.
    row: dict[str, str]
    source: Path
    #: The policy this file is encoded to, `None` where it is a copy.
    encode: Encode | None = None
    length_ms: int | None = None


@dataclass(frozen=True)
class Transcoding:
    """Where encodes are kept, and what makes one."""

    cache: Path
    run: Callable[[Path, Path, Encode, int | None], None]
    workers: int = os.cpu_count() or 4

    def at(self, f: Wanted) -> Path | None:
        """Where the encode of this file's source under its policy is kept, `None` for a copy."""
        if f.encode is None:
            return None
        return self.cache / f.encode.key / f"{f.row['encoding']}{f.encode.suffix}"


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
class Job:
    """One run against one target: where, as what, holding what, encoding with what."""

    target: Path
    settings: Settings
    #: The manifest's files, updated as the run writes and deletes.
    held: dict[str, dict[str, str]]
    transcoding: Transcoding | None = None


@dataclass
class Done:
    """What carrying a plan out did."""

    written: int = 0
    deleted: int = 0
    failed: list[tuple[str, str]] = field(default_factory=list)


def manifest_path(target: Path) -> Path:
    """Where a target keeps the record that makes it a rendering."""
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


def save_manifest(target: Path, settings: Settings, files: Mapping[str, Mapping[str, str]]) -> None:
    """Write the manifest by rename, so a reader never sees half of one."""
    at = manifest_path(target)
    body = {
        "version": VERSION,
        "query": settings.query,
        "encode": settings.encode,
        "files": dict(sorted(files.items())),
    }
    tmp = at.with_name(at.name + ".tmp")
    tmp.write_text(json.dumps(body, indent=1, ensure_ascii=False) + "\n", encoding="utf-8")
    tmp.replace(at)


def settle(manifest: Mapping[str, Any] | None, query: str | None, encode: str | None) -> Settings:
    """What this run renders: what was asked, else what the target remembers.

    **A target remembers its query and its encode policy**, so rendering a
    work laptop's subset again does not need either retyped — and changing
    one is said out loud, because narrowing a query deletes from that device
    and changing the policy rewrites every transcoded file in it.
    """
    was = Settings(manifest.get("query"), manifest.get("encode") or COPY) if manifest else Settings()
    now = Settings(was.query if query is None else query, was.encode if encode is None else encode)
    if manifest and now.query != was.query:
        print(f"query changes: {was.query!r} → {now.query!r}")
    if manifest and now.encode != was.encode:
        print(f"encode changes: {was.encode} → {now.encode}")
    if now.encode != COPY and now.encode not in ENCODES:
        msg = f"no encode policy {now.encode!r}; there is {COPY}, {', '.join(ENCODES)}"
        raise Refused(msg)
    return now


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


def recordings_of(rows: Iterable[Mapping[str, Any]], out: Plan) -> dict[str, str]:
    """Which recording each copy is of, counting into `out` the recordings held nowhere here."""
    recording: dict[str, str] = {}
    held: set[str] = set()
    for r in rows:
        enc = r.get("encoding")
        if enc and r.get("recording"):
            recording[enc] = r["recording"]
            if r.get("locator"):
                held.add(enc)
        elif not enc:
            out.elsewhere += 1
    out.elsewhere += len(set(recording.values()) - {recording[e] for e in held})
    return recording


@dataclass
class Places:
    """The layout's answer, grouped: the copies competing for each place, and where each lands."""

    groups: dict[tuple[str, str], list[str]] = field(default_factory=dict)
    at: dict[str, str] = field(default_factory=dict)
    #: Copies the query located here and the layout did not.
    disagree: int = 0

    def add(self, recording: str, path: str, encodings: Iterable[str]) -> None:
        """Put copies of `recording` at `path`, competing with any already there."""
        for e in encodings:
            self.at[e] = path
            self.groups.setdefault(place(recording, path), []).append(e)


def places_of(layout: Iterable[Mapping[str, Any]], recording: Mapping[str, str], out: Plan) -> Places:
    """Group the layout by [`place`], resolving the contests that are only a choice."""
    found = Places()
    contested: dict[str, list[str]] = {}
    for placed in layout:
        enc = placed["encoding"]
        if placed.get("contested"):
            contested.setdefault(placed["path"], []).append(enc)
        elif "path" in placed:
            found.add(recording[enc], placed["path"], [enc])
        elif "refused" in placed:
            out.refused.append((enc, placed["refused"]))
        else:
            found.disagree += 1

    # **A contest between copies of one recording is the choice, not a
    # conflict.** Two of its copies wanting one path are one appearance — the
    # library's `Enya` and `The Celts` copies of one track, here, fourteen
    # times — and the better is rendered. Copies of two recordings wanting
    # one path are a real collision, and both stay refused.
    for path, encs in contested.items():
        recordings = {recording[e] for e in encs}
        if len(recordings) == 1:
            found.add(recordings.pop(), path, encs)
        else:
            out.refused.extend((e, f"two recordings want {path}") for e in encs)
    return found


def wanted(
    rows: Iterable[Mapping[str, Any]],
    layout: Iterable[Mapping[str, Any]],
    choose: Callable[[list[list[str]]], list[dict[str, Any]]],
    encode: Encode | None = None,
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
    found = places_of(layout, recordings_of(rows, out), out)
    groups = list(found.groups.values())
    files: list[Wanted] = []
    disagree = found.disagree
    # `strict`: one answer per group, or the pairing below is a guess.
    for _, c in zip(groups, choose(groups), strict=True):
        enc, locator = c.get("encoding"), c.get("locator")
        if not enc or not locator:
            # Every copy in the group was located here, so "none held" is the
            # library contradicting itself.
            disagree += 1
            continue
        files.append(one_file(found.at[enc], c, encode))
        if c.get("tied"):
            others = [where or e for where, e in zip(c.get("tied_at") or [], c["tied"], strict=False)]
            out.ties.append((files[-1].path, locator, others or list(c["tied"])))

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
    return uncollided(files, out), out


def one_file(path: str, c: Mapping[str, Any], encode: Encode | None) -> Wanted:
    """The file a chosen copy becomes: itself, or its encode under the target's policy."""
    if encode is not None and "lossless" not in c:
        # **Absent is not `False`.** A `gmuse_core` built before #334 says
        # nothing here, and reading that as "lossy" copies every FLAC without
        # a word — measured: a full run planned 0 transcodes. Both builds call
        # themselves 0.2.1, so `open_library`'s version check cannot see it.
        msg = "gmuse_core does not say which copies are lossless; rebuild it (`just wheel-dev`)"
        raise Refused(msg)
    if encode is None or not c["lossless"]:
        return Wanted(path, {"encoding": c["encoding"]}, Path(c["locator"]))
    # **The format's word, not the extension's**: `lossless` comes from the
    # library, which read the stream.
    row = {"encoding": c["encoding"], "encoder": encode.key}
    return Wanted(
        str(PurePosixPath(path).with_suffix(encode.suffix)), row, Path(c["locator"]), encode, c.get("length_ms")
    )


def folded(path: str) -> str:
    """A path as a case- and form-folding filesystem matches it."""
    return unicodedata.normalize("NFC", path).casefold()


def uncollided(files: list[Wanted], out: Plan) -> list[Wanted]:
    """Refuse two files that would land on one path, both of them.

    **A transcode changes the extension, so it can collide where the layout
    saw nothing.** `05 Lush.flac` becomes `05 Lush.mp3`, which the layout never
    compared with another recording's `05 Lush.mp3`. Neither is written:
    picking one would be a silent choice between two recordings.
    """
    count: dict[str, int] = {}
    for f in files:
        count[folded(f.path)] = count.get(folded(f.path), 0) + 1
    kept = []
    for f in files:
        if count[folded(f.path)] > 1:
            out.refused.append((f.row["encoding"], f"two files want {f.path}"))
        else:
            kept.append(f)
    return kept


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


def fill_cache(writes: Iterable[Wanted], t: Transcoding) -> dict[str, str]:
    """Encode every source the writes need and the cache lacks, in parallel.

    Returns `{path: why}` for each write whose encode failed. Each encode lands
    by rename, so the cache holds whole encodes or none — an interrupted run
    leaves nothing a later one could mistake for finished.
    """
    needs = [(t.at(f), f) for f in writes]
    todo = {dest: f for dest, f in needs if dest is not None and not dest.is_file()}

    def one(dest: Path, f: Wanted) -> str | None:
        if f.encode is None:
            return None
        dest.parent.mkdir(parents=True, exist_ok=True)
        tmp = dest.with_name(f".{dest.stem}.part{dest.suffix}")
        try:
            t.run(f.source, tmp, f.encode, f.length_ms)
            tmp.replace(dest)
        except (OSError, subprocess.SubprocessError, ValueError) as e:
            tmp.unlink(missing_ok=True)
            return str(e)
        return None

    with ThreadPoolExecutor(max_workers=t.workers) as pool:
        why = dict(zip(todo, pool.map(lambda kv: one(*kv), todo.items()), strict=True))
    # A failed encode fails every path that wanted that source, not only the first.
    return {f.path: why[dest] for dest, f in needs if dest is not None and why.get(dest)}


def ffmpeg(source: Path, dest: Path, encode: Encode, length_ms: int | None) -> None:
    """Encode with ffmpeg, then check the result is as long as the source.

    **Audio only** (`-map 0:a`): embedded art travels as a video stream, and
    cover art is a release's, which a later step carries beside the album.
    """
    subprocess.run(
        ["ffmpeg", "-nostdin", "-v", "error", "-y", "-i", str(source), "-map", "0:a", *encode.args, str(dest)],
        check=True,
        capture_output=True,
    )
    if length_ms is not None:
        check_length(dest, length_ms, probe_ms(dest))


def probe_ms(path: Path) -> int:
    """How long `ffprobe` says a file plays, in milliseconds."""
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", str(path)],
        check=True,
        capture_output=True,
        text=True,
    )
    return round(float(out.stdout.strip()) * 1000)


def check_length(path: Path, want_ms: int, got_ms: int) -> None:
    """Refuse an encode that came back a different length from its source."""
    if abs(got_ms - want_ms) > LENGTH_SLACK_MS:
        msg = f"{path.name} plays {got_ms} ms where its source plays {want_ms}"
        raise ValueError(msg)


def carry_out(job: Job, p: Plan, *, write: bool) -> Done:
    """Do the plan. Encodes first, deletes next, then writes; the manifest last, and as it goes.

    **Encodes first**, so a write never waits on one and a failed encode is
    known before anything in the target moves. **Deletes before writes**
    because a case-only rename on a case-folding filesystem is one file:
    writing `a/x` and then deleting `a/X` would delete the file just written.
    """
    out = Done()
    if not write:
        out.deleted = len(p.deletes)
        out.written = len(p.writes)
        return out

    failed: dict[str, str] = {}
    if any(f.encode is not None for f in p.writes):
        if job.transcoding is None:
            msg = "this rendering transcodes, and nothing was given to transcode with"
            raise Refused(msg)
        failed = fill_cache(p.writes, job.transcoding)

    job.target.mkdir(parents=True, exist_ok=True)
    try:
        for path in p.deletes:
            at = job.target / path
            try:
                at.unlink(missing_ok=True)
            except OSError as e:
                out.failed.append((path, str(e)))
                continue
            job.held.pop(path, None)
            out.deleted += 1
            prune(at.parent, job.target)

        for n, f in enumerate(p.writes, 1):
            if f.path in failed:
                out.failed.append((f.path, failed[f.path]))
                continue
            if write_one(job, f, out):
                out.written += 1
            if n % SAVE_EVERY == 0:
                save_manifest(job.target, job.settings, job.held)
    finally:
        save_manifest(job.target, job.settings, job.held)
    return out


def write_one(job: Job, f: Wanted, out: Done) -> bool:
    """Put one file in place — from the master, or from the cache for an encode."""
    at = job.target / f.path
    source = (job.transcoding.at(f) if job.transcoding else None) or f.source
    try:
        at.parent.mkdir(parents=True, exist_ok=True)
        # **Through a temporary and a rename**, so the path holds the old
        # file or the new one and never part of either.
        tmp = at.with_name(f".{at.name}.rendering")
        shutil.copy2(source, tmp)
        tmp.replace(at)
    except OSError as e:
        out.failed.append((f.path, str(e)))
        return False
    job.held[f.path] = f.row
    return True


def prune(folder: Path, target: Path) -> None:
    """Take away directories a delete left empty, up to the target."""
    while folder != target and folder.is_relative_to(target):
        try:
            folder.rmdir()
        except OSError:
            return
        folder = folder.parent


def report(job: Job, p: Plan) -> None:
    """Say what a run would do, before it does any of it."""
    s = job.settings
    print(f"rendering {s.query or 'the whole library'} into {job.target}, lossless as {s.encode}")
    print(f"  {p.unchanged} unchanged, {len(p.writes)} to write, {len(p.deletes)} to delete")
    encodes = {job.transcoding.at(f) for f in p.writes} - {None} if job.transcoding else set()
    if encodes:
        fresh = sum(1 for at in encodes if at is not None and not at.is_file())
        print(f"  {len(encodes)} of the writes are transcodes, {fresh} not yet in the cache")
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


def render(  # noqa: PLR0913 — the command's options, one for one
    lib: gmuse_core.Library,
    target: Path,
    query: str | None,
    *,
    write: bool,
    encode: str | None = None,
    transcoding: Transcoding | None = None,
) -> Done:
    """Plan a rendering of `query` into `target`, say what it would do, and do it if told."""
    check_outside(target, lib.roots)
    manifest = load_manifest(target)
    job = Job(target, settle(manifest, query, encode), dict(manifest["files"]) if manifest else {}, transcoding)

    rows = lib.query(job.settings.query, every_copy=True)
    on_device = list(dict.fromkeys(r["encoding"] for r in rows if r.get("encoding") and r.get("locator")))
    files, base = wanted(rows, lib.layout(on_device), lib.choose, ENCODES.get(job.settings.encode))
    p = plan(target, files, job.held, base)
    report(job, p)

    done = carry_out(job, p, write=write)
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
        encode: str | None = typer.Option(
            None,
            help=f"what lossless becomes: {COPY}, {', '.join(ENCODES)}; defaults to the target's own, else {COPY}",
        ),
        *,
        write: bool = typer.Option(default=False, help="actually write; without this, a dry run"),
    ) -> None:
        """Render a query of the library into a directory, transcoding lossless if asked."""
        transcoding = Transcoding(cache_dir() / "transcodes", ffmpeg)
        try:
            done = render(
                open_library(), target.expanduser(), query, write=write, encode=encode, transcoding=transcoding
            )
        except Refused as e:
            print(f"refused: {e}")
            raise typer.Exit(1) from e
        if done.failed:
            raise typer.Exit(1)
