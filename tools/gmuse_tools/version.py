#!/usr/bin/env python3
"""Which version of a song a recording is — issue #171, `facet::VERSION`.

The facet has existed and been documented since #117 — *"Which version of a
song you are holding. `live`, `instrumental`."* — and had **never once been
asserted**. Meanwhile the library carries 1,114 parentheticals across 519
distinct forms, which is that same fact written into a title where nothing can
filter on it.

## Three kinds live in those brackets, and only one is this pass's

- **a version** — `(live)`, `(acoustic)`, `(radio edit)`. 620 of them, and what
  this pass is for.
- **a version by somebody** — `(Fur Coat remix)`, `(Joris Voorn remix)`. 188,
  and already answered: a remixer is a *credit*, `Contributed` with role
  `remixer`, settled by `joined remixed` where three sources agree (#170).
  Filing them as versions would put a person's name on an axis of renderings.
- **part of the title** — `(to Take My Man)`, `(Just Ask)`, `(Sooner or
  Later)`. **306**, which is more than a quarter, and the reason this pass is
  built the way it is.

## Exact match, never substring

`You Ain't Woman Enough (to Take My Man)` is the song's name. So is `Jesse
James (Just Ask)`. Both were stripped by a render and restored by hand, and
that is the whole lesson: a parenthetical is a title until a curated term says
otherwise.

Substring matching would have found `mix` inside `Danny Morris mix`, `Monotone
mix` and `Hell mix` — three remix names — and `live` inside nothing useful. So
the whole bracket is normalised and looked up. A form that is not in the table
is **reported, never guessed**, which is the discipline `joined.py` follows for
the same reason and `hierarchy.py` follows for the genre tree.

## What is deliberately not a term

`intro`, `outro`, `interlude` are excluded on a different ground: they say what
a *track* is, not which rendering of a work you hold. That is `facet::FUNCTION`
if it is anywhere, and guessing across axes is how a taxonomy stops being
trustworthy.
"""

from __future__ import annotations

import collections
import json
import re
import sys
import unicodedata as u
from pathlib import Path
from typing import TYPE_CHECKING, Final

import typer

from gmuse_tools.genre.common import out_path, write_tsv

if TYPE_CHECKING:
    from collections.abc import Iterable

#: The controlled vocabulary. Every value in `SPELLINGS` is one of these, so a
#: term can be renamed in one place and nothing drifts.
TERMS: Final[frozenset[str]] = frozenset(
    {
        "original",
        "live",
        "acoustic",
        "instrumental",
        "demo",
        "rehearsal",
        "radio-edit",
        "single-edit",
        "album-edit",
        "extended-mix",
        "club-mix",
        "dub",
        "vocal-mix",
        "mono",
        "remaster",
        "reprise",
        "excerpt",
        "alternate-take",
        "alternate-mix",
        "overdub",
        "undub",
        "medley",
    }
)

#: **Terms the library already holds, adopted rather than improved on.**
#:
#: `extended-mix` was here before this vocabulary was — asserted by an earlier
#: pass, eleven times — and `extended` would have read better. Two spellings of
#: one term is exactly the drift a controlled vocabulary exists to stop, and
#: the incumbent wins because renaming it would strand eleven assertions with
#: no retraction to fix them. The lesson is the ordinary one: check what is
#: already asserted before naming anything.

#: **`(Original Mix)` is a term, and the argument that it is not was wrong.**
#:
#: It appears in 361 titles here, and a first version of this table dropped it
#: on the reasoning that it is the default — every recording is an original
#: until something says otherwise, so asserting it on the third that spell it
#: out invents a distinction that is not there.
#:
#: That confuses *what is usually true* with *what has been said*, which is the
#: one distinction this library is built on. `library-api.md` states it: **a
#: claim is not a value.** No version term on a recording means nobody has
#: said — not that it is the original. So `original` is the only way to say
#: positively that this *is* the original, and silence cannot carry that.
#:
#: It is load-bearing already. `identify.original_among` finds the recording a
#: remix was made out of by taking "the candidates carrying no version
#: qualifier at all, **or the ones explicitly marked `(original mix)` where any
#: say so**" — the derivation edges this library holds were found by reading
#: exactly this bracket. And on a dance single it is a real contrast printed on
#: the record: `Track (Original Mix)` sits beside `Track (Someone's Remix)`,
#: and which is which is the whole question a remix credit answers.

#: The bracket, exactly as it appears, to the term or terms it means.
#:
#: **Exact and whole**, for the reason in the module doc. A compound maps to
#: several: `rehearsal, mono` is two facts about one recording and collapsing
#: it to either one loses the other.
SPELLINGS: Final[dict[str, tuple[str, ...]]] = {
    # ── the one it was made from ──────────────────────────────────────────
    "original mix": ("original",),
    "original": ("original",),
    "original version": ("original",),
    "original club mix": ("original", "club-mix"),
    "original radio mix": ("original", "radio-edit"),
    "extended original mix": ("original", "extended-mix"),
    # ── played in front of people ─────────────────────────────────────────
    "live": ("live",),
    "live acoustic": ("live", "acoustic"),
    "live and acoustic": ("live", "acoustic"),
    "live at the ryman": ("live",),
    "live at cmt artists of the year awards": ("live",),
    "first rooftop performance": ("live",),
    "john peel sessions": ("live",),
    # ── how it was played ─────────────────────────────────────────────────
    "acoustic": ("acoustic",),
    "acoustic version": ("acoustic",),
    "instrumental": ("instrumental",),
    "instrumental mix": ("instrumental",),
    "instrumental version": ("instrumental",),
    "gypsy instrumental": ("instrumental",),
    # ── not the finished record ───────────────────────────────────────────
    "demo": ("demo",),
    "demo/alternate version": ("demo", "alternate-take"),
    "rehearsal": ("rehearsal",),
    "rehearsal, mono": ("rehearsal", "mono"),
    "sketches version": ("demo",),
    "alternate take": ("alternate-take",),
    "alternate version": ("alternate-take",),
    "alternate mix": ("alternate-mix",),
    "alternative mix": ("alternate-mix",),
    "overdub": ("overdub",),
    "undub": ("undub",),
    # ── cut for a purpose ─────────────────────────────────────────────────
    "radio edit": ("radio-edit",),
    "radio mix": ("radio-edit",),
    "single edit": ("single-edit",),
    "single version": ("single-edit",),
    "album edit": ("album-edit",),
    "album version": ("album-edit",),
    "lp version": ("album-edit",),
    "edit": ("radio-edit",),
    "extended mix": ("extended-mix",),
    "extended remix": ("extended-mix",),
    "club mix": ("club-mix",),
    "vocal mix": ("vocal-mix",),
    "full vocal mix": ("vocal-mix",),
    "dub": ("dub",),
    "dub mix": ("dub",),
    "deep mix": ("club-mix",),
    # ── how it was mastered ───────────────────────────────────────────────
    "mono": ("mono",),
    "2021 mix": ("remaster",),
    "1969 glyn johns mix": ("remaster",),
    "1970 glyn johns mix": ("remaster",),
    "single version, 2021 mix": ("single-edit", "remaster"),
    # ── a piece of something ──────────────────────────────────────────────
    "reprise": ("reprise",),
    "excerpt": ("excerpt",),
    "medley": ("medley",),
}

#: A bracket naming a person who reworked it. **Not this pass's business** —
#: a remixer is a credit and `joined remixed` settles it (#170, #145). Matched
#: only to keep them out of the review file, where they would be noise.
A_REMIXER: Final = re.compile(r"\b(?:re-?mix|rework|refix|bootleg|vip|mixed by|remixed by)\b", re.IGNORECASE)

#: Brackets that name what a *track* is rather than which version of a work.
#: A different axis — `facet::FUNCTION` if anywhere — and guessing across axes
#: is how a taxonomy stops being trustworthy.
ANOTHER_AXIS: Final[frozenset[str]] = frozenset(
    {"intro", "outro", "interlude", "overture", "opening intro", "outrocut", "bonus track", "untitled", "skit"}
)

#: A numbered take — `take 4`, `take 19`. The one form that cannot be
#: enumerated, so it is the one pattern here, and it is anchored end to end:
#: `take` alone is an ordinary word (`Take It All`, `to Take My Man`) and only
#: the number makes it a claim about which attempt you are holding.
A_NUMBERED_TAKE: Final = re.compile(r"^takes? \d+$")

BRACKETED: Final = re.compile(r"\(([^)]*)\)|\[([^\]]*)\]")


def normalise(bracket: str) -> str:
    """The form a lookup uses: case and spacing folded, accents kept.

    Matches `joined.key`'s shallowness on purpose — this is a *comparison* and
    nothing is minted from it, so folding here cannot create the unrepairable
    merge the Rust side is careful about. Curly quotes fold to straight because
    a catalogue and a tagger disagree about them constantly and neither means
    anything by it.
    """
    s = u.normalize("NFKC", bracket).strip().lower()

    # marks a catalogue and a tagger disagree about, folded here so a lookup
    # does not care which one somebody typed.
    for typeset, plain in (("\u2019", "'"), ("\u2018", "'"), ("\u2010", "-"), ("\u2013", "-")):
        s = s.replace(typeset, plain)
    return re.sub(r"\s+", " ", s)


def brackets_in(title: str) -> list[str]:
    """Every bracketed run in a title, in order."""
    return [(m.group(1) or m.group(2)).strip() for m in BRACKETED.finditer(title) if (m.group(1) or m.group(2)).strip()]


def read(bracket: str) -> tuple[str, tuple[str, ...]]:
    """What one bracket is, and which terms it means.

    Returns a verdict and the terms. Only `version` carries any, and only from
    `SPELLINGS` — everything else is a form somebody has to look at, which is
    the point.
    """
    form = normalise(bracket)
    if form in SPELLINGS:
        return "version", SPELLINGS[form]
    if A_NUMBERED_TAKE.match(form):
        return "version", ("alternate-take",)
    if form in ANOTHER_AXIS:
        return "another axis — what the track is, not which version", ()
    if A_REMIXER.search(form):
        return "a remixer — a credit, not a version", ()
    return "unrecognised — a title until somebody says otherwise", ()


def survey(rows: Iterable[dict[str, str]]) -> tuple[list[dict[str, object]], list[dict[str, str]]]:
    """Read every title, and propose only what the vocabulary knows.

    **Nothing is written for an unrecognised form.** It goes to the review file
    with its count, which is how the vocabulary grows: a person reads the tail
    and either adds a spelling or confirms it is a title.
    """
    lines: list[dict[str, object]] = []
    unknown: collections.Counter[str] = collections.Counter()
    tally: collections.Counter[str] = collections.Counter()
    for row in rows:
        recording, title = row.get("recording"), row.get("title") or ""
        for bracket in brackets_in(title):
            verdict, terms = read(bracket)
            tally[verdict] += 1
            if verdict == "unrecognised — a title until somebody says otherwise":
                unknown[normalise(bracket)] += 1
            if not (terms and recording):
                continue
            lines += [
                {
                    "fact": "qualifier_assigned",
                    "recording": recording,
                    "qualifier": term,
                    # A reading of a title this library already holds, which is
                    # what `inferred` means. Not `tag_variant`: the tag is
                    # where the string came from, and the *term* is ours.
                    "basis": "inferred",
                    "confidence": "provisional",
                }
                for term in terms
            ]
    for verdict, n in tally.most_common():
        print(f"{n:5}  {verdict}", file=sys.stderr)
    review = [{"form": f, "seen": str(n)} for f, n in unknown.most_common()]
    return lines, review


def run(work: Path, out: Path, review: Path) -> None:
    """Read the work list, propose the versions, report the rest."""
    rows = [json.loads(line) for line in work.read_text().splitlines() if line.strip()]
    lines, unknown = survey(rows)
    out.parent.mkdir(parents=True, exist_ok=True)
    with out.open("w") as fh:
        for line in lines:
            fh.write(json.dumps(line) + "\n")
    write_tsv(review, unknown, ["form", "seen"])
    print(f"\n{len(lines)} version fact(s) -> {out}", file=sys.stderr)
    print(f"{len(unknown)} form(s) the vocabulary does not know -> {review}", file=sys.stderr)
    print("Nothing was written for them: a bracket is a title until a curated term says otherwise.", file=sys.stderr)


WORK = typer.Argument(Path("work.jsonl"), help="the work list from `gmuse shelf query`")


def register(app: typer.Typer) -> None:
    """Attach this pass to the CLI."""
    sub = typer.Typer(help=__doc__, no_args_is_help=True)

    @sub.command("propose")
    def propose_cmd(work: Path = WORK) -> None:
        """Assert which version a recording is, for the terms the vocabulary knows."""
        run(work.expanduser(), out_path("version-propose.jsonl"), out_path("version-unknown.tsv"))

    app.add_typer(sub, name="version")
