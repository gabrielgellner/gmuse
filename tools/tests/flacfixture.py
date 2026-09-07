"""The smallest real FLAC, so a tag test has something to tag."""

from __future__ import annotations

import subprocess
from pathlib import Path


def write_flac(path: Path, seconds: float = 0.1) -> None:
    """A silent FLAC, written by ffmpeg because mutagen cannot make one."""
    subprocess.run(
        ["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", f"anullsrc=r=44100:cl=stereo:d={seconds}", str(path)],
        check=True,
    )
