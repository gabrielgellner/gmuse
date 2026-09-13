#!/usr/bin/env python3
"""Assemble the mdBook from the design documents — #247.

**The book has no second copy of anything.** `docs/design/` is the authority, and this
script derives the book's `src/` from it: chapter files, and a `SUMMARY.md` built from
the documents' own headings. Nothing is written twice, so nothing can disagree.

That is the #252 lesson applied to structure rather than prose. Three alternatives were
considered and rejected:

- **`{{#include}}` stubs.** mdBook resolves an include outside `src/` happily, but a
  *broken* one is a `WARN` with **exit 0** and the literal `{{#include …}}` text lands
  in the published page. A renamed document would silently produce a book with a hole.
- **`{{#include file:12:340}}` line ranges.** Any edit above line 12 shifts them, and
  the failure is worse than silent — it is wrong content with no warning at all.
- **`<!-- ANCHOR -->` comments in the sources.** Stable and checkable, at the cost of
  two dozen HTML comments interleaved through documents people read raw in the
  repository.

Deriving the split from `##` headings means renaming a section renames its chapter
rather than breaking one, and a document that gains a section gains a chapter.

Run through `just docs`.
"""

from __future__ import annotations

import re
import shutil
import sys
from dataclasses import dataclass, field
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
DESIGN = ROOT / "docs" / "design"
BOOK = ROOT / "docs" / "book"
FRONT = BOOK / "front"
SRC = BOOK / "src"


@dataclass
class Doc:
    """A design document, and how it enters the book."""

    file: str
    title: str
    #: One line under the title saying what a reader is looking at. The status
    #: markers matter more than anything else here: several of these documents are
    #: proposals, and a book that published them as description would be lying.
    standing: str
    #: Split on `##` headings into one page each. For the long ones only — a short
    #: document reads better whole than as four stubs.
    split: bool = False


@dataclass
class Part:
    title: str
    intro: str
    docs: list[Doc] = field(default_factory=list)


PARTS = [
    Part(
        title="The Information Model",
        intro="information-model.md",
        docs=[
            Doc(
                file="domain-model.md",
                title="The domain model",
                standing="**Proposal, not built.** What a library entry is, as against what a file is. "
                "Read it before touching anything keyed by a path.",
                split=True,
            ),
            Doc(
                file="domain-model-vocabularies.md",
                title="Where a name comes from",
                standing="**Settled.** Which vocabulary a term is borrowed from, and how a borrowed "
                "distinction is spelled.",
            ),
            Doc(
                file="library-api.md",
                title="The named operations",
                standing="**The contract, and built.** Import, relink and forget are named operations; "
                "a scan reconciles holdings and reads no tags.",
            ),
        ],
    ),
    Part(
        title="The Layers",
        intro="layers.md",
        docs=[
            Doc(
                file="layers.md",
                title="What may talk to what",
                standing="**Built.** The librarian, the player, the scanner and the tools — and the "
                "direction rule, which is now a compile error rather than a convention.",
            ),
            Doc(
                file="access-layers.md",
                title="Inside the librarian",
                standing="**Proposal, partly built.** Which layer inside the librarian may talk to "
                "which. The rule it turns on is that a read never touches the filesystem.",
                split=True,
            ),
            Doc(
                file="storage-layout.md",
                title="Where a file lives",
                standing="**Settled design, not built.** One rule: the storage tree may use "
                "structural facts only.",
            ),
        ],
    ),
    Part(
        title="Challenges to the Model",
        intro="challenges.md",
        docs=[
            Doc(
                file="domain-model-review.md",
                title="A review of the domain model",
                standing="**A critique, kept.** What a reading of the model found wrong with it.",
            ),
            Doc(
                file="domain-model-second-opinion.md",
                title="A second opinion",
                standing="**A critique, kept.** An independent reading, which disagrees with the "
                "first in places.",
            ),
        ],
    ),
]

#: Design documents deliberately not in this book, and why.
#:
#: **An explicit list, so a new document cannot go unpublished by being forgotten.**
#: The check at the bottom fails when `docs/design/` holds something that is neither
#: published nor listed here — which is the same shape as every other claim in this
#: repository that a machine can hold.
NOT_IN_THIS_BOOK = {
    "music-player-tui.md": "the project brief and its Decision Log; a book of its own",
    "gmuse-engine-architecture.md": "the engine's architecture; belongs with the brief",
    "identification-lessons.md": "a working document about this library's metadata",
    "ingestion-flow.md": "how a download becomes a row today; operational",
    "info-panel.md": "a UI proposal, for the interaction-model book",
    "library-sync-design.md": "how logs merge; follows the information model",
    "query-language.md": "the query language; for the interaction-model book",
    "transport-keymap-spec.md": "the transport vocabulary; interaction model",
    "tree-navigation-design.md": "navigation; interaction model",
    "tree-view-design.md": "the tree view; interaction model",
    "vision-and-future.md": "directions, not design",
    "domain-model-review.md": "published in Challenges",
    "domain-model-second-opinion.md": "published in Challenges",
}


def slug(text: str) -> str:
    """A filename from a heading, stable under rewording of punctuation."""
    out = re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")
    return out or "section"


def sections(body: str) -> list[tuple[str, str]]:
    """Split on `##` headings, keeping anything before the first one as a preamble.

    Fenced code is skipped, because a `## ` inside a shell block is not a heading and
    splitting on one would cut a chapter in half mid-example.
    """
    lines = body.splitlines()
    fenced = False
    cuts: list[tuple[int, str]] = []
    for n, line in enumerate(lines):
        if line.startswith("```"):
            fenced = not fenced
            continue
        if not fenced and line.startswith("## "):
            cuts.append((n, line[3:].strip()))

    if not cuts:
        return [("", body)]

    out: list[tuple[str, str]] = []
    head = "\n".join(lines[: cuts[0][0]]).strip()
    if head:
        out.append(("", head))
    for i, (at, title) in enumerate(cuts):
        end = cuts[i + 1][0] if i + 1 < len(cuts) else len(lines)
        out.append((title, "\n".join(lines[at:end]).rstrip()))
    return out


def strip_title(body: str) -> str:
    """Drop a document's own `# Title`, which the chapter heading replaces."""
    lines = body.splitlines()
    for n, line in enumerate(lines):
        if line.startswith("# "):
            return "\n".join(lines[n + 1 :]).lstrip("\n")
        if line.strip():
            break
    return body


def main() -> int:
    if not DESIGN.is_dir():
        print(f"no {DESIGN}", file=sys.stderr)
        return 1

    if SRC.exists():
        shutil.rmtree(SRC)
    SRC.mkdir(parents=True)

    # The introduction is written for the book and lives in `front/`.
    intro = FRONT / "introduction.md"
    if not intro.is_file():
        print(f"missing {intro}", file=sys.stderr)
        return 1
    (SRC / "introduction.md").write_text(intro.read_text())

    summary = ["# Summary", "", "[Introduction](./introduction.md)", ""]
    published: set[str] = set()

    for part in PARTS:
        summary.append(f"# {part.title}")
        summary.append("")

        preface = FRONT / part.intro
        if not preface.is_file():
            print(f"missing {preface}", file=sys.stderr)
            return 1
        name = f"part-{slug(part.title)}.md"
        (SRC / name).write_text(preface.read_text())
        summary.append(f"- [About this part](./{name})")

        for doc in part.docs:
            source = DESIGN / doc.file
            if not source.is_file():
                print(
                    f"{doc.file} is in the book and not on disk — it moved or was "
                    f"renamed, and a book that quietly loses a chapter is the thing "
                    f"this script exists to avoid",
                    file=sys.stderr,
                )
                return 1
            published.add(doc.file)
            body = strip_title(source.read_text())
            stem = slug(doc.title)

            if not doc.split:
                page = f"{stem}.md"
                (SRC / page).write_text(
                    f"# {doc.title}\n\n> {doc.standing}\n>\n"
                    f"> Source: [`docs/design/{doc.file}`](https://gitlab.com/"
                    f"gabrielgellner/gmuse/-/blob/main/docs/design/{doc.file})\n\n{body}\n"
                )
                summary.append(f"- [{doc.title}](./{page})")
                continue

            # Split: the document's own `##` headings become its chapters.
            parts = sections(body)
            landing = f"{stem}.md"
            preamble = parts[0][1] if parts and parts[0][0] == "" else ""
            (SRC / landing).write_text(
                f"# {doc.title}\n\n> {doc.standing}\n>\n"
                f"> Source: [`docs/design/{doc.file}`](https://gitlab.com/"
                f"gabrielgellner/gmuse/-/blob/main/docs/design/{doc.file})\n\n{preamble}\n"
            )
            summary.append(f"- [{doc.title}](./{landing})")
            for title, text in parts:
                if not title:
                    continue
                page = f"{stem}-{slug(title)}.md"
                (SRC / page).write_text(f"{text}\n")
                summary.append(f"  - [{title}](./{page})")

        summary.append("")

    # **Nothing in `docs/design/` goes unpublished by being forgotten.**
    on_disk = {p.name for p in DESIGN.glob("*.md")}
    unaccounted = sorted(on_disk - published - set(NOT_IN_THIS_BOOK))
    if unaccounted:
        print(
            "these design documents are neither in the book nor listed as excluded:\n  "
            + "\n  ".join(unaccounted)
            + "\n\nAdd each to PARTS or to NOT_IN_THIS_BOOK with a reason. A document "
            "that is silently unpublished is the failure this list prevents.",
            file=sys.stderr,
        )
        return 1

    stale = sorted(set(NOT_IN_THIS_BOOK) - on_disk - published)
    if stale:
        print(
            "these are listed as excluded and no longer exist:\n  " + "\n  ".join(stale),
            file=sys.stderr,
        )
        return 1

    (SRC / "SUMMARY.md").write_text("\n".join(summary).rstrip() + "\n")
    pages = len(list(SRC.glob("*.md"))) - 1
    print(f"book: {pages} pages from {len(published)} design documents")
    return 0


if __name__ == "__main__":
    sys.exit(main())
