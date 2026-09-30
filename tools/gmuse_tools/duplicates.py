#!/usr/bin/env python3
"""Find the same music held twice, when the bytes do not say so.

The scan already reports **byte-identical** files: two paths minting one
encoding, which `held` sees as one holding and which costs only disk. That is
the easy half and it is exact.

This is the other half. `~/MusicLibrary` held Thomas Brinkmann's *Studio 1* in
two folders differing only in spacing, and only four of thirteen tracks were
byte-identical. The rest are **separate encodes of the same master**: identical
duration, identical CBR 320, identical file size, and 180,518 differing bytes
spread to the last one. Their encoded packets differ, so they mint different
encodings and different recordings, and the library showed **22 rows for a
13-track album** — nine phantoms you can play and rate, splitting the history
of music you own once.

No exact hash can find those, which is why this scores evidence instead.

## Two stages, because the second one is the expensive one

**Nominate on what the library already knows.** Same artist, durations within
a second. Measured on 6,311 files that is 2,603 candidate pairs over 2,579
files — and it finds all nine known Brinkmann pairs. Nominating on duration
alone finds them too, at 70,540 pairs; adding the *title* drops recall to zero,
because the untitled copy is exactly the one worth finding.

**Confirm with a fingerprint.** Chromaprint separates cleanly and is not close:
1.000 for the two Brinkmann encodes, 0.460 for two different tracks off the
same album. Only nominated files are fingerprinted.

## It does not decide, and the first draft of it did

This sorts suspects into lists and stops. That is not modesty for its own
sake — the first version scored each pair and printed a verdict, and its
verdict on the Brinkmann pair was **"the second buys nothing"**. The second
holds Picard's whole release metadata: barcode, catalogue number, ASIN. The
side it called worthless was the better-tagged one, and the side it preferred
was better only on ReplayGain, usable titles and play history — a judgement
call between real trade-offs, printed as a fact.

The same overreach sat one level earlier. A 0.90 cutoff meant "below this,
not the same music", which quietly discarded Emmylou Harris's `Hello Stranger`
against `Hello Stranger` at 0.883, `Te busqué (Spanish version)` against
`Te busqué` at 0.812, and the Pixies' `Holiday Song` against `The Holiday
Song` at 0.754. Every one of those is a question for a person.

So: **the fingerprint is reported, never thresholded into a conclusion.** Two
lists, and the names describe the evidence rather than prescribe an action —
audio that is identical, and audio that is close. Which file to keep, and
whether to keep both, is not answerable from what is measurable here, and the
report's job is to hand over what a person needs to answer it: how each side
is tagged, what it cost, and where it sits.
"""

from __future__ import annotations

import bisect
import json
import struct
import subprocess
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from operator import attrgetter
from pathlib import Path
from typing import Final

import gmuse_core

from gmuse_tools.library import open_library
import mutagen
import typer

#: Durations further apart than this are not the same recording. Generous,
#: because the point of the nomination stage is recall — the fingerprint is
#: what says no.
DURATION_TOLERANCE: Final = 1.0

#: The fingerprint says this is the same audio. Not a verdict about the files
#: — two identical encodes can still differ in everything a tagger wrote.
IDENTICAL: Final = 0.98

#: Below this is the noise floor: unrelated audio agrees on about half its
#: bits by construction, and 2,458 of 2,603 measured pairs sit at 0.5. Pairs
#: under it are counted in the report rather than listed, because a report
#: nobody finishes reading is the same as no report.
FLOOR: Final = 0.60

#: Sizes within this of each other are the same encode plus different tags.
SIZE_SLACK: Final = 0.02

#: Lengths closer than this are the same length. Two encodes of one master
#: agree exactly; anything coarser would call a real edit "the same".
SAME_LENGTH: Final = 0.01


@dataclass(frozen=True)
class Copy:
    """One file, as both the library and the disk describe it."""

    recording: str
    encoding: str
    locator: str
    title: str
    artist: str
    seconds: float
    bitrate: int
    channels: int
    sample_rate: int
    size: int
    tags: int
    musicbrainz: bool
    replaygain: bool

    @property
    def suffix(self) -> str:
        """The container, which is half of "is this a better copy or a stray"."""
        return Path(self.locator).suffix.lower().lstrip(".")


@dataclass
class Suspect:
    """Two copies that look like one recording, and why."""

    left: Copy
    right: Copy
    score: float
    reasons: list[str]

    @property
    def band(self) -> str:
        """Which list this belongs in — a statement about the fingerprint only.

        Deliberately **not** a verdict. Whether two files with identical audio
        should become one file is a question about tags, history and taste,
        and nothing measured here can answer it.
        """
        return "identical audio" if self.score >= IDENTICAL else "close"


def read(row: dict[str, str]) -> Copy | None:
    """Open the locator and ask the file what the work list does not carry.

    A work-list row has no duration and no bitrate, and this is the sanctioned
    way to get them: the tool opens the **locator** and names its findings by
    the **id**.
    """
    locator = row.get("locator")
    encoding = row.get("encoding")
    if not locator or not encoding:
        return None
    try:
        probed = mutagen.File(locator)
    except Exception:  # noqa: BLE001 — an unreadable file is skipped, not fatal
        return None
    if probed is None or getattr(probed, "info", None) is None:
        return None
    try:
        size = Path(locator).stat().st_size
    except OSError:
        return None
    # Stringified keys, because every container spells these differently and
    # the question — "is there anything here worth not losing" — is the same.
    keys = " ".join(str(k) for k in (probed.tags or {})).lower()
    return Copy(
        recording=row["recording"],
        encoding=encoding,
        locator=locator,
        title=row.get("title", ""),
        artist=row.get("artist", ""),
        # **The library's length where it has one.** `info.length` is computed
        # from the Xing header where a file has one and from `size / first
        # frame's bitrate` where it does not — wrong by up to 26 minutes on
        # this library. Nomination buckets by duration, so a wrong one hides a
        # real duplicate in a bucket of its own. The library counts frames at
        # import, which is exact.
        seconds=(
            round(row["length_ms"] / 1000.0, 2)
            if isinstance(row.get("length_ms"), (int, float)) and row["length_ms"] > 0
            else round(probed.info.length, 2)
        ),
        bitrate=getattr(probed.info, "bitrate", 0),
        channels=getattr(probed.info, "channels", 0),
        sample_rate=getattr(probed.info, "sample_rate", 0),
        size=size,
        tags=len(probed.tags or {}),
        musicbrainz="musicbrainz" in keys,
        replaygain="replaygain" in keys,
    )


def fold(text: str | None) -> str:
    """A name with everything a second import might have changed removed."""
    return "".join(c.lower() for c in (text or "") if c.isalnum())


def nominate(copies: list[Copy], *, any_artist: bool = False) -> list[tuple[Copy, Copy]]:
    """Pairs worth fingerprinting: a near-identical length, and usually an artist.

    **Recall is the whole job here** — a pair this misses is never looked at
    again. Nominating on the title as well would be tighter and would have
    found none of the nine real ones, because the copy worth finding is the
    one whose tags were lost.

    **`any_artist` drops the last bucket, and asks a different question.** The
    artist bucket is a recall limit, not a correctness one: it can only find
    music held twice under the *same* credit. What it cannot see is a track on
    a chart compilation that is also on its own artist's album, the same
    recording under two joined-credit spellings, or anything a copy filed as
    `Various Artists` duplicates.

    That is the question "is this the same music" actually asks, and it is
    affordable now: 149,546 pairs against 2,603, over the same fingerprints,
    and the comparison is C-level since `packed` exists. What it costs is
    fingerprinting nearly every file rather than the 2,579 the artist bucket
    nominates.
    """
    buckets: dict[str, list[Copy]] = defaultdict(list)
    for copy in copies:
        if any_artist:
            buckets["everything"].append(copy)
        elif fold(copy.artist):
            buckets[fold(copy.artist)].append(copy)

    pairs = []
    for group in buckets.values():
        group.sort(key=attrgetter("seconds"))
        lengths = [c.seconds for c in group]
        for i, left in enumerate(group):
            # Sorted by length, so everything in range is one contiguous run.
            stop = bisect.bisect_right(lengths, left.seconds + DURATION_TOLERANCE)
            pairs.extend((left, right) for right in group[i + 1 : stop] if left.encoding != right.encoding)
    return pairs


def _words(stdout: str) -> list[int]:
    """The fingerprint out of `fpcalc`'s JSON, or nothing it could not parse."""
    try:
        return [int(v) for v in json.loads(stdout)["fingerprint"]]
    except (json.JSONDecodeError, KeyError, TypeError, ValueError):
        return []


def _piped(locator: str) -> list[int]:
    """Fingerprint the audio stream alone, decoded by ffmpeg.

    **`fpcalc` refuses a file with cover art in it.** Its demuxer meets the
    embedded MJPEG stream and answers `Invalid data found when processing
    input` — for the whole file, not the picture. Measured on this library:
    **1,550 files, 22% of it**, silently unfingerprintable and therefore
    invisible to this pass.

    `ffmpeg -map 0:a` hands over the audio and nothing else, and the same
    binary that refused the file reads the stream happily. The duration it
    reports is 0.00 because a pipe has no length, which costs nothing here —
    the length came from the tags, and it is the nomination that used it.
    """
    try:
        decode = subprocess.Popen(
            ["ffmpeg", "-v", "quiet", "-i", locator, "-map", "0:a", "-f", "wav", "-"],
            stdout=subprocess.PIPE,
        )
    except OSError:
        return []
    try:
        done = subprocess.run(
            ["fpcalc", "-raw", "-json", "-"],
            stdin=decode.stdout,
            capture_output=True,
            text=True,
            timeout=180,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        decode.kill()
        return []
    finally:
        if decode.stdout:
            decode.stdout.close()
        decode.wait()
    return _words(done.stdout) if done.returncode == 0 else []


def fingerprint(locator: str) -> list[int]:
    """Chromaprint's raw fingerprint, or an empty list if it will not decode.

    Two ways, because the first one refuses a fifth of this library: `fpcalc`
    on the file, then — only when that comes back with nothing — the audio
    stream alone through ffmpeg. See [`_piped`] for what it is working around.
    """
    try:
        done = subprocess.run(
            ["fpcalc", "-raw", "-json", locator],
            capture_output=True,
            text=True,
            timeout=120,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return _piped(locator)
    if done.returncode != 0:
        return _piped(locator)
    return _words(done.stdout) or _piped(locator)


def packed(raw: list[int]) -> bytes:
    """A fingerprint as bytes, so a comparison is one operation rather than N.

    **The whole cost of this pass is pairwise.** Comparing two fingerprints a
    word at a time is a Python loop over a couple of thousand elements, which
    is affordable for the 2,603 pairs the artist-bucketed nomination makes and
    is not for the 149,546 that ignoring the artist makes. Packed, the compare
    is a `bytes` slice, one big-integer XOR and one `bit_count` — all of it in
    C — and the sweep runs in seconds.
    """
    return struct.pack(f"<{len(raw)}I", *(w & 0xFFFF_FFFF for w in raw))


def agreement(left: list[int], right: list[int]) -> float:
    """Fraction of fingerprint bits that agree, over the shorter of the two.

    Aligned at the start rather than searched for an offset, which is right
    here and would not be for a DJ mix containing a track: nomination already
    required the two to be the same length.
    """
    return agrees(packed(left), packed(right))


def agrees(left: bytes, right: bytes) -> float:
    """The same measure, on fingerprints already packed by [`packed`]."""
    span = min(len(left), len(right))
    if span == 0:
        return 0.0
    a = int.from_bytes(left[:span], "little")
    b = int.from_bytes(right[:span], "little")
    return 1.0 - (a ^ b).bit_count() / (8.0 * span)


def why(left: Copy, right: Copy, score: float) -> list[str]:
    """The evidence, stated and not interpreted.

    Every line here is something measured. The first draft ended one of them
    with "the second buys nothing", which was an instruction wearing the
    clothes of an observation, and it was wrong about the only pair anybody
    had checked by hand.
    """
    said = [f"fingerprint {score:.3f}"]
    gap = abs(left.seconds - right.seconds)
    said.append(f"same length {left.seconds:.2f}s" if gap < SAME_LENGTH else f"length differs by {gap:.2f}s")

    if left.suffix == right.suffix and left.bitrate == right.bitrate:
        said.append(f"both {left.suffix} at {left.bitrate // 1000}k")
    else:
        said.append(f"{left.suffix} {left.bitrate // 1000}k vs {right.suffix} {right.bitrate // 1000}k")

    if left.size and abs(left.size - right.size) <= SIZE_SLACK * max(left.size, right.size):
        said.append("same size to within a tag")

    here, there = Path(left.locator).parent, Path(right.locator).parent
    if here == there:
        said.append("in the same folder")
    elif fold(here.name) == fold(there.name):
        said.append(f'folder names differ only in spelling: "{here.name}" vs "{there.name}"')
    elif here.parent == there.parent:
        said.append(f'sibling folders: "{here.name}" vs "{there.name}"')

    if fold(left.title) != fold(right.title):
        missing = not fold(left.title) or not fold(right.title)
        said.append(
            "one side has no title, which is why nothing converged them"
            if missing
            else f'titled differently: "{left.title}" vs "{right.title}"'
        )
    elif fold(left.title):
        said.append(f'both titled "{left.title}"')

    said.extend(keeping(left, right))
    return said


def keeping(left: Copy, right: Copy) -> list[str]:
    """What each side would cost to lose.

    **The half a person actually decides on**, and the half the first draft
    left out. Choosing between the two Brinkmann copies took hand-inspecting
    both for Picard tags and ReplayGain, after the tool had already announced
    which one was worthless.
    """
    said = []
    if left.tags != right.tags:
        said.append(f"tag count {left.tags} vs {right.tags}")
    for label, get in (("MusicBrainz ids", "musicbrainz"), ("ReplayGain", "replaygain")):
        mine, theirs = getattr(left, get), getattr(right, get)
        if mine != theirs:
            said.append(f"{label} on the {'first' if mine else 'second'} only")
    return said


def _rank(suspect: Suspect) -> tuple[float, str]:
    """Strongest evidence first, then stable by path."""
    return (-suspect.score, suspect.left.locator)


@dataclass
class Findings:
    """Everything the pass saw, including what it is not listing.

    `below` and `unchecked` exist because both were silently zero before.
    A pair `fpcalc` could not read scored 0.0 and was indistinguishable from
    a pair that is genuinely unrelated — 38 of them, reported as nothing.
    """

    suspects: list[Suspect]
    below: int
    unchecked: list[tuple[Copy, Copy]]
    pairs: int
    files: int


def examine(copies: list[Copy], *, workers: int, floor: float = FLOOR, any_artist: bool = False) -> Findings:
    """Nominate, fingerprint what was nominated, and rank it. Decide nothing."""
    pairs = nominate(copies, any_artist=any_artist)
    wanted = sorted({c.locator for pair in pairs for c in pair})
    with ThreadPoolExecutor(max_workers=workers) as pool:
        prints = {at: packed(raw) for at, raw in zip(wanted, pool.map(fingerprint, wanted), strict=True)}

    found: list[Suspect] = []
    unchecked: list[tuple[Copy, Copy]] = []
    below = 0
    for left, right in pairs:
        if not prints[left.locator] or not prints[right.locator]:
            unchecked.append((left, right))
            continue
        score = agrees(prints[left.locator], prints[right.locator])
        if score >= floor:
            found.append(Suspect(left, right, score, why(left, right, score)))
        else:
            below += 1
    found.sort(key=_rank)
    return Findings(suspects=found, below=below, unchecked=unchecked, pairs=len(pairs), files=len(wanted))


def render(found: Findings, *, root: Path | None) -> str:
    """The report: two lists, an accounting, and no advice."""

    def shown(locator: str) -> str:
        path = Path(locator)
        if root:
            try:
                return str(path.relative_to(root))
            except ValueError:
                pass
        return str(path)

    def block(suspect: Suspect) -> list[str]:
        return [
            f"  {shown(suspect.left.locator)}",
            f"  {shown(suspect.right.locator)}",
            *(f"    - {reason}" for reason in suspect.reasons),
            "",
        ]

    lines = [f"{found.pairs} candidate pair(s) over {found.files} file(s)\n"]
    for band in ("identical audio", "close"):
        here = [s for s in found.suspects if s.band == band]
        lines.append(f"── {band} — {len(here)} ──\n")
        for suspect in here:
            lines.extend(block(suspect))
        if not here:
            lines.append("  none\n")

    if found.unchecked:
        # Named, because "could not read" and "not a duplicate" were the same
        # answer before and one of them is a bug in the making.
        lines.append(f"── could not fingerprint — {len(found.unchecked)} ──\n")
        for left, right in found.unchecked:
            lines.extend([f"  {shown(left.locator)}", f"  {shown(right.locator)}", ""])

    # No silent caps: say what was dropped, or the report reads as coverage.
    lines.append(f"{found.below} pair(s) scored below {FLOOR:.2f} and are not listed.")
    lines.append("Nothing here is a recommendation. Which copy to keep, and whether to keep both, is yours.")
    return "\n".join(lines)


def register(app: typer.Typer) -> None:
    """Attach the duplicate audit."""

    @app.command("duplicates")
    # PLR0913/PLR0917: a command's parameters are its flags, and the rule's
    # concern — an argument list nobody can hold in their head at a call site —
    # does not arise for something only ever called from a shell.
    # FBT001/FBT003: a Typer flag is declared as a boolean parameter with a
    # boolean default, which is the framework's only spelling for one.
    def duplicates_cmd(
        root: Path = typer.Option(None, "--root", help="trim this prefix off reported paths"),
        workers: int = typer.Option(8, "--workers", help="parallel fpcalc processes"),
        floor: float = typer.Option(FLOOR, "--floor", help="lowest fingerprint score to list"),
        any_artist: bool = typer.Option(False, "--any-artist", help="the same music, whoever the tags say made it"),  # noqa: FBT001, FBT003
        as_json: bool = typer.Option(False, "--json", help="one suspect per line, for a script"),  # noqa: FBT001, FBT003
    ) -> None:
        """Same music held twice, scored, with the evidence. Changes nothing.

        **Every copy** — #183, #198. This is a pass about *files*: it opens each
        one and fingerprints it, so one row per recording would hide exactly
        what it is looking for. It used to take whatever work list somebody
        handed it, which meant a list built without `--every-copy` could not
        find a second copy however hard it looked.
        """
        rows = open_library().query(every_copy=True)
        copies = [c for c in (read(row) for row in rows) if c is not None]
        found = examine(copies, workers=workers, floor=floor, any_artist=any_artist)
        if as_json:
            for suspect in found.suspects:
                typer.echo(
                    json.dumps(
                        {
                            "band": suspect.band,
                            "score": round(suspect.score, 4),
                            "left": {"encoding": suspect.left.encoding, "locator": suspect.left.locator},
                            "right": {"encoding": suspect.right.encoding, "locator": suspect.right.locator},
                            "reasons": suspect.reasons,
                        }
                    )
                )
        else:
            typer.echo(render(found, root=root.expanduser() if root else None))
