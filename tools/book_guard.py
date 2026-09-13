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
FLOOR = 300

#: Fewer pages than this and the book lost most of itself.
LEAST = 20


def content(page: Path) -> int | None:
    """Characters of text inside `<main>`, or `None` if the page has no `<main>`."""
    found = re.search(r"<main>(.*?)</main>", page.read_text(), re.S)
    if not found:
        return None
    return len(" ".join(re.sub(r"<[^>]+>", " ", found.group(1)).split()))


def main(argv: list[str]) -> int:
    out = Path(argv[1] if len(argv) > 1 else "docs/book/book")
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

    print(f"book: {len(pages)} pages checked at {out / 'index.html'}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
