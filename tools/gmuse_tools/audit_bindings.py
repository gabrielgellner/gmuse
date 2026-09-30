#!/usr/bin/env python3
"""Does each binding name a recording of the right length — #307.

**Every other identity check asks whether a file has an identity.** `audit
identity` reports the albums that carry no MusicBrainz id, `shelf import`
refuses a file that names none, `shelf query --unidentified` lists the ones
resting on their bytes. All three are about *absence*, and a binding that is
present and wrong passes every one of them.

That is the failure this library actually has. `identification-lessons.md`
exists because twenty years of retagging left ids that name the wrong
performance, and a wrong recording id is worse than a missing one: history,
ratings, `shelf copies` and the re-buy list all hang off the recording, so
they hang off the wrong row and nothing looks missing.

Measured on 311 Kylie Minogue copies before this existed: **20 named a
recording whose length disagreed with the file by more than 10s** — an
extended mix bound to a radio edit, a radio edit bound to an extended mix.
The inversion is the useful part: the five albums with *no* release binding
were clean, 0 of 100, and every one of the 20 sat on an album that had one.
So a release binding is not evidence the recording binding is right.

**This reports and does not resolve**, per `CLAUDE.md`. A disagreement is
evidence the binding is wrong and not by itself the right answer: MusicBrainz's
own length can be absent or wrong, a rip can carry a run-out, and the repair is
a relink to a recording somebody has identified. The TSV carries the encoding
id a `{"fact": "identified", …}` proposal would name, and choosing the
recording is left to a person.

    uv run gmuse-tools audit bindings
    uv run gmuse-tools audit bindings --filter 'artist="Kylie Minogue"'
    uv run gmuse-tools audit bindings --out out/bindings.tsv
"""

from __future__ import annotations

import collections
import re
import subprocess
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Final

import typer

from gmuse_tools import bronze
from gmuse_tools.genre.common import write_tsv
from gmuse_tools.ingest import INC, MB, asked
from gmuse_tools.library import open_library

#: Seconds either way before a length stops agreeing.
#:
#: **Chosen from the library, and there was no gap to read it off.** #307 asked
#: for the threshold to sit in the gap between the largest agreeing difference
#: and the smallest disagreeing one. Measured over 7,522 bound copies on
#: 2026-09-30, there is none: half agree within 0.11s and 90% within 1.96s,
#: then the counts decay smoothly — 112 copies at 3-4s, 57 at 4-5s, 26, 18, 18,
#: 13 through 10s, 9, 5, 4 through 20s — and only past 20s does a second
#: population appear, *rising* toward the 34 copies off by more than two
#: minutes.
#:
#: So the line is set by what sits either side of it, read row by row. **Below
#: 10s the band is essentially all one recording**, and the tell is a
#: systematic offset per album: every track of Emmylou Harris's *Gliding Bird*
#: runs 6-8s long, which is a different master with longer fades, not seven
#: wrong bindings. From 10s up, real version mismatches start to appear — a
#: single edit bound to an album cut at 15.7s, `Dirty Deeds Done Dirt Cheap` at
#: 20.9s. At 10s this reports 93 of 7,522 copies, 1.2%, which is a list a
#: person will actually read (`identification-lessons.md` §17).
#:
#: **What it cannot see, by construction:** a wrong binding within the
#: tolerance. Two versions of a song less than ten seconds apart — a radio
#: edit that only trims a fade — pass this check whichever one is bound. A
#: length is evidence against a binding, never for one.
TOLERANCE_S: Final = 10.0

#: How many rows to print before deferring to the file.
SHOWN: Final = 25

#: Why a copy was not compared. **Four sentences, not one** — the lesson of
#: #298, where "nothing to measure" covered a file that could not be decoded and
#: a row the view could not see, and 595 invisible copies went unremarked. Each
#: of these is a different fact and only some of them are about the binding.
NO_LOCAL_LENGTH: Final = "our copy has no length"
NO_MB_LENGTH: Final = "MusicBrainz gives the recording no length"
NOT_FOUND: Final = "MusicBrainz does not know the recording id"
UNANSWERED: Final = "MusicBrainz did not answer — ask again later"

#: What a disagreement turns out to be once the file itself is measured. **Four
#: kinds, because they call for four different repairs** — and the first is the
#: only one this audit exists to find.
BINDING_WRONG: Final = "the binding names the wrong recording"
LENGTH_WRONG: Final = "the library's recorded length is wrong; the binding is fine"
BROKEN: Final = "the file is broken"
BOTH_OFF: Final = "neither length matches the file"

#: Shorter than this, a decoded file is not a recording of anything.
BROKEN_BELOW_S: Final = 5.0

#: Stand-ins in the lengths map for "asked, and the answer was not a length".
#: A plain `None` would conflate "no length" with "never asked", which is the
#: distinction `bronze` exists to keep.
ABSENT: Final = "absent"
MISSING: Final = "missing"
UNASKED: Final = "unasked"


@dataclass(frozen=True)
class Copy:
    """One file, the recording it is bound to, and how long it actually runs."""

    encoding: str
    recording: str
    release: str | None
    ours_s: float | None
    locator: str | None
    title: str
    artist: str


@dataclass(frozen=True)
class Disagreement:
    """A copy whose length does not match the recording it names."""

    copy: Copy
    theirs_s: float
    #: What the file decodes to, measured independently of the library.
    decoded_s: float | None = None
    kind: str = BINDING_WRONG

    @property
    def delta(self) -> float:
        """Ours minus theirs: positive means our file runs longer."""
        assert self.copy.ours_s is not None
        return self.copy.ours_s - self.theirs_s


@dataclass
class Checked:
    """Everything a pass found, with nothing folded into anything else."""

    agree: list[Copy] = field(default_factory=list)
    disagree: list[Disagreement] = field(default_factory=list)
    unchecked: dict[str, list[Copy]] = field(default_factory=lambda: collections.defaultdict(list))

    @property
    def compared(self) -> int:
        """How many copies were actually compared — the instrument.

        A pass whose claim is an absence ("no binding disagrees") has to say it
        looked. Zero disagreements from zero comparisons reads exactly like a
        clean library, and a network outage produces precisely that.
        """
        return len(self.agree) + len(self.disagree)


def compare(
    copies: list[Copy],
    lengths: dict[str, float | str],
    tolerance: float = TOLERANCE_S,
) -> Checked:
    """Sort every copy into agree, disagree, or not compared — and say why not.

    Pure: no network, no library. `lengths` maps a recording id to its length
    in seconds, or to one of the stand-ins above, and a recording absent from
    the map was never asked about.
    """
    out = Checked()
    for c in copies:
        if c.ours_s is None or c.ours_s <= 0:
            out.unchecked[NO_LOCAL_LENGTH].append(c)
            continue
        theirs = lengths.get(c.recording, UNASKED)
        if theirs == ABSENT:
            out.unchecked[NO_MB_LENGTH].append(c)
        elif theirs == MISSING:
            out.unchecked[NOT_FOUND].append(c)
        elif not isinstance(theirs, (int, float)):
            # Unasked or unanswered: either way, no comparison happened, and
            # the one outcome this must not produce is "agree".
            out.unchecked[UNANSWERED].append(c)
        elif abs(c.ours_s - theirs) <= tolerance:
            out.agree.append(c)
        else:
            out.disagree.append(Disagreement(c, float(theirs)))
    out.disagree.sort(key=lambda d: -abs(d.delta))
    return out


def decode_length(locator: str) -> float | None:
    """How long a file actually runs, by decoding every frame of it.

    **Not the header, and not the library.** A VBR MP3 whose header lies, or
    whose length was estimated from one frame's bitrate, can read as six times
    its real duration — measured on this library, Arcade Fire's *Neon Bible*
    was recorded at 1,882s for a track that decodes to 310.3s. Decoding is the
    only answer that does not inherit either mistake.
    """
    err = subprocess.run(
        ["ffmpeg", "-v", "quiet", "-stats", "-i", locator, "-map", "0:a", "-f", "null", "-"],
        capture_output=True,
        text=True,
        check=False,
    ).stderr
    stamps = re.findall(r"time=(\d+):(\d+):([\d.]+)", err)
    if not stamps:
        return None
    h, m, sec = stamps[-1]
    return int(h) * 3600 + int(m) * 60 + float(sec)


def verify(
    checked: Checked,
    tolerance: float = TOLERANCE_S,
    measure: Callable[[str], float | None] = decode_length,
) -> None:
    """Measure every accused file for itself, and say what each one really is.

    **The library's length is one of the two things being compared, so it
    cannot also be the evidence.** Before this existed, 32 of the 93
    disagreements on this library were the library's own recorded length being
    wrong while the binding was right: *Neon Bible* decodes to 310.3s against
    MusicBrainz's 310.0s and was reported as 1,572s off. That is the rule
    `CLAUDE.md` gives for tests — the second assertion has to be computed some
    other way than the thing it vouches for — applied to the tool's own input.

    Only the disagreements are decoded, about one copy in a hundred, so this
    costs a minute on a library that takes a second to compare.
    """
    for i, d in enumerate(checked.disagree):
        if not d.copy.locator:
            continue  # nothing here to measure; the accusation stands as made
        real = measure(d.copy.locator)
        assert d.copy.ours_s is not None
        if real is None or real < BROKEN_BELOW_S:
            kind = BROKEN
        elif abs(real - d.theirs_s) <= tolerance:
            kind = LENGTH_WRONG
        elif abs(real - d.copy.ours_s) <= tolerance:
            kind = BINDING_WRONG
        else:
            kind = BOTH_OFF
        checked.disagree[i] = Disagreement(d.copy, d.theirs_s, real, kind)


def copies_from(rows: list[dict[str, object]]) -> list[Copy]:
    """The rows a binding check can say anything about.

    A row with no MusicBrainz recording rests on its bytes or on another
    authority, so there is no length to compare it against — it is not
    unchecked, it is out of scope, and counting it as unchecked would make
    every library with unidentified music look like a failed pass.
    """
    out = []
    for r in rows:
        rec, enc = r.get("musicbrainz"), r.get("encoding")
        if not rec or not enc:
            continue
        ms = r.get("length_ms")
        out.append(
            Copy(
                encoding=str(enc),
                recording=str(rec),
                release=str(r["release_musicbrainz"]) if r.get("release_musicbrainz") else None,
                ours_s=(ms / 1000) if isinstance(ms, (int, float)) and ms > 0 else None,
                locator=str(r["locator"]) if r.get("locator") else None,
                title=str(r.get("title") or ""),
                artist=str(r.get("artist") or ""),
            )
        )
    return out


def lengths_of(copies: list[Copy]) -> dict[str, float | str]:
    """Every recording's length, asking MusicBrainz as little as possible.

    **Release first, because `ingest` already asked.** A release looked up with
    `inc=recordings` carries every track's recording and its length, and
    `ingest` makes exactly that request — so with the same URL and the same
    parameters, `bronze` answers from the cache. On this library that is 516 of
    526 releases already on disk, against 7,218 recordings that would each be a
    request at one a second: about two hours, cut to the ten releases nobody had
    asked for plus the recordings no release covers.

    A recording is asked about on its own only where no release this pass saw
    carries it — a copy with no release binding, or one relinked to a recording
    its release does not list.
    """
    lengths: dict[str, float | str] = {}
    for release in sorted({c.release for c in copies if c.release}):
        try:
            body = asked(f"{MB}/release/{release}", {"inc": INC, "fmt": "json"})
        except bronze.UnansweredError:
            continue  # its recordings fall through to being asked one by one
        for medium in (body or {}).get("media", []):
            for track in medium.get("tracks", []):
                rec = track.get("recording") or {}
                if rec.get("id") and rec["id"] not in lengths:
                    ms = rec.get("length")
                    lengths[rec["id"]] = ms / 1000 if ms else ABSENT

    wanted = sorted({c.recording for c in copies} - set(lengths))
    for n, rec in enumerate(wanted, 1):
        try:
            body = asked(f"{MB}/recording/{rec}", {"fmt": "json"})
        except bronze.UnansweredError:
            continue  # left unasked, which `compare` reports as unanswered
        if body is None:
            lengths[rec] = MISSING
        else:
            ms = body.get("length")
            lengths[rec] = ms / 1000 if ms else ABSENT
        if n % 100 == 0:
            print(f"  … {n}/{len(wanted)} recordings asked about on their own", flush=True)
    return lengths


def report(checked: Checked, total: int, out: Path | None, tolerance: float = TOLERANCE_S) -> None:
    """Say what disagrees, worst first, and exactly how much was looked at."""
    print(f"{total} bound copies")
    print(f"   compared           : {checked.compared}")
    print(f"   agree within {tolerance:g}s   : {len(checked.agree)}")
    print(f"   DISAGREE           : {len(checked.disagree)}")
    for reason, rows in sorted(checked.unchecked.items(), key=lambda kv: -len(kv[1])):
        print(f"   not compared       : {len(rows):5}  {reason}")

    # **The instrument, said aloud.** A clean report and a pass that looked at
    # nothing print the same zero; this is the line that tells them apart.
    if total and checked.compared == 0:
        print("\n  Nothing was compared, so the zero above means nothing. See the reasons.")

    # **By what each one calls for**, because the four kinds are four different
    # repairs: a relink, a re-measure, a replacement, a closer look. Listed with
    # the binding kind first, which is the one this audit exists to find.
    by_kind: dict[str, list[Disagreement]] = collections.defaultdict(list)
    for d in checked.disagree:
        by_kind[d.kind].append(d)
    measured = sum(1 for d in checked.disagree if d.decoded_s is not None)
    if checked.disagree:
        print(f"\n  {measured} of {len(checked.disagree)} disagreements re-measured by decoding the file:")
        for kind in (BINDING_WRONG, LENGTH_WRONG, BROKEN, BOTH_OFF):
            if by_kind.get(kind):
                print(f"   {len(by_kind[kind]):5}  {kind}")

    for kind in (BINDING_WRONG, BOTH_OFF, BROKEN, LENGTH_WRONG):
        rows = by_kind.get(kind, [])
        if not rows:
            continue
        print(f"\n  {kind} — worst {min(SHOWN, len(rows))}:")
        for d in rows[:SHOWN]:
            c = d.copy
            where = c.locator.split("/MusicLibrary/")[-1] if c.locator else "(not on this device)"
            real = f"{d.decoded_s:7.1f}" if d.decoded_s is not None else "      ?"
            print(f"   lib {c.ours_s:7.1f}s  file {real}s  bound-to {d.theirs_s:7.1f}s  {where[:62]}")
        if len(rows) > SHOWN:
            print(f"   … and {len(rows) - SHOWN} more")

    if out is not None:
        write_tsv(
            out,
            [
                {
                    "kind": d.kind,
                    "library_s": f"{d.copy.ours_s:.1f}",
                    "decoded_s": f"{d.decoded_s:.1f}" if d.decoded_s is not None else "",
                    "musicbrainz_s": f"{d.theirs_s:.1f}",
                    "artist": d.copy.artist,
                    "title": d.copy.title,
                    "recording_musicbrainz": d.copy.recording,
                    "encoding": d.copy.encoding,
                    "locator": d.copy.locator or "",
                }
                for d in checked.disagree
            ],
        )
        print(f"\n  -> {out}")


def register(app: typer.Typer) -> None:
    """Attach the binding audit."""

    @app.command("bindings")
    def bindings_cmd(
        query: str = typer.Option(None, "--filter", help="narrow the library the way `:filter` does"),
        out: Path = typer.Option(None, help="also write every disagreement as TSV"),
        tolerance: float = typer.Option(TOLERANCE_S, help="seconds either way that still agree"),
        decode: bool = typer.Option(True, help="decode each accused file rather than trust its recorded length"),
    ) -> None:
        """Does each binding name a recording of the right length. Changes nothing."""
        rows = open_library().query(filter=query, every_copy=True)
        copies = copies_from(rows)
        checked = compare(copies, lengths_of(copies), tolerance)
        if decode:
            verify(checked, tolerance)
        report(checked, len(copies), out, tolerance)
