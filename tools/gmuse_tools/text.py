"""Folding a name to what two spellings of it can be expected to share.

**One implementation of the fold, two alphabet policies on top of it** — #296.
The fold is the load-bearing half and `identification-lessons.md` §19 states it
in one line: *fold to ASCII through NFKD; do not strip.* What a caller does
after the fold is its own question, and the two callers here answer it
differently on purpose:

- `identify.folded` keeps `[a-z0-9]`, because it compares a credit against
  MusicBrainz's spelling of a Latin-script artist name and wants a tight key.
- `genre.common.compare_key` keeps anything `str.isalnum()` accepts, because it
  compares whole tracklists and a title in a non-Latin script must not fold to
  the empty string — every such title would then key alike and a set
  intersection would call them all the same track.

Measured 2026-09-30: no title in the library (7,820) or the staging tree
(~10,000) is entirely non-Latin today, so that second case is a hazard rather
than a bug. It is guarded anyway because it fails **silently and wrongly** —
`tracklist_match` would report agreement — and because the library's scope is
meant to grow.
"""

from __future__ import annotations

import unicodedata


def fold_accents(text: str) -> str:
    """Decompose and drop the combining marks, leaving the letters.

    **Folded away, not stripped.** `Lvis Mejía` reaches this library as
    `Lvis Meji` + U+00B4 + `a`, because some tagger long ago turned a combining
    acute into a literal one. A normaliser that deletes every non-ASCII
    character maps those two spellings to `lvismejia` and `lvismeja`, which do
    not match — and the first version of §19's name check refused both of that
    artist's tracks for exactly that reason, a correct match thrown away by the
    guard meant to protect it.

    NFKD rather than NFD, so a compatibility form folds too: `ﬁ` becomes `fi`
    and a full-width digit becomes an ASCII one.
    """
    decomposed = unicodedata.normalize("NFKD", text or "")
    return "".join(c for c in decomposed if not unicodedata.combining(c))
