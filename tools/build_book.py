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
GUIDES = ROOT / "docs" / "guides"
BOOKS_DIR = ROOT / "docs" / "books"


@dataclass
class Doc:
    """A document, and how it enters a book."""

    file: str
    title: str
    #: One line under the title saying what a reader is looking at. The status
    #: markers matter more than anything else here: several of these documents are
    #: proposals, and a book that published them as description would be lying.
    standing: str
    #: Split on `##` headings into one page each. For the long ones only — a short
    #: document reads better whole than as four stubs.
    split: bool = False
    #: Which directory it comes from. The guides are **not** copied here: they are
    #: `include_str!`'d into the binary as view 7, so the file this reads is the file
    #: the program ships. One source, two outputs — which was the argument for mdBook
    #: fitting this repository at all (#247).
    source: str = "design"


@dataclass
class Part:
    title: str
    intro: str
    docs: list[Doc] = field(default_factory=list)


@dataclass
class Book:
    """One book, published at `/{key}/` on the Pages site."""

    key: str
    title: str
    blurb: str
    parts: list[Part] = field(default_factory=list)


MODEL = [
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
                file="playback-programme.md",
                title="Who decides what plays next",
                standing="**Settled design, not built.** The playback half of #257 — the programme, "
                "the window, and why every repeat mode is one function the librarian answers.",
                split=True,
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

INTERACTION = [
    Part(
        title="Using It",
        intro="using-it.md",
        docs=[
            Doc(
                file="moving-around.md",
                title="Moving around",
                standing="**A guide, and the program ships it.** This is view 7, verbatim.",
                source="guides",
            ),
            Doc(
                file="the-library-tree.md",
                title="The library tree",
                standing="**A guide.** One pane holding artists, their albums, and the tracks of "
                "whichever albums you have open.",
                source="guides",
            ),
            Doc(
                file="finding-things.md",
                title="Finding things",
                standing="**A guide.** Search, filters, and the query language from the using end.",
                source="guides",
            ),
            Doc(
                file="building-playlists.md",
                title="Building playlists",
                standing="**A guide.** The vim gestures — yank, put, delete, undo — applied to a "
                "list of tracks.",
                source="guides",
            ),
            Doc(
                file="saved-rules.md",
                title="Rules that make lists",
                standing="**A guide.** A named, persistent filter, which is a playlist that keeps "
                "itself current.",
                source="guides",
            ),
            Doc(
                file="what-the-library-says.md",
                title="What the library says",
                standing="**A guide.** Reading and editing what the library holds about a track.",
                source="guides",
            ),
            Doc(
                file="format-strings.md",
                title="Writing the lines",
                standing="**A guide.** The format language, which the config, the `:` line and "
                "`playback.get` all share.",
                source="guides",
            ),
        ],
    ),
    Part(
        title="Why It Is Shaped This Way",
        intro="why-shaped.md",
        docs=[
            Doc(
                file="tree-view-design.md",
                title="The tree view",
                standing="**Design.** Why one pane holds three levels, and why that reads as a "
                "single buffer.",
                split=True,
            ),
            Doc(
                file="tree-navigation-design.md",
                title="Navigating it",
                standing="**Design.** Motions that use the shape rather than counting rows.",
                split=True,
            ),
            Doc(
                file="tree-over-the-wire.md",
                title="The tree over the wire",
                standing="**Settled design, not built.** What a view holds once the library is the "
                "librarian's: a screenful, a cursor, and four bits a row.",
                split=True,
            ),
            Doc(
                file="query-language.md",
                title="The query language",
                standing="**Design.** One language for the `:` line, the config, saved rules and "
                "the socket.",
                split=True,
            ),
            Doc(
                file="transport-keymap-spec.md",
                title="The transport vocabulary",
                standing="**Proposal, not built.** The DJ-derived verbs the player's four cannot "
                "express. Issues #225–#229.",
                split=True,
            ),
            Doc(
                file="view-model.md",
                title="What a view is",
                standing="**A review of a proposal.** Mostly a record of decisions already made: "
                "what #97 removed, why the digits are counts, and what is actually left to build.",
                split=True,
            ),
            Doc(
                file="info-panel.md",
                title="The info panel",
                standing="**Proposal, not built.** The surface that shows what the library says "
                "about one thing, and who said it. The rule it turns on is that a claim is never "
                "a value.",
                split=True,
            ),
        ],
    ),
]

BOOKS = [
    Book(
        key="model",
        title="gmuse — the information model and the layers",
        blurb="What the library is, and which part of the program may talk to which.",
        parts=MODEL,
    ),
    Book(
        key="interaction",
        title="gmuse — the interaction model",
        blurb="How the player is driven: the tree, the motions, the query language, "
        "the transport vocabulary.",
        parts=INTERACTION,
    ),
]


#: Design documents deliberately in no book, and why.
#:
#: **An explicit list, so a new document cannot go unpublished by being forgotten.**
#: The check at the bottom fails when `docs/design/` or `docs/guides/` holds something
#: that is neither published in some book nor listed here — which is the same shape as
#: every other claim in this repository that a machine can hold.
NOT_IN_ANY_BOOK = {
    "music-player-tui.md": "the project brief and its Decision Log; a book of its own",
    "gmuse-engine-architecture.md": "the engine's architecture; belongs with the brief",
    "identification-lessons.md": "a working document about this library's metadata",
    "ingestion-flow.md": "how a download becomes a row today; operational",
    "library-sync-design.md": "how logs merge; follows the information model",
    "vision-and-future.md": "directions, not design",
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


def one(book: Book, published: set[str]) -> int:
    """Generate one book's `src/`. Returns 0, or 1 having said what went wrong."""
    where = BOOKS_DIR / book.key
    front = where / "front"
    src = where / "src"
    if src.exists():
        shutil.rmtree(src)
    src.mkdir(parents=True)

    intro = front / "introduction.md"
    if not intro.is_file():
        print(f"missing {intro}", file=sys.stderr)
        return 1
    (src / "introduction.md").write_text(intro.read_text())

    summary = ["# Summary", "", "[Introduction](./introduction.md)", ""]

    for part in book.parts:
        summary.append(f"# {part.title}")
        summary.append("")

        preface = front / part.intro
        if not preface.is_file():
            print(f"missing {preface}", file=sys.stderr)
            return 1
        name = f"part-{slug(part.title)}.md"
        (src / name).write_text(preface.read_text())
        summary.append(f"- [About this part](./{name})")

        for doc in part.docs:
            home = GUIDES if doc.source == "guides" else DESIGN
            source = home / doc.file
            if not source.is_file():
                print(
                    f"{doc.file} is in the {book.key} book and not at {source} — it "
                    f"moved or was renamed, and a book that quietly loses a chapter is "
                    f"the thing this script exists to avoid",
                    file=sys.stderr,
                )
                return 1
            published.add(doc.file)
            body = strip_title(source.read_text())
            stem = slug(doc.title)
            link = (
                f"https://github.com/gabrielgellner/gmuse/blob/main/docs/"
                f"{'guides' if doc.source == 'guides' else 'design'}/{doc.file}"
            )
            header = f"> {doc.standing}\n>\n> Source: [`docs/{'guides' if doc.source == 'guides' else 'design'}/{doc.file}`]({link})"

            if not doc.split:
                page = f"{stem}.md"
                (src / page).write_text(f"# {doc.title}\n\n{header}\n\n{body}\n")
                summary.append(f"- [{doc.title}](./{page})")
                continue

            parts = sections(body)
            landing = f"{stem}.md"
            preamble = parts[0][1] if parts and parts[0][0] == "" else ""
            # **The landing page lists what follows.** Several documents open straight
            # into their first heading, which left the page above them a stub — a title,
            # a status marker, and nothing to read or click. The contents are derived
            # from the same split, so they cannot disagree with the sidebar.
            contents = "\n".join(
                f"- [{title}](./{stem}-{slug(title)}.md)" for title, _ in parts if title
            )
            (src / landing).write_text(
                f"# {doc.title}\n\n{header}\n\n{preamble}\n\n## In this chapter\n\n{contents}\n"
            )
            summary.append(f"- [{doc.title}](./{landing})")
            for title, text in parts:
                if not title:
                    continue
                page = f"{stem}-{slug(title)}.md"
                (src / page).write_text(f"{text}\n")
                summary.append(f"  - [{title}](./{page})")

        summary.append("")

    (src / "SUMMARY.md").write_text("\n".join(summary).rstrip() + "\n")
    pages = len(list(src.glob("*.md"))) - 1
    print(f"  {book.key}: {pages} pages")
    return 0


def main() -> int:
    if not DESIGN.is_dir() or not GUIDES.is_dir():
        print("no docs/design or docs/guides", file=sys.stderr)
        return 1

    published: set[str] = set()
    for book in BOOKS:
        if one(book, published) != 0:
            return 1

    # **Nothing goes unpublished by being forgotten.**
    on_disk = {p.name for p in DESIGN.glob("*.md")} | {p.name for p in GUIDES.glob("*.md")}
    unaccounted = sorted(on_disk - published - set(NOT_IN_ANY_BOOK))
    if unaccounted:
        print(
            "these documents are in no book and not listed as excluded:\n  "
            + "\n  ".join(unaccounted)
            + "\n\nAdd each to a book or to NOT_IN_ANY_BOOK with a reason. A document "
            "that is silently unpublished is the failure this list prevents.",
            file=sys.stderr,
        )
        return 1

    stale = sorted(set(NOT_IN_ANY_BOOK) - on_disk)
    if stale:
        print(
            "these are listed as excluded and no longer exist:\n  " + "\n  ".join(stale),
            file=sys.stderr,
        )
        return 1

    print(f"book: {len(BOOKS)} books from {len(published)} documents")
    return 0

if __name__ == "__main__":
    sys.exit(main())
