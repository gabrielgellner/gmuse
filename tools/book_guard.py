#!/usr/bin/env python3
"""Check the built book, because mdBook's exit code proves very little — #247.

`mdbook build` reports a broken `{{#include}}` as a **WARN and exits 0**, leaving the
literal include text in the published page. It will also cheerfully write a book with a
chapter that rendered to nothing. So the artifact is inspected rather than trusted.

Three checks:

1. **Enough pages.** A book that lost most of itself.
2. **No unresolved include.** Belt and braces — `tools/build_book.py` uses no includes
   at all, precisely so this cannot happen, and this catches anyone who adds one.
3. **No empty chapter.** A chapter whose document went missing renders as navigation
   with nothing inside it.

**Check 3 is measured inside `<main>`, and that is not a detail.** The first version
measured the whole page's text against a 2,000-character floor — under which nothing
could ever fall, because mdBook's navigation chrome alone renders 2,131 characters on
every page. It was vacuous: it could not fail. Measuring chapter content separates the
numbers cleanly — mdBook's own `404.html` holds 105 characters and the shortest real
chapter holds 466.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

#: Pages mdBook generates that are meant to be nearly empty.
#:
#: Excluded by name rather than by lowering the threshold, which would have hidden a
#: real chapter losing its document — the same trade the floor above got wrong once.
NOT_CHAPTERS = {"toc.html", "404.html", "print.html"}

#: Below this many characters inside `<main>`, a chapter has lost its document.
#:
#: **200, and the number moved once for a reason worth keeping.** It was 300, chosen
#: against the model book where the shortest real chapter held 466. The interaction book
#: has a genuinely shorter one — `query-language.md`'s closing "Related" section, which
#: is four links and 286 characters, and is not a defect. mdBook's own `404.html` holds
#: 105, so 200 still clears the noise by a factor of two.
FLOOR = 200

#: Fewer pages than this and the book lost most of itself.
LEAST = 20

#: Diagrams the book is expected to render through `mdbook-mermaid`.
#:
#: **A count, because the failure is silent otherwise.** The preprocessor is a separate
#: binary and its assets are generated rather than committed, so running
#: `mdbook build` without `mdbook-mermaid install` leaves `additional-js` pointing at
#: files that are not there — mdBook warns, exits 0, and publishes pages whose diagrams
#: are inert. Asserting the number rendered is the only thing that notices.
#:
#: Four: the two class diagrams in the domain model, which predate the preprocessor and
#: were inert here until it arrived, plus the layer diagrams in `layers.md` and
#: `access-layers.md`. Raise it when a diagram is added — that is the point of a count
#: rather than a "more than zero".
#: Keyed by book, because only the model book has diagrams today.
DIAGRAMS = {"model": 4, "interaction": 0}


def content(page: Path) -> int | None:
    """Characters of text inside `<main>`, or `None` if the page has no `<main>`."""
    found = re.search(r"<main>(.*?)</main>", page.read_text(), re.S)
    if not found:
        return None
    return len(" ".join(re.sub(r"<[^>]+>", " ", found.group(1)).split()))


def main(argv: list[str]) -> int:
    out = Path(argv[1] if len(argv) > 1 else "docs/books/model/book")
    # `docs/books/<key>/book` — the key is what `DIAGRAMS` is keyed by.
    key = out.parent.name
    expected = DIAGRAMS.get(key, 0)
    if not out.is_dir():
        print(f"no book at {out} — run `just docs`", file=sys.stderr)
        return 1

    pages = sorted(out.glob("*.html"))
    if len(pages) < LEAST:
        print(
            f"only {len(pages)} pages in {out} — the book lost most of itself",
            file=sys.stderr,
        )
        return 1

    unresolved = [p.name for p in pages if "{{#include" in p.read_text()]
    if unresolved:
        print(
            "an unresolved include reached the published page — mdBook reports this as "
            "a warning and exits 0, so it would otherwise ship:\n  "
            + "\n  ".join(unresolved),
            file=sys.stderr,
        )
        return 1

    thin: list[str] = []
    for page in pages:
        if page.name in NOT_CHAPTERS:
            continue
        size = content(page)
        if size is None:
            thin.append(f"{page.name} (no <main> at all)")
        elif size < FLOOR:
            thin.append(f"{page.name} ({size} characters of content)")
    if thin:
        print(
            "these chapters rendered with almost nothing in them, which is what a "
            "chapter that lost its document looks like:\n  " + "\n  ".join(thin),
            file=sys.stderr,
        )
        return 1

    # Occurrences, not pages: the domain model's chapter carries two.
    drawn = sum(
        p.read_text().count('class="mermaid"')
        for p in pages
        if p.name != "print.html"
    )
    if drawn < expected:
        print(
            f"{key}: {drawn} mermaid diagram(s) rendered, expected {expected} — either a "
            f"diagram was removed and DIAGRAMS needs lowering, or `mdbook-mermaid "
            f"install docs/book` has not been run and the published diagrams are inert",
            file=sys.stderr,
        )
        return 1

    vendored = [p.name for p in out.glob("mermaid*.js")]
    if expected and not vendored:
        print(
            "no mermaid script in the output, so the diagrams above cannot draw",
            file=sys.stderr,
        )
        return 1

    print(f"  {key}: {len(pages)} pages, {drawn} diagrams — {out / 'index.html'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
