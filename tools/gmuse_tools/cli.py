"""One command, grouped by what a pass is for.

Twelve entry points, each with its own `argparse` and its own idea of how to be
invoked — `python -m genre.propose ~/MusicLibrary` beside `python quality.py
survey`. Grouped here instead:

```
gmuse-tools identify fetch|propose      what a recording is
gmuse-tools quality  survey|propose     what the bytes are
gmuse-tools genre    fetch|propose|apply|snapshot
gmuse-tools cache    stats|prune        what an authority said
gmuse-tools audit    library|dj         what is wrong
gmuse-tools tags     normalise
gmuse-tools ingest   run
```

## The shape every pass shares

**Read something, say what would happen, and do it only when told.** That is
`gmuse library apply`'s shape too, and it is not a coincidence: these produce
the files it consumes. A pass that writes by default is a pass nobody reads the
output of first, which matters because these are wrong often enough to be worth
reviewing — three of 607 confident identifications this week were a different
version of the right song.

## Why the groups are nouns

`identify` and `quality` are the two halves of "what is this file", and they
answer about different subjects: a recording and an encoding. Keeping them
apart in the command tree keeps that distinction visible at the place someone
types, which is where it is most often forgotten.
"""

from __future__ import annotations

import typer

from gmuse_tools import audit_dj, audit_library, bronze, identify, ingest, quality, tags

# `credits` shadows a builtin, which ruff is right to flag — the module is named
# for what it holds and the alias keeps both true.
from gmuse_tools import credits as credits_pass
from gmuse_tools.genre import apply as genre_apply
from gmuse_tools.genre import fetch as genre_fetch
from gmuse_tools.genre import propose as genre_propose
from gmuse_tools.genre import snapshot as genre_snapshot

app = typer.Typer(
    name="gmuse-tools",
    help=__doc__,
    no_args_is_help=True,
    add_completion=False,
    rich_markup_mode=None,
)

identify_app = typer.Typer(no_args_is_help=True, help=identify.__doc__)
quality_app = typer.Typer(no_args_is_help=True, help=quality.__doc__)
genre_app = typer.Typer(no_args_is_help=True, help="Normalise genre tags — broad term first.")
cache_app = typer.Typer(no_args_is_help=True, help=bronze.__doc__)
audit_app = typer.Typer(no_args_is_help=True, help="Report what is wrong, and change nothing.")

app.add_typer(identify_app, name="identify")
app.add_typer(quality_app, name="quality")
app.add_typer(genre_app, name="genre")
app.add_typer(cache_app, name="cache")
app.add_typer(audit_app, name="audit")

identify.register(identify_app)
quality.register(quality_app)
bronze.register(cache_app)
credits_pass.register(app)
tags.register(app)
ingest.register(app)
audit_library.register(audit_app)
audit_dj.register(audit_app)
genre_fetch.register(genre_app)
genre_propose.register(genre_app)
genre_apply.register(genre_app)
genre_snapshot.register(genre_app)


if __name__ == "__main__":
    app()
