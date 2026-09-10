#!/usr/bin/env python3
"""What a file's audio actually is, rather than what its header claims.

A file transcoded from 128 kbps and re-encoded at 320 still *says* 320. Nothing
in the tags gives it away, and the LAME header records the last encode, not the
provenance. What does give it away is the **lowpass cutoff**: every lossy
encoder discards everything above a frequency chosen by bitrate, and
re-encoding cannot put back what was thrown away.

    LAME "320 kbps"      16 kHz  -23.2 dB      17 kHz  -81.9 dB

A 58 dB fall in 1 kHz is not music. It is an encoder's filter, and at 16 kHz it
is the signature of a 128 kbps source wearing a 320 kbps header.

## Why the steepness matters more than the frequency

A quiet folk recording, a 1940s mono transfer, a spoken-word track — none of
them have energy at 19 kHz either, and none of them are transcodes. Judging on
the cutoff frequency alone accuses all of them.

So this measures the **cliff**, not the edge: how far the spectrum falls in the
kilohertz above where it stops. An encoder's filter drops 30-60 dB almost
vertically; music rolls off gently. Where there is no cliff, the verdict is "no
sharp cutoff" — a fact about the recording, not a charge against it.

An earlier attempt using ffmpeg's band filters is worth recording as a failure:
it ranked 96 kbps *above* 128, because filter leakage and musical content
swamped the signal it was looking for. The FFT is not gold-plating; the cheap
version does not work.

## What the cutoff cannot see

A competent re-encode from a good source leaves no wall, so a 320 that was
decoded and written again measures exactly like one that was not. **Provenance
is the only record of it**, and on this library it is on the path: 144 files
carry a `_pn` from a DJ mastering tool. 130 of them measure clean and are
reported as `re-encoded` — a fact about where the bytes came from rather than a
charge against them. Read before a rename takes it away (#143).

    uv run python -m quality survey ~/MusicLibrary       # -> out/quality-review.tsv
    uv run python -m quality survey ~/MusicLibrary --tier 320
"""

from __future__ import annotations

import collections
import csv
import json
import shutil
import subprocess
from concurrent.futures import ProcessPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Final

import numpy as np
import typer
from mutagen import File as MutagenFile
from mutagen import MutagenError

from gmuse_tools.genre.common import AUDIO, out_path, walk, write_tsv
from gmuse_tools.paths import PROCESSED

#: Analysis rate. Everything is resampled here so one FFT size means one
#: frequency resolution across formats.
RATE: Final = 44100
#: FFT window. 8192 at 44.1 kHz is 5.4 Hz per bin and ~186 ms per frame — fine
#: enough to place a cutoff to a fraction of a kilohertz, long enough that a
#: cymbal crash does not dominate.
WINDOW: Final = 8192
#: Where in the track to listen. Intros are quiet and fade-ins are misleading.
START: Final = 45
DURATION: Final = 20

#: Below this many frames there is not enough audio to average.
MIN_FRAMES: Final = 8
#: A bin this far under the reference band counts as empty.
FLOOR_DB: Final = 45
#: A fall of this many dB in the kilohertz above the cutoff is an encoder's
#: filter rather than music running out.
CLIFF_DB: Final = 25

#: The band the cutoff is judged against: loud enough to be the music, low
#: enough that no encoder has touched it.
REFERENCE_HZ: Final = (1000, 5000)
#: Nothing below this is a cutoff; it is a bass-heavy track or a bad decode.
FLOOR_HZ: Final = 2000
#: Below this a lossless file is carrying lossy audio, whatever the container.
LOSSLESS_FLOOR_KHZ: Final = 19.0

#: Where to hunt for an encoder's wall. Below 8 kHz nothing lowpasses; above
#: 22 kHz there is nothing left at 44.1 kHz.
SEARCH_FROM_HZ: Final = 8000
SEARCH_TO_HZ: Final = 22000
#: The width the fall is measured over. Half a kilohertz is narrow enough that
#: a gentle slope cannot accumulate into one, wide enough to span a real wall.
STEP_HZ: Final = 500
#: Fewer bins than this in the search band and there is nothing to walk.
MIN_BINS: Final = 4
#: `Collection/Artist/Album` — the three directories a row is named by.
NAMED_DEPTH: Final = 3
#: What a clean encode reaches. Albums are ranked by how far under this they
#: fall, so a genuinely low-bitrate rip outranks a merely mislabelled one.
CLEAN_KHZ: Final = 20.5

#: What LAME's own lowpass gives at each bitrate, in kHz. A file whose audio
#: stops well below its declared row was not encoded at that rate from a clean
#: source — whatever its header says.
EXPECTED: Final[dict[int, float]] = {
    320: 20.5,
    256: 19.5,
    224: 19.0,
    192: 18.6,
    160: 17.5,
    128: 16.0,
    112: 15.4,
    96: 15.1,
}
#: How far under the expectation is worth reporting. Encoders vary, presets
#: vary, and 1 kHz of slack keeps the list to files that are actually wrong.
SLACK_KHZ: Final = 1.5


@dataclass
class Measured:
    """One file's verdict."""

    path: str
    codec: str
    bitrate: int
    cutoff_khz: float | None
    cliff_db: float | None
    verdict: str
    note: str


def decode(path: str) -> np.ndarray | None:
    """A mono float32 window from the middle of the file, or None."""
    try:
        raw = subprocess.run(
            [
                "ffmpeg",
                "-v",
                "quiet",
                "-ss",
                str(START),
                "-t",
                str(DURATION),
                "-i",
                path,
                "-ac",
                "1",
                "-ar",
                str(RATE),
                "-f",
                "f32le",
                "-",
            ],
            capture_output=True,
            timeout=120,
            check=False,
        ).stdout
    except (subprocess.SubprocessError, OSError):
        return None
    audio = np.frombuffer(raw, dtype=np.float32)
    return audio if audio.size >= WINDOW * MIN_FRAMES else None


def spectrum(audio: np.ndarray) -> tuple[np.ndarray, np.ndarray]:
    """Averaged power spectrum in dB, and its frequency axis."""
    frames = audio.size // WINDOW
    window = np.hanning(WINDOW)
    acc = np.zeros(WINDOW // 2 + 1)
    for i in range(frames):
        acc += np.abs(np.fft.rfft(audio[i * WINDOW : (i + 1) * WINDOW] * window)) ** 2
    psd = 10 * np.log10(acc / frames + 1e-20)
    return np.fft.rfftfreq(WINDOW, 1 / RATE), psd


def measure(path: str) -> tuple[float, float] | None:
    """Return `(cutoff_khz, cliff_db)`, or None when the file cannot be read.

    **Looks for the wall, not the edge.** An earlier version took the highest
    frequency still within 45 dB of the 1-5 kHz peak, which measures *level* and
    is therefore a measure of the music: a track with a strong midrange and a
    soft top end reported a 3 kHz "cutoff" and 57% of this library came back as
    unjudgeable. Nothing was wrong with those files.

    An encoder's lowpass is a **shape** — a near-vertical fall of 30-60 dB
    inside a kilohertz, at the same place in every frame. So this walks the
    spectrum above 8 kHz in half-kilohertz steps and finds the steepest single
    drop. That is content-independent: quiet music has a gentle slope wherever
    it ends, and a brick wall is a brick wall whatever is playing.
    """
    audio = decode(path)
    if audio is None:
        return None
    freqs, psd = spectrum(audio)

    # Smoothed, so one loud bin cannot pass for an edge.
    kernel = np.ones(9) / 9
    smooth = np.convolve(psd, kernel, mode="same")

    band = (freqs >= SEARCH_FROM_HZ) & (freqs <= SEARCH_TO_HZ)
    f = freqs[band]
    s = smooth[band]
    if f.size < MIN_BINS:
        return None

    step = max(1, int(STEP_HZ / (freqs[1] - freqs[0])))
    drops = s[:-step] - s[step:]
    if not drops.size:
        return None
    at = int(np.argmax(drops))
    return float(f[at]) / 1000, float(drops[at])


def nearest_tier(bitrate: int) -> int:
    """The `EXPECTED` row a declared bitrate belongs to."""
    return min((abs(tier - bitrate), tier) for tier in EXPECTED)[1]


def likely_source(cutoff_khz: float) -> int:
    """Which bitrate would naturally produce this cutoff."""
    return min((abs(want - cutoff_khz), tier) for tier, want in EXPECTED.items())[1]


def judge(bitrate: int, *, lossless: bool, cutoff: float | None, cliff: float | None) -> tuple[str, str]:
    """What to make of one measurement."""
    if cutoff is None:
        return "unmeasured", "too short or silent in the sampled window"
    if cliff is not None and cliff < CLIFF_DB:
        # No filter edge. The music simply has nothing up there, which is a fact
        # about the recording and not a charge against the file.
        return "no sharp cutoff", f"falls {cliff:.0f} dB — a gentle roll-off, not a filter"
    if lossless:
        if cutoff < LOSSLESS_FLOOR_KHZ:
            return "LOSSY SOURCE", f"lossless container, but the audio stops at {cutoff:.1f} kHz"
        return "ok", ""

    want = EXPECTED[nearest_tier(bitrate)]
    if cutoff < want - SLACK_KHZ:
        likely = likely_source(cutoff)
        return "TRANSCODE?", f"declares {bitrate}k but cuts at {cutoff:.1f} kHz — looks like a {likely}k source"
    return "ok", ""


def inspect(path_str: str) -> Measured:
    """Measure and judge one file. Runs in a worker process."""
    try:
        handle = MutagenFile(path_str)
    except (MutagenError, OSError):
        handle = None
    if handle is None or handle.info is None:
        return Measured(path_str, "?", 0, None, None, "unmeasured", "no readable stream info")

    codec = type(handle.info).__name__.replace("Info", "").lower()
    bitrate = round(getattr(handle.info, "bitrate", 0) / 1000)
    lossless = Path(path_str).suffix.lower() in {".flac", ".wav", ".aiff", ".alac"}

    got = measure(path_str)
    cutoff, cliff = got or (None, None)
    verdict, note = judge(bitrate, lossless=lossless, cutoff=cutoff, cliff=cliff)
    return Measured(path_str, codec, bitrate, cutoff, cliff, verdict, note)


def survey(root: Path, out: Path, tier: int | None, workers: int) -> None:
    """Measure every audio file under `root` and write the findings."""
    paths = [str(p) for p in walk(root) if p.suffix.lower() in AUDIO]
    if tier is not None:
        paths = [p for p in paths if _declared(p) == tier]
    print(f"measuring {len(paths)} file(s) with {workers} workers — a few minutes")

    rows: list[dict[str, object]] = []
    tally: collections.Counter[str] = collections.Counter()
    with ProcessPoolExecutor(max_workers=workers) as pool:
        for m in pool.map(inspect, paths, chunksize=8):
            marked = processed(Path(m.path))
            verdict = m.verdict
            # **A re-encode the audio does not show** — #143. The cutoff test
            # catches a lossy ancestor by the wall an encoder leaves; a
            # competent 320-to-320 pass leaves no wall, and 130 of this
            # collection's 144 marked files measure clean. The marker is the
            # only record that it happened.
            if marked and verdict == "ok":
                verdict = "re-encoded"
            tally[verdict] += 1
            if verdict == "ok":
                continue
            _, artist, album = named(Path(m.path))
            rows.append(
                {
                    "verdict": verdict,
                    "processed": "y" if marked else "",
                    "artist": artist,
                    "album": album,
                    "title": Path(m.path).stem,
                    "declared": m.bitrate,
                    "cutoff_khz": f"{m.cutoff_khz:.1f}" if m.cutoff_khz else "",
                    "cliff_db": f"{m.cliff_db:.0f}" if m.cliff_db else "",
                    "codec": m.codec,
                    "note": m.note
                    or ("a mastering tool re-encoded this; the audio does not show it" if marked else ""),
                    "path": m.path,
                }
            )

    # **Written even when it is empty** — a survey that found nothing used to
    # leave the last one that found something standing, so a clean library went
    # on being described by a stale file listing work already done.
    rows.sort(key=_worst_first)
    write_tsv(out, rows)
    for verdict, n in tally.most_common():
        print(f"  {n:6d}  {verdict}")
    print(f"\n{len(rows)} row(s) -> {out}")


def processed(path: Path) -> bool:
    """Whether a filename records that a mastering tool re-encoded this — #143.

    **Evidence the measurement cannot reach.** The cutoff test finds a lossy
    ancestor by the wall an encoder leaves; a competent re-encode from a good
    source leaves no wall, so a 320 that was decoded and written again looks
    exactly like one that was not. The marker is the only record that it
    happened, and it is on the path — which is why it must be read before a
    rename takes it away.

    Measured here: 9.7% of marked files fail the cutoff test against 6.2% of
    the rest, so the marker is a poor *predictor* of damage and a complete
    record of provenance. Those are different jobs.
    """
    return path.stem.endswith(PROCESSED)


def _declared(path: str) -> int:
    """The bitrate a file claims, for `--tier`."""
    try:
        handle = MutagenFile(path)
    except (MutagenError, OSError):
        return 0
    if handle is None or handle.info is None:
        return 0
    b = round(getattr(handle.info, "bitrate", 0) / 1000)
    return nearest_tier(b)


def named(path: Path) -> tuple[str, str, str]:
    """`(collection, artist, album)` from a library path.

    Read from the path rather than the tags on purpose. These files may have
    been moved out by `quarantine`, and a row that cannot say which album it is
    about the moment the folder relocates is a row you have to read a path to
    use. The layout is `Collection/Artist/Album/track`, so the last three
    directories are the answer wherever the tree is rooted.
    """
    parts = list(path.parts[:-1] if path.suffix else path.parts)
    # Pad so a short path yields empty strings rather than an index error.
    while len(parts) < NAMED_DEPTH:
        parts.insert(0, "")
    collection, artist, album = parts[-NAMED_DEPTH:]
    return collection, artist, album


def _worst_first(row: dict[str, object]) -> tuple[int, float]:
    """Sort key: the biggest lies first, then by how far short they fall."""
    rank = {"LOSSY SOURCE": 0, "TRANSCODE?": 1, "no sharp cutoff": 2, "unmeasured": 3}
    return rank.get(str(row["verdict"]), 9), float(str(row["cutoff_khz"]) or 99)


def _worst_album_first(row: dict[str, object]) -> float:
    """Sort key: highest score first."""
    return -float(str(row["score"]))


def albums(review: Path, out: Path, limit: int, under: int | None, min_share: float) -> None:
    """Roll the per-file findings up to albums worth re-acquiring.

    Ranked by `share x deficit`, and both halves matter. An album with one bad
    track among twelve is a bad track; an album where every track cuts at 16 kHz
    is a bad *source*, and that is the one worth buying again. Multiplying by how
    far short it falls keeps a wholly-16 kHz album above a wholly-19 kHz one.

    **The deficit is measured against clean audio, not against the declaration.**
    Ranking by how much a file *lied* puts a mislabelled 320 above a genuine
    64 kbps rip, which is backwards for a list whose purpose is deciding what to
    buy again: the 64 kbps album sounds worse and is the better use of money.
    `declared` stays in the output, so the lie is still visible.

    The totals come from the directory rather than from this file, because the
    review lists only what was flagged — dividing flagged by flagged would score
    every album 100%.

    `under` filters on what the audio *sounds like*, not on what it claims: an
    album declaring 320 whose cutoff says 128 is under 190 by the only measure
    that matters here.

    `min_share` is about **blast radius**. Judging an album by its worst track
    moves nine good ones out with the two bad: *Bringing It All Back Home* is
    2 of 11, which is not a bad rip, it is a mostly-good album with two odd
    files. `carries` counts what would come along for the ride.
    """
    if not review.exists():
        print(f"no {review.name} — run `survey` first")
        return

    flagged: dict[Path, list[tuple[float, int]]] = collections.defaultdict(list)
    with review.open(encoding="utf-8") as handle:
        for row in csv.DictReader(handle, delimiter="\t"):
            if row["verdict"] != "TRANSCODE?" or not row["cutoff_khz"]:
                continue
            path = Path(row["path"])
            flagged[path.parent].append((float(row["cutoff_khz"]), int(row["declared"])))

    rows: list[dict[str, object]] = []
    for folder, found in flagged.items():
        total = sum(1 for f in folder.iterdir() if f.suffix.lower() in AUDIO)
        if not total:
            continue
        cutoffs = sorted(c for c, _ in found)
        median = cutoffs[len(cutoffs) // 2]
        declared = collections.Counter(b for _, b in found).most_common(1)[0][0]
        deficit = CLEAN_KHZ - median
        share = len(found) / total
        looks_like = likely_source(median)
        if under is not None and looks_like >= under:
            continue
        if share * 100 < min_share:
            continue
        collection, artist, album = named(folder)
        rows.append(
            {
                "score": f"{share * deficit:.1f}",
                "collection": collection,
                "artist": artist,
                "album": album,
                "flagged": len(found),
                "of": total,
                "share": f"{share * 100:.0f}%",
                "declared": declared,
                "median_khz": f"{median:.1f}",
                "worst_khz": f"{cutoffs[0]:.1f}",
                "looks_like": looks_like,
                # What moving the whole folder takes with it.
                "carries": total - len(found),
                "path": str(folder),
            }
        )

    rows.sort(key=_worst_album_first)
    matched = len(rows)
    rows = rows[:limit]
    write_tsv(out, rows)
    whole = sum(1 for r in rows if r["flagged"] == r["of"])
    moving = sum(int(str(r["of"])) for r in rows)
    carried = sum(int(str(r["carries"])) for r in rows)
    if under is not None:
        print(f"{matched} album(s) sound like they came from under {under}k")
    print(f"{len(rows)} album(s) -> {out}")
    print(f"  {whole} of them are flagged end to end — a bad source, not a bad track")
    print(f"  {moving} files in total, of which {carried} are not themselves flagged")


def _target_for(folder: Path, root: Path, dest: Path, tally: collections.Counter[str]) -> Path | None:
    """Where a folder should land, or None with a reason counted."""
    if not folder.is_dir():
        tally["already gone"] += 1
        return None
    try:
        target = dest / folder.relative_to(root)
    except ValueError:
        tally["outside the library"] += 1
        return None
    if target.exists():
        tally["target already there"] += 1
        return None
    return target


def quarantine(review: Path, root: Path, dest: Path, manifest: Path, *, write: bool) -> None:
    """Move whole album folders out of the library.

    **Folders move entire or not at all.** Nothing is cherry-picked out of an
    album: the un-flagged tracks travel with their siblings, because an album
    split across two trees is worse than either half. `carries` in the review
    says how many that is, so the cost is known before the move rather than
    discovered after it.

    The path under `dest` mirrors the one under `root`, artist level included —
    `General/Ween/The Pod` stays `General/Ween/The Pod` — so the quarantine
    reads like the library it came from rather than a heap of album names.

    An artist folder emptied by the move is removed, but only when nothing at
    all is left in it. A stray file is a reason to stop, not to sweep.
    """
    if not review.exists():
        print(f"no {review.name} — run `albums` first")
        return
    with review.open(encoding="utf-8") as handle:
        folders = [Path(row["path"]) for row in csv.DictReader(handle, delimiter="\t")]

    moves: list[dict[str, object]] = []
    tally: collections.Counter[str] = collections.Counter()
    for folder in folders:
        target = _target_for(folder, root, dest, tally)
        if target is None:
            continue
        files = sum(1 for f in folder.iterdir() if f.suffix.lower() in AUDIO)
        moves.append({"files": files, "from": str(folder), "to": str(target)})
        if write:
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(str(folder), str(target))
            # Only when the artist has nothing left at all.
            if not any(folder.parent.iterdir()):
                folder.parent.rmdir()
                tally["artist folder emptied"] += 1
        tally["moved" if write else "would move"] += 1

    if moves and write:
        write_tsv(manifest, moves)
        print(f"manifest of {len(moves)} move(s) -> {manifest}")
    for key, n in sorted(tally.items()):
        print(f"  {n:6d}  {key}")
    print(f"  {sum(int(str(m['files'])) for m in moves)} audio files in those folders")
    if not write:
        print("dry run — pass --write to actually move")


def unquarantine(manifest: Path, *, write: bool) -> None:
    """Put quarantined albums back where they came from."""
    if not manifest.exists():
        print(f"no {manifest.name}")
        return
    tally: collections.Counter[str] = collections.Counter()
    with manifest.open(encoding="utf-8") as handle:
        for row in csv.DictReader(handle, delimiter="\t"):
            src, back = Path(row["to"]), Path(row["from"])
            if not src.is_dir():
                tally["not in quarantine"] += 1
                continue
            if back.exists():
                tally["something is already back there"] += 1
                continue
            if write:
                back.parent.mkdir(parents=True, exist_ok=True)
                shutil.move(str(src), str(back))
                # Symmetry with the move: an artist left holding nothing goes
                # too, so a restored quarantine is empty rather than a skeleton.
                if src.parent.is_dir() and not any(src.parent.iterdir()):
                    src.parent.rmdir()
            tally["restored" if write else "would restore"] += 1
    for key, n in sorted(tally.items()):
        print(f"  {n:6d}  {key}")
    if not write:
        print("dry run — pass --write to actually restore")


def propose(rows: Path, out: Path, analyser: int) -> None:
    """Turn a survey into facts the library will take.

    **Reads the query output, not the survey TSV.** The survey names files by
    path, and a path is not an identifier: it is a property of one device's
    copy, stale the moment a file moves and meaningless on another machine.

    So the input is what `gmuse library query --format jsonl` handed us, which
    carries both halves — the `encoding` id to propose against and the
    `locator` to open. This tool never computes an identity and should not:
    `audio_id` is a hash over Symphonia's packet boundaries.

    Files that could not be measured are **skipped rather than proposed as
    zero**. A missing measurement and a measurement of nothing are different
    facts, and only one of them is true.
    """
    if not rows.exists():
        print(f"no {rows.name} — run `gmuse library query … --format jsonl` first")
        return

    written = skipped = 0
    with rows.open(encoding="utf-8") as handle, out.open("w", encoding="utf-8") as sink:
        for raw in handle:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            row = json.loads(line)
            encoding, locator = row.get("encoding"), row.get("locator")
            if not encoding or not locator:
                # Nothing here holds a copy, so there is nothing to measure.
                skipped += 1
                continue
            found = measure(locator)
            if found is None:
                skipped += 1
                continue
            cutoff_khz, cliff = found
            # **The cliff travels with the cutoff** — #184. It was measured
            # here and dropped on the floor, so 7,210 facts recorded where a
            # spectrum stops and not whether that was an encoder or the music,
            # and `albums` had to re-read every file to say which.
            sink.write(
                json.dumps(
                    {
                        "fact": "measured",
                        "encoding": encoding,
                        "cutoff_hz": round(cutoff_khz * 1000),
                        "cliff_db": min(round(cliff), 255) if cliff is not None else None,
                        "analyser": analyser,
                    }
                )
                + "\n"
            )
            written += 1

    print(f"{written} measurement(s) -> {out}")
    if skipped:
        print(f"  {skipped} row(s) had nothing to measure and were left out, not proposed as zero")
    print(f"\nreview it, then: gmuse library apply {out}")


LIB = typer.Argument(Path("~/MusicLibrary"), help="library root")
WRITE = typer.Option(default=False, help="actually do it; without this, a dry run")


def register(app: typer.Typer) -> None:
    """Attach this pass's commands to the tool."""

    @app.command("survey")
    def survey_cmd(
        root: Path = LIB,
        tier: int | None = typer.Option(None, help="only files declaring this bitrate"),
        workers: int = typer.Option(8, help="parallel decoders"),
    ) -> None:
        """Measure every file's spectrum. Writes no tags."""
        survey(root.expanduser(), out_path("quality-review.tsv"), tier, workers)

    @app.command("albums")
    def albums_cmd(
        limit: int = typer.Option(100),
        under: int | None = typer.Option(None, help="only albums that sound under this bitrate"),
        min_share: float = typer.Option(0.0, help="only albums with at least this share flagged"),
    ) -> None:
        """Roll the findings up to albums worth re-acquiring."""
        albums(
            out_path("quality-review.tsv"),
            out_path("quality-albums.tsv"),
            limit,
            under,
            min_share,
        )

    @app.command("propose")
    def propose_cmd(
        rows: Path = typer.Argument(..., help="jsonl from `gmuse library query --format jsonl`"),
        out: Path = typer.Option(Path("quality-proposals.jsonl")),
        analyser: int = typer.Option(
            1,
            help="must match model::encoding::ANALYSER, so a re-measure can find stale rows",
        ),
    ) -> None:
        """Measure what a query listed, and say so as proposed facts.

        The output is **not** under `out_path`: a proposal is a work product
        rather than this tool's state, so it goes where the caller says and
        defaults to the working directory — `library-api.md` §7.
        """
        propose(rows.expanduser(), out.expanduser(), analyser)

    @app.command("quarantine")
    def quarantine_cmd(
        root: Path = LIB,
        dest: Path = typer.Option(Path("~/LowQualityMusic")),
        *,
        write: bool = WRITE,
    ) -> None:
        """Move the listed album folders out of the library."""
        quarantine(
            out_path("quality-albums.tsv"),
            root.expanduser(),
            dest.expanduser(),
            out_path("quarantine-manifest.tsv"),
            write=write,
        )

    @app.command("unquarantine")
    def unquarantine_cmd(*, write: bool = WRITE) -> None:
        """Put them back, from the manifest the move wrote."""
        unquarantine(out_path("quarantine-manifest.tsv"), write=write)
