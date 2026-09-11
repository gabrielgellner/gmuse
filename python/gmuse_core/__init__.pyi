"""gmuse's library, for the tools that prepare files for it — #197.

Hand-written rather than generated. `pyo3-stub-gen` would do it, and the
surface is four methods: a generator is a dependency and a build step to buy
something a reviewer can read in thirty seconds. Revisit when the surface is
large enough that keeping this in step is the harder job.
"""

from pathlib import Path
from typing import Any

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

    @property
    def data_dir(self) -> Path: ...
    @property
    def roots(self) -> list[Path]: ...
