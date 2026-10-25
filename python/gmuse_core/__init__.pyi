"""gmuse's library, for the tools that prepare files for it — #197.

Hand-written rather than generated. `pyo3-stub-gen` would do it, and the
surface is four methods: a generator is a dependency and a build step to buy
something a reviewer can read in thirty seconds. Revisit when the surface is
large enough that keeping this in step is the harder job.
"""

from pathlib import Path
from typing import Any

__version__: str
"""The crate version this extension was compiled from — #298.

Not the wheel's `dist-info` version, which describes what was installed. This
is a property of the bytes that are loaded, which is the thing that can be out
of step with the source tree beside them.
"""

class Library:
    """A read handle. **Opens no music file.**

    It loads the folded projection — or stats the logs, finds it current, and
    skips the fold. A file that arrived reaches a row when `gmuse shelf import`
    takes it in; a file that moved when `gmuse shelf scan` says so. Neither is
    this object's business.
    """

    @staticmethod
    def open(data_dir: Path | str | None = None) -> Library: ...

    def query(
        self,
        filter: str | None = None,
        *,
        unidentified: bool = False,
        every_copy: bool = False,
    ) -> list[dict[str, Any]]:
        """The rows a filter leaves, in the shape `shelf query` prints.

        `filter` is the language the `:` line takes, through the same parser —
        `artist="*campfire*"` reaches every name an artist answers to, hidden
        ones included. A spec this refuses is one the `:` line refuses, and the
        `ValueError` says what it choked on.

        `every_copy` gives one row per copy rather than per recording, which is
        what a pass about the *bytes* wants: measurements are keyed by encoding,
        so a library holding one recording as a FLAC and an MP3 can otherwise
        only ever reach whichever one `open` picked.
        """
        ...

    def propose(self, items: list[str], path: Path | str) -> int:
        """Write proposals, having checked every one first.

        Each is deserialised into the same `Proposal` the apply pass reads, so a
        malformed one raises **here**, naming the row and what was wrong with
        it, rather than being found by `gmuse shelf apply` after a long pass.
        """
        ...

    def choose(self, groups: list[list[str]]) -> list[dict[str, Any]]:
        """The copy each group's rendering is made from — #333.

        A group is the encoding ids that compete for one place: the copies of
        one recording that would land in one folder. The best this device
        holds, in the order a view ranks copies by. One dict per group, in
        order: `encoding`, its `locator`, whether it is `lossless`, its
        `length_ms`, `tied` (the copies only an id separated from it) and
        `tied_at` — or `{}` where none is held here.
        """
        ...

    def statement(self, encodings: list[str]) -> list[dict[str, Any]]:
        """What the library says about each copy's appearance — #335.

        One dict per encoding id, in order: `title`, `artist`, `album`,
        `album_artist`, `label`, `track`, `disc`, `discs`, `year`, `genres`,
        `compilation`, `musicbrainz_recording`, `musicbrainz_release`, the
        source's `track_gain_db`, `track_peak`, `album_gain_db`, `album_peak`,
        and the recording's `stars` (#338) — which is not a tag.
        A field the library has no value for is absent, not empty; `{}` for a
        copy with no row. Read from the projection, never from the file.
        """
        ...

    def layout(self, encodings: list[str]) -> list[dict[str, Any]]:
        """Where each copy belongs in a rendered tree, relative — #332.

        One dict per encoding id, in the order given: `path` where it belongs,
        or `refused` saying why there is nowhere, or `elsewhere` for a copy this
        device does not hold. A path two of the copies want is refused for
        both, because this is the function a re-render uses — and carries
        `contested` and the `path`, so a caller that can resolve the contest
        knows where it is.
        """
        ...

    def encoding_at(self, path: Path | str) -> str | None:
        """Which encoding the bytes at this path *are*.

        A tool that opens a file and then proposes a fact about it has two ways
        to name what it measured: the encoding a work list said lives there, or
        the encoding the bytes it actually read are. Those agree until the file
        changed, and then the first is a false fact in an append-only log.

        `None` for a file that cannot be read — a row to skip, not a run to
        end. Guarded by a `stat`, so an unchanged file costs no re-read.
        """
        ...

    def release_from_musicbrainz(self, mbid: str) -> str:
        """The library's id for a MusicBrainz release.

        Ids here are convergent — a UUIDv5 over the mbid — so two machines that
        never spoke mint the same one. It is **one-way**, which is why the
        library cannot hand an mbid back and why a pass that wants to look a
        release up still reads one from a tag.

        What this is for is checking that tag before believing it: derive the
        id it claims, compare it to the `release` on the row, and a
        disagreement is a row to skip rather than a fact written against
        somebody else's pressing.
        """
        ...

    def check(self, items: list[str]) -> int:
        """Check proposals without writing them, for a pass that writes as it goes.

        `propose` is right for a pass that finishes. One that may be
        interrupted — `ingest tracklist` takes an hour — writes each line as it
        settles it, so holding them back to validate together would lose what
        an interrupted run had learned. Same check, no file.
        """
        ...

    @property
    def data_dir(self) -> Path: ...
    @property
    def roots(self) -> list[Path]: ...
