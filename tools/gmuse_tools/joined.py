#!/usr/bin/env python3
"""Is this credit one artist or several, when the tags do not say — issue #152.

`credits.py` asks a neighbouring question and answers it with the **MusicBrainz
id count**: two names and two ids mean the slash is a separator, two names and
one id mean it is a character, so `AC/DC` survives. That arbiter is right and
this pass does not replace it. It is simply absent on the files that need it
most — 1 of the 96 DJ singles imported from `DanceMusic/1980-2014` carried a
MusicBrainz id at all, against 92.7% of the library it landed in.

Without an id, an artist is minted from the credit *string*, so
`Armand Van Helden,Dizzee Rascal` becomes an artist and Stephan Bodzin becomes
four people. What this pass must not do is fix that with a rule about
punctuation: the same library holds `Angus & Julia Stone`, `Itch-E &
Scratch-E`, `Bob Dylan & The Band`, and `Lonnie Young, Jr.` — a comma **inside**
one person's name.

So it corroborates instead, and reports rather than splits.

## Three signals from the library itself, and one from outside

- **reordered** — the same members under two spellings. A band name is stable;
  nothing writes `Scratch-E & Itch-E`. A credit list is not: `Marc Romboy,
  Stephan Bodzin` / `Stephan Bodzin & Marc Romboy` / `Stephan Bodzin vs. Marc
  Romboy` are three spellings of one pair, and the reordering is the proof.
- **all members solo** — every part is also credited alone somewhere.
- **shares members** — a *different* credit overlaps by two or more:
  `Kiko, Spencer K` beside `Kiko,Oxia,Spencer K`.
- **MusicBrainz**, for what is left: does an artist exist under the whole
  string, and under each part?

Measured on this library: the three local signals decide 40 of 92 with **no
false positives**, MusicBrainz decides a further 42, and 10 go to a person.

## What it will not do

**Nothing here writes.** A credit stays one artist until somebody says
otherwise, because the failure mode is not symmetric: a split is repairable by
a merge (#88) and a wrong merge is not.

    uv run gmuse-tools joined survey ~/MusicLibrary     # -> out/joined-review.tsv
    uv run gmuse-tools joined survey ~/MusicLibrary --offline   # skip the lookup

    gmuse shelf query > work.jsonl                     # the library says what it holds
    uv run gmuse-tools joined propose work.jsonl       # -> out/joined-propose.jsonl
    gmuse shelf apply out/joined-propose.jsonl         # dry run
    gmuse shelf apply out/joined-propose.jsonl --write

`propose` reads the **work list** rather than walking a disk, for `identify.py`'s
reason: a proposal has to name an id the library knows, and a path is a property
of one machine's copy. It proposes only what a verdict called a `list`, and a
credit the pass could not settle produces no line at all.
"""

from __future__ import annotations

import collections
import json
import re
import sqlite3
import sys
import unicodedata as u
from pathlib import Path
from typing import TYPE_CHECKING, Final, NamedTuple

import typer

from gmuse_tools import bronze
from gmuse_tools.genre.common import out_path, tags_of, walk, write_tsv

if TYPE_CHECKING:
    from collections.abc import Callable, Iterable

#: Two credits must share at least this many members before the overlap says
#: anything. One shared person is a collaboration — `Hank Williams` appears in
#: two unrelated credits here — and only a second makes it a list.
ENOUGH_SHARED: Final = 2
#: How many names make a credit a list at all rather than one artist.
SEVERAL: Final = 2

#: What separates two credited artists.
#:
#: `and` earns its place on `Tom Waits and Crystal Gayle` and costs three false
#: splits — `J.E. Mainer and Band`, `Neko Case and Her Boyfriends`,
#: `Leroy Miller, with hoe group`. It is kept because those land in `unsure`
#: rather than in a proposal, which is the outcome they want anyway.
#:
#: `feat.` earns its place on the eleven `2Pac feat. …` credits, which the
#: first version left as `unsure` for the wrong reason: without it the credit
#: split to `["2Pac feat. Danny Boy", "Syke", "CPO"]` and MusicBrainz was asked
#: about an artist that has never existed. This is the very case
#: `Scanned::credit_is_joined` documents — `2Pac feat. Nate Dogg` is not `2Pac`
#: to a hash — so it was always in scope.
#:
#: `with` is **not** here, and the pair that argues both ways is why:
#: `Emmylou Harris with Dolly Parton and Linda Ronstadt` wants it and
#: `Leroy Miller, with hoe group` does not, since `hoe group` is not an artist
#: under any spelling. Two credits, opposite answers, so it goes to a person.
#:
#: `/` is **not** here: it is `credits.py`'s separator and its arbiter is
#: better. Splitting `AC/DC` is that pass's job to refuse, not this one's.
JOINERS: Final = re.compile(
    r"\s*,\s*|\s+&\s+|\s+and\s+|\s+feat\.?\s+|\s+ft\.?\s+|\s+[xX]\s+|\s+vs\.?\s+", re.IGNORECASE
)

#: The joiners that still apply **inside** a comma-separated list — everything
#: but `&` and `and`.
#:
#: A comma is the list separator, so a `&` between two commas is part of a name:
#: `Chus & Ceballos` is a duo, and splitting it invented `Chus` and `Ceballos`.
#: A `feat.` never is. `2Pac feat. Danny Boy, Syke & CPO` is four people and the
#: reason `feat.` joined this pattern in the first place, so dropping it here
#: would trade one bug for the one it replaced.
WITHIN: Final = re.compile(
    r"\s+feat\.?\s+|\s+ft\.?\s+|\s+[xX]\s+|\s+vs\.?\s+", re.IGNORECASE
)

MB_ARTIST: Final = "https://musicbrainz.org/ws/2/artist"
#: The one status that carries an answer.
HTTP_OK: Final = 200
USER_AGENT: Final = {"User-Agent": "gmuse-joined/0.1 ( https://gitlab.com/gabrielgellner/gmuse )"}


def key(name: str) -> str:
    """A name reduced to what a comparison should ignore.

    Matches `ingest::normalise`'s shallowness in spirit — case and spacing go,
    accents stay — but goes further because this is a *comparison* and never an
    id. Nothing is minted from it, so folding here cannot create the
    unrepairable merge the Rust side is careful about.
    """
    return re.sub(r"[^a-z0-9]+", "", u.normalize("NFKD", name).lower())


def members(credit: str) -> tuple[str, ...]:
    """The names a credit appears to list, in the order it lists them.

    **A comma is the list separator, and inside a list an `&` is part of a
    name.** `Chus & Ceballos, DJ Chus, Pablo Ceballos, Leonardo Gonnelli` was
    split on both and produced `Chus` and `Ceballos` — two artists who have
    never existed, since the duo *is* DJ Chus and Pablo Ceballos. Worse, the
    corroboration agreed: a sibling credit listed the same wrongly-split parts
    in another order, so the pass confirmed its own mistake and marked it
    `list`.

    **Except the last item**, because that is where English puts the `&`:
    `Dolly Parton, Linda Ronstadt & Emmylou Harris` is three people and only
    the final comma-part carries the joiner. So commas divide the list, the
    tail is divided by everything, and an ampersand anywhere else stays inside
    the name it belongs to.

    **`feat.` keeps applying throughout**, because it is never part of an act's
    name: `2Pac feat. Danny Boy, Syke & CPO` is four people, and it is the case
    that put `feat.` in `JOINERS` to begin with.

    A credit with no comma at all is not a list yet, so every joiner applies —
    which is what makes `Stephan Bodzin & Marc Romboy` two.
    """
    if "," not in credit:
        return tuple(p.strip() for p in JOINERS.split(credit) if p.strip())
    listed = [p.strip() for p in credit.split(",") if p.strip()]
    if not listed:
        return ()
    within = [p.strip() for part in listed[:-1] for p in WITHIN.split(part) if p.strip()]
    tail = [p.strip() for p in JOINERS.split(listed[-1]) if p.strip()]
    return tuple(within + tail)


class Verdict(NamedTuple):
    """One credit, and what is known about it."""

    credit: str
    verdict: str
    why: str
    parts: tuple[str, ...]


def reordered(corpus: Iterable[str]) -> set[str]:
    """Credits whose members appear elsewhere in a different order.

    The strongest of the three, and the only one that needs no other evidence:
    a name does not get reordered, a list does.

    **Order, not spelling.** `Neko Case & Her Boyfriends` and `Neko Case and
    Her Boyfriends` are the same two members in the same order, which is a
    spelling variant and not evidence of anything. An early version counted it
    and flagged a real band.
    """
    by_set: dict[frozenset[str], set[str]] = collections.defaultdict(set)
    for c in corpus:
        parts = members(c)
        if len(parts) >= SEVERAL:
            by_set[frozenset(key(p) for p in parts)].add(c)
    out: set[str] = set()
    for group in by_set.values():
        if len({tuple(key(p) for p in members(c)) for c in group}) > 1:
            out |= group
    return out


def all_solo(credit: str, solo: frozenset[str]) -> bool:
    """Every member is also credited on its own somewhere."""
    parts = members(credit)
    return len(parts) >= SEVERAL and all(key(p) in solo for p in parts)


def shares_members(credit: str, corpus: Iterable[str]) -> str | None:
    """Another, *different* credit that overlaps by two or more members.

    **Different**, and that word is the whole test. Comparing a credit against
    a punctuation variant of itself finds two shared members every time, which
    flagged `Neko Case & Her Boyfriends` against `Neko Case and Her Boyfriends`
    and would have shipped a false positive on a real band.
    """
    mine = frozenset(key(p) for p in members(credit))
    if len(mine) < ENOUGH_SHARED:
        return None
    for other in corpus:
        theirs = frozenset(key(p) for p in members(other))
        if theirs != mine and len(mine & theirs) >= ENOUGH_SHARED:
            return other
    return None


def locally(corpus: list[str]) -> dict[str, Verdict]:
    """What the library alone can say. No network, no guesses about spelling."""
    solo = frozenset(key(c) for c in corpus if len(members(c)) == 1)
    swapped = reordered(corpus)
    out: dict[str, Verdict] = {}
    for c in corpus:
        parts = members(c)
        if len(parts) < SEVERAL:
            continue
        if c in swapped:
            out[c] = Verdict(c, "list", "the same members appear in another order", parts)
        elif all_solo(c, solo):
            out[c] = Verdict(c, "list", "every member is credited alone elsewhere", parts)
        elif (other := shares_members(c, corpus)) is not None:
            out[c] = Verdict(c, "list", f"shares two or more members with {other!r}", parts)
        else:
            out[c] = Verdict(c, "unsure", "nothing in the library corroborates it", parts)
    return out


def ask_musicbrainz(con: sqlite3.Connection, name: str) -> dict[str, str] | None:
    """Does MusicBrainz hold an artist known by exactly this name?

    **Aliases count, and leaving them out was a real bug.** A tag carries the
    name a record was *credited* to, which is often not the name MusicBrainz
    files the artist under: `Puffy AmiYumi` is an alias of `PUFFY`,
    `Me'Shell NdegéOcello` of `Meshell Ndegeocello`, `Dramacydal` of `Outlawz`.
    Searching the primary name alone reported all three as unknown, which read
    as a limit of the method and was a limit of the query.

    **An exact name match is still not an identification.** `The Game`,
    `La Fleur` and `Sonya Elise` are common enough that the artist returned may
    have nothing to do with the record in hand. That is tolerable because the
    output is reviewed and never applied, and it is the second reason this pass
    proposes nothing.
    """
    status, body = bronze.get(
        con,
        MB_ARTIST,
        {"query": f'artist:"{name}" OR alias:"{name}"', "fmt": "json", "limit": "5"},
        headers=USER_AGENT,
    )
    if status != HTTP_OK or not isinstance(body, dict):
        return None
    for a in body.get("artists", []):
        known = [a.get("name", ""), *(al.get("name", "") for al in a.get("aliases", []))]
        if any(key(k) == key(name) for k in known):
            as_filed = a.get("name", "")
            note = a.get("disambiguation") or ""
            if key(as_filed) != key(name):
                note = f"filed as {as_filed}" + (f", {note}" if note else "")
            return {"name": as_filed, "type": a.get("type") or "", "id": a["id"], "note": note}
    return None


def resolve(v: Verdict, ask: Callable[[str], dict[str, str] | None]) -> Verdict:
    """Ask an authority about one credit the library could not settle.

    The whole string is the decisive question. Where MusicBrainz holds an
    artist under it, that is a credited entity and the credit stays whole —
    **even when the members also exist**, which is the ordinary case for a duo.
    `Angus & Julia Stone` is a group whose members record solo, and both facts
    being true does not make it a coin flip.
    """
    whole = ask(v.credit)
    hits = [ask(p) for p in v.parts]
    found = [h for h in hits if h]
    if whole is not None:
        note = f"MusicBrainz: {whole['type'] or 'artist'}"
        if whole["note"]:
            note += f", {whole['note']}"
        return v._replace(verdict="one", why=note)
    if len(found) == len(v.parts):
        return v._replace(verdict="list", why="MusicBrainz holds every member and not the whole")
    if found:
        missing = [p for p, h in zip(v.parts, hits, strict=True) if not h]
        return v._replace(verdict="unsure", why=f"MusicBrainz knows {len(found)} of {len(v.parts)}; not {missing}")
    return v._replace(verdict="unsure", why="MusicBrainz knows neither the whole nor the members")


def credits_under(root: Path) -> list[str]:
    """Every artist and album-artist credit the files under `root` carry.

    Both fields, because they answer different questions and disagree: the
    album credit is what the tree files by, and the track credit is where a
    guest shows up. A pass that read only one would miss half the cases.
    """
    seen: collections.Counter[str] = collections.Counter()
    for path in walk(root):
        tags = tags_of(path)
        for field in ("albumartist", "album_artist", "artist"):
            value = tags.get(field, "").strip()
            if value:
                seen[value] += 1
    return list(seen)


def settle(corpus: list[str], *, offline: bool = False) -> dict[str, Verdict]:
    """Decide what can be decided: the library first, then an authority.

    The library first because it is free and has no false positives on this
    data, and an authority only for what is left — which is also what keeps a
    rate limit off the 37 credits that never needed it.
    """
    decided = locally(corpus)
    if offline:
        return decided
    con = bronze.open_cache()
    cache: dict[str, dict[str, str] | None] = {}

    def ask(name: str) -> dict[str, str] | None:
        if name not in cache:
            cache[name] = ask_musicbrainz(con, name)
        return cache[name]

    unsure = [v for v in decided.values() if v.verdict == "unsure"]
    for i, v in enumerate(unsure, 1):
        print(f"  {i}/{len(unsure)} asking about {v.credit}", file=sys.stderr)
        decided[v.credit] = resolve(v, ask)
    return decided


def survey(root: Path, out: Path, *, offline: bool = False) -> None:
    """Report every joined-looking credit and what is known about it."""
    found = credits_under(root)
    decided = settle(found, offline=offline)

    def order(v: Verdict) -> tuple[str, str]:
        return (v.verdict, v.credit.lower())

    rows = sorted(decided.values(), key=order)
    write_tsv(
        out,
        [{"verdict": v.verdict, "credit": v.credit, "members": " | ".join(v.parts), "why": v.why} for v in rows],
    )
    tally = collections.Counter(v.verdict for v in rows)
    for name, n in tally.most_common():
        print(f"{n:5}  {name}", file=sys.stderr)
    print(f"\n{len(rows)} joined-looking credit(s) of {len(found)} -> {out}", file=sys.stderr)
    print("nothing was written to the library: a credit stays one artist until somebody says.", file=sys.stderr)


LIB = typer.Argument(Path("~/MusicLibrary"), help="library root")
OFFLINE = typer.Option(default=False, help="use only what the library itself can corroborate")
WORK = typer.Argument(Path("work.jsonl"), help="the work list from `gmuse shelf query`")


def register(app: typer.Typer) -> None:
    """Attach this pass to the CLI."""
    sub = typer.Typer(help=__doc__, no_args_is_help=True)

    @sub.command("survey")
    def survey_cmd(root: Path = LIB, *, offline: bool = OFFLINE) -> None:
        """Report which credits name several artists. Writes nothing to the library."""
        survey(root.expanduser(), out_path("joined-review.tsv"), offline=offline)

    @sub.command("propose")
    def propose_cmd(work: Path = WORK, root: Path = LIB, *, offline: bool = OFFLINE) -> None:
        """Turn the settled verdicts into facts `gmuse shelf apply` can read."""
        propose(work.expanduser(), root.expanduser(), out_path("joined-propose.jsonl"), offline=offline)

    app.add_typer(sub, name="joined")


def proposals(work: list[dict[str, str]], decided: dict[str, Verdict]) -> list[dict[str, object]]:
    """Turn settled verdicts into lines `gmuse shelf apply` can read.

    **Only `list`.** A credit that is one artist needs no proposal and a credit
    nothing settled must not get one — the whole discipline of this pass is
    that an unsure row goes to a person rather than to the library.

    The members are proposed **as the credit spells them**, not as MusicBrainz
    files them. A name from an authority is a separate claim somebody else can
    propose, and `CreditAsserted` already has `as_credited` for the difference;
    conflating the two would smuggle a rename into a split.
    """
    out: list[dict[str, object]] = []
    for row in work:
        v = decided.get(row.get("artist", ""))
        if v is None or v.verdict != "list":
            continue
        out.append(
            {
                "fact": "credited",
                "recording": row["recording"],
                "artists": list(v.parts),
                # `inferred` where the library corroborated it, because that is
                # what it was: a reading of what this library already holds.
                "basis": "musicbrainz" if v.why.startswith("MusicBrainz") else "inferred",
            }
        )
    return out


def propose(work: Path, root: Path, out: Path, *, offline: bool = False) -> None:
    """Read the work list, settle what can be settled, and write the proposals.

    **Two sources, and they answer different questions.** The work list says
    which recording a credit belongs to, and it is the only thing that may:
    a proposal has to name an id the library knows. The *tags* are what the
    corroboration reads, because the library's answer is already canonical
    where MusicBrainz named it — measured here, 338 distinct artist strings
    against the tags' 503, and the 165 missing are mostly the per-track
    `feat.` credits that corroborate each other. Settling on the narrow corpus
    decided 4 credits; on the wide one, 57.
    """
    rows = [json.loads(line) for line in work.read_text().splitlines() if line.strip()]
    corpus = sorted({r["artist"] for r in rows if r.get("artist")} | set(credits_under(root)))
    decided = settle(corpus, offline=offline)
    lines = proposals(rows, decided)

    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as fh:
        for line in lines:
            fh.write(json.dumps(line) + "\n")

    settled = sum(1 for v in decided.values() if v.verdict == "list")
    unsure = sum(1 for v in decided.values() if v.verdict == "unsure")
    print(f"{len(lines)} proposal(s) over {settled} settled credit(s) -> {out}", file=sys.stderr)
    print(f"{unsure} credit(s) left to a person, and no line was written for them.", file=sys.stderr)
