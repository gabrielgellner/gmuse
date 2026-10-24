"""Where a rendering lands: a directory here, or one on another machine over ssh — #336.

A rendering is the same plan whichever it is (`render.py`); what differs is how
a file gets there. So everything the renderer does to a target goes through the
six things below, and nothing else touches the target.

```
TARGET      a directory on this machine
HOST:/PATH  a directory on HOST, reached over ssh — `asnas:/volume1/Media/PlexMusic`
```

## Over ssh, the way `archive` goes

#328 measured the SMB mount against ssh: a dry run of ~12,000 files took 118 s
through the share and 0.5 s over ssh, the copy moved 3.5-6.5 MB/s against 34,
and the share decomposed accented names. So a remote target is rsync and a
shell over ssh, with rsync on both ends, and never the mount.

## Staged here, sent in batches

A file is copied, encoded and tagged **on this machine**, where the master is,
and only the finished file travels: a FLAC is ~2.5x its encode (§3). Files go
in batches, and the manifest is saved after each, so an interrupted run has
lost at most one batch of rows and no file — rsync lands each by rename.

## One listing, not a question per file

What a target holds is listed once, before planning, and once after writing
to check that every path the manifest names is really there: rsync has
exited 0 on this NAS having skipped files (`nas-music-sources`), and a count
is the check that would notice.

## The remote side is BusyBox

The NAS's `find` and `xargs` are BusyBox (measured), so the commands here use
only what it has: `find -print0`, `xargs -0`, `rm -f --`, `rmdir --`. Names
cross as the UTF-8 the layout wrote, NFC, untouched.
"""

from __future__ import annotations

import shlex
import shutil
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import TYPE_CHECKING, Final

if TYPE_CHECKING:
    from collections.abc import Iterable

#: The file in a target that makes it a rendering.
MANIFEST: Final = ".gmuse-rendering.json"

#: Where a local target stages its files: inside it, so landing one is a rename.
STAGING: Final = ".gmuse-staging"

#: What a remote command exits with when the manifest is not there, as opposed
#: to the 255 ssh exits with when the host is not.
NO_MANIFEST: Final = 3


class Unreachable(RuntimeError):  # noqa: N818 — a state of the target, read as one
    """A target that could not be read or written at all."""


def parents(paths: Iterable[str]) -> list[str]:
    """Every directory above these paths, deepest first — what a delete may have emptied."""
    found = {str(p) for path in paths for p in PurePosixPath(path).parents if str(p) != "."}
    return sorted(found, key=lambda d: (-d.count("/"), d))


@dataclass
class LocalDir:
    """A directory on this machine."""

    root: Path

    @property
    def name(self) -> str:
        """How a report names it."""
        return str(self.root)

    def read_manifest(self) -> str | None:
        """The manifest's text, `None` where there is none."""
        at = self.root / MANIFEST
        return at.read_text(encoding="utf-8") if at.is_file() else None

    def write_manifest(self, text: str) -> None:
        """Write the manifest by rename, so a reader never sees half of one."""
        self.root.mkdir(parents=True, exist_ok=True)
        tmp = self.root / f"{MANIFEST}.tmp"
        tmp.write_text(text, encoding="utf-8")
        tmp.replace(self.root / MANIFEST)

    def files(self) -> set[str]:
        """Every file the target holds, relative, but the manifest and the staging area."""
        if not self.root.is_dir():
            return set()
        out = set()
        for p in self.root.rglob("*"):
            rel = p.relative_to(self.root)
            if p.is_file() and rel.parts[0] != STAGING and rel.name not in {MANIFEST, f"{MANIFEST}.tmp"}:
                out.add(str(PurePosixPath(*rel.parts)))
        return out

    def remove(self, paths: list[str]) -> list[tuple[str, str]]:
        """Delete these paths and the directories that leaves empty; `(path, why)` for each that failed."""
        failed = []
        for path in paths:
            try:
                (self.root / path).unlink(missing_ok=True)
            except OSError as e:
                failed.append((path, str(e)))
        for d in parents(paths):
            try:
                (self.root / d).rmdir()
            except OSError:
                continue
        return failed

    def staging(self) -> Path:
        """Where to build files before they land: in the target, so landing is a rename."""
        at = self.root / STAGING
        at.mkdir(parents=True, exist_ok=True)
        return at

    def put(self, staged: Path, paths: list[str]) -> list[tuple[str, str]]:
        """Land staged files at their paths, each by rename."""
        failed = []
        for path in paths:
            at = self.root / path
            try:
                at.parent.mkdir(parents=True, exist_ok=True)
                (staged / path).replace(at)
            except OSError as e:
                failed.append((path, str(e)))
        return failed

    def done_staging(self, staged: Path) -> None:
        """Take the staging area away."""
        shutil.rmtree(staged, ignore_errors=True)


@dataclass
class Remote:
    """A directory on another machine, over ssh and rsync.

    `host` is `None` only in tests, where the same commands run through a
    local shell — the strings a NAS would be sent, exercised without one.
    """

    host: str | None
    root: str
    ssh: tuple[str, ...] = ("ssh", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10")
    rsync: tuple[str, ...] = ("rsync", "-a", "--no-perms", "--protect-args")

    @property
    def name(self) -> str:
        """How a report names it."""
        return f"{self.host}:{self.root}" if self.host else self.root

    def run(self, command: str, stdin: bytes = b"") -> subprocess.CompletedProcess[bytes]:
        """Run a shell command on the target's machine."""
        argv = [*self.ssh, self.host, command] if self.host else ["sh", "-c", command]
        return subprocess.run(argv, input=stdin, capture_output=True, check=False)

    def checked(self, command: str, stdin: bytes = b"") -> bytes:
        """Run it, and say why if the machine could not."""
        done = self.run(command, stdin)
        if done.returncode != 0:
            why = done.stderr.decode(errors="replace").strip() or f"exit {done.returncode}"
            msg = f"{self.name}: {why}"
            raise Unreachable(msg)
        return done.stdout

    def read_manifest(self) -> str | None:
        """The manifest's text, `None` where there is none — and an error where the host is not there."""
        at = shlex.quote(f"{self.root}/{MANIFEST}")
        done = self.run(f"if [ -f {at} ]; then cat -- {at}; else exit {NO_MANIFEST}; fi")
        if done.returncode == NO_MANIFEST:
            return None
        if done.returncode != 0:
            msg = f"{self.name}: {done.stderr.decode(errors='replace').strip() or f'exit {done.returncode}'}"
            raise Unreachable(msg)
        return done.stdout.decode("utf-8")

    def write_manifest(self, text: str) -> None:
        """Write the manifest by rename on the far side, so a reader never sees half of one."""
        root, at = shlex.quote(self.root), shlex.quote(f"{self.root}/{MANIFEST}")
        self.checked(f"mkdir -p -- {root} && cat > {at}.tmp && mv -f -- {at}.tmp {at}", text.encode("utf-8"))

    def files(self) -> set[str]:
        """Every file the target holds, relative, but the manifest — one listing, one round trip."""
        root = shlex.quote(self.root)
        out = self.checked(f"if [ -d {root} ]; then cd -- {root} && find . -type f -print0; fi")
        names = (n.decode("utf-8").removeprefix("./") for n in out.split(b"\0") if n)
        return {n for n in names if n not in {MANIFEST, f"{MANIFEST}.tmp"}}

    def remove(self, paths: list[str]) -> list[tuple[str, str]]:
        """Delete these paths and the directories that leaves empty, in two commands."""
        if not paths:
            return []
        root = shlex.quote(self.root)
        listing = b"\0".join(p.encode("utf-8") for p in paths) + b"\0"
        done = self.run(f"cd -- {root} && xargs -0 rm -f --", listing)
        if done.returncode != 0:
            why = done.stderr.decode(errors="replace").strip() or f"exit {done.returncode}"
            return [(p, why) for p in paths]
        dirs = parents(paths)
        if dirs:
            # `rmdir` refuses a directory still holding something, which is
            # the point: only what this delete emptied goes.
            self.run(
                f"cd -- {root} && xargs -0 rmdir -- 2>/dev/null; true", b"\0".join(d.encode() for d in dirs) + b"\0"
            )
        return []

    def staging(self) -> Path:
        """A local directory to build files in before they are sent."""
        return Path(tempfile.mkdtemp(prefix="gmuse-render-"))

    def put(self, staged: Path, paths: list[str]) -> list[tuple[str, str]]:
        """Send staged files to their paths in one rsync, then let them go here.

        `--files-from` keeps each file's relative path and makes the
        directories above it; rsync lands each file by rename.
        """
        if not paths:
            return []
        dest = f"{self.host}:{self.root}/" if self.host else f"{self.root}/"
        if not self.host:
            Path(self.root).mkdir(parents=True, exist_ok=True)
        listing = b"\0".join(p.encode("utf-8") for p in paths) + b"\0"
        done = subprocess.run(
            [*self.rsync, "--from0", "--files-from=-", f"{staged}/", dest],
            input=listing,
            capture_output=True,
            check=False,
        )
        for path in paths:
            (staged / path).unlink(missing_ok=True)
        if done.returncode != 0:
            why = done.stderr.decode(errors="replace").strip().splitlines()[-1:] or [f"exit {done.returncode}"]
            return [(p, why[0]) for p in paths]
        return []

    def done_staging(self, staged: Path) -> None:
        """Take the local staging area away."""
        shutil.rmtree(staged, ignore_errors=True)


Destination = LocalDir | Remote


def parse(text: str) -> Destination:
    """`HOST:/PATH` is a remote target and anything else a directory here — parsed once, at the edge.

    **The path must be absolute** for a remote, because a relative one would be
    relative to whatever the login shell's home is, which is a guess.
    """
    host, sep, path = text.partition(":")
    if sep and host and "/" not in host and path.startswith("/"):
        return Remote(host, path.rstrip("/") or "/")
    return LocalDir(Path(text).expanduser())
