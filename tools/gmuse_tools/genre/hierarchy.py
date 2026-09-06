"""The genre hierarchy: which term is narrower than which — issue #79.

This library's tag convention is compound and broad-term-first, `Electronic;
Techno`, and it exists for one reason: a view could not name a level of a
vocabulary, so the string had to carry the hierarchy instead. gmuse can express
it now — `:view levels genre@1,genre@2` and `genre*="Electronic"` — and this is
where the edges come from.

## One map, two consumers

`NARROWER` is the curation. `propose.IMPLIES` is derived from it, so the term a
tag string gets given and the term the model climbs to cannot drift apart. A
child may have **several** parents: `Bossa Nova` is Jazz and Latin, `Country
Folk` is both of those. The model takes every one, and a row under a two-parent
style appears under both broad terms at `genre@1` — genre is many-valued and
its counts do not sum, so that is the consistent answer rather than an
arbitrary pick.

**The first parent listed is the one the tag string gets**, because a `; `
value has room for one broad term and the order is the whole convention.

## Where the rest comes from

Discogs separates *genres* (fourteen broad terms) from *styles* (hundreds), so
a release naming exactly one genre teaches an edge for every style on it. That
covers what curation has not reached. A style whose evidence is thin, or that
is genuinely contested, goes to the review file instead of being written —
`Gospel` is Folk and Blues and Funk depending on the record, and guessing is
how a taxonomy stops being trustworthy.
"""

from __future__ import annotations

import collections
import json
import sqlite3
from pathlib import Path
from typing import Final

import typer

from gmuse_tools.genre.common import fix_case, out_path, tags_of, walk, write_tsv

#: The terms this library treats as broad — Discogs' 15 Genres, **after**
#: `RENAME`. Both spellings are in here on purpose: a source says
#: `Folk, World, & Country` and the library says `Folk`, and if only the
#: source's spelling were listed then a value already renamed would not be
#: recognised as broad. That bug put `Folk` in twice — once from `IMPLIES`,
#: once from the term itself — on every folk record in the library.
BROAD: Final[frozenset[str]] = frozenset(
    {
        "Electronic",
        "Rock",
        "Pop",
        "Jazz",
        "Blues",
        "Classical",
        "Hip Hop",
        "Funk / Soul",
        "Funk",
        "Folk, World, & Country",
        "Folk",
        "Reggae",
        "Latin",
        "Stage & Screen",
        "Non-Music",
        "Brass & Military",
        "Children's",
        # Not Discogs genres, but this library's own broad-first vocabulary uses
        # them and they must not be given a second broad term on top.
        "Country",
        "R&B",
        "Americana",
    }
)
#: This library's spellings, where they differ from Discogs'.
RENAME: Final[dict[str, str]] = {
    "Funk / Soul": "Funk",
    "Folk, World, & Country": "Folk",
    # A source term that is a category, not a genre.
    "General Country": "Country",
}

#: Terms that are not genres at all. `Other` says nothing, and an **artist name
#: in a genre field** is a tagger's mistake — dropped by rule rather than by
#: list, since the rule generalises and a list of artist names never would.
DROP: Final[frozenset[str]] = frozenset({"other", "unknown", "misc", "miscellaneous", "none"})
#: Child → the broad terms it is narrower than, **broadest-first for the tag
#: string**. Curated, and the reasons that are not obvious are written down.
NARROWER: Final[dict[str, tuple[str, ...]]] = {
    # ── rock ──────────────────────────────────────────────────────────────
    "Alternative Rock": ("Rock",),
    "Alternative": ("Rock",),
    "Hard Rock": ("Rock",),
    "Grunge": ("Rock",),
    # **Metal is a level, not a leaf.** Six terms in this library are a kind
    # of metal, and putting them all directly under Rock throws away the one
    # grouping somebody browsing would actually want. It also makes the
    # hierarchy three deep, which is what `genre@3` is for.
    "Metal": ("Rock",),
    "Thrash Metal": ("Metal",),
    "Thrash": ("Metal",),
    "Doom Metal": ("Metal",),
    "Sludge Metal": ("Metal",),
    "Heavy Metal": ("Metal",),
    "Death Metal": ("Metal",),
    # **No shortcut past a parent that already reaches the top.** `Metal` is
    # under `Rock`, so also naming `Rock` here makes `Stoner Rock` one hop from
    # the top — and `genre@1` then reports `Metal` as a broad term. A second
    # parent is for a genuinely second lineage, not for restating the first.
    "Stoner Rock": ("Metal",),
    "Indie": ("Rock",),
    "Indie Rock": ("Rock",),
    "Punk": ("Rock",),
    "Post-punk": ("Rock",),
    "Alternative Punk": ("Rock",),
    "Art Rock": ("Rock",),
    "Psychedelic Rock": ("Rock",),
    "Psychedelic": ("Rock",),
    "Experimental Rock": ("Rock",),
    "Post Rock": ("Rock",),
    "Post-rock": ("Rock",),
    "Britpop": ("Rock",),
    "Noise Pop": ("Rock",),
    "Noise Rock": ("Rock",),
    "Post-Hardcore": ("Rock",),
    "Industrial Metal": ("Metal",),
    "Rock And Roll": ("Rock",),
    "Rock & Roll": ("Rock",),
    "Glam": ("Rock",),
    "Garage Rock": ("Rock",),
    "Symphonic Rock": ("Rock",),
    "Southern Rock": ("Rock",),
    "Shoegaze": ("Rock",),
    "Arena Rock": ("Rock",),
    "Classic Rock": ("Rock",),
    "Soft Rock": ("Rock",),
    "Pop Rock": ("Rock", "Pop"),
    "Blues Rock": ("Rock", "Blues"),
    "New Wave": ("Rock",),
    "Acoustic Rock": ("Rock",),
    "Dance-rock": ("Rock",),
    "Dance-punk": ("Rock",),
    "Hardcore Punk": ("Rock",),
    "Garage Rock Revival": ("Rock",),
    "Post-punk Revival": ("Rock",),
    "Post-grunge": ("Rock",),
    "Neo-psychedelia": ("Rock",),
    "Piano Rock": ("Rock",),
    "Slacker Rock": ("Rock",),
    "Surf Rock": ("Rock",),
    "Jam Band": ("Rock",),
    "Merseybeat": ("Rock",),
    "Beat Music": ("Rock",),
    "Noise": ("Rock",),
    "Funk Rock": ("Rock", "Funk"),
    "Punk Blues": ("Rock", "Blues"),
    # ── folk ──────────────────────────────────────────────────────────────
    "Bluegrass": ("Folk",),
    "Anti-folk": ("Folk",),
    "Indie Folk": ("Folk",),
    "Neofolk": ("Folk",),
    "Alternative Folk": ("Folk",),
    "Contemporary Folk": ("Folk",),
    "Progressive Folk": ("Folk",),
    "Freak Folk": ("Folk",),
    "Celtic": ("Folk",),
    "Cajun": ("Folk",),
    "Old-time": ("Folk", "Country"),
    "Sea Shanties": ("Folk",),
    "Traditional Bluegrass": ("Folk",),
    "Progressive Bluegrass": ("Folk",),
    "Stomp And Holler": ("Folk",),
    "Singer-songwriter": ("Folk",),
    # `Folk Rock` is Folk first because that is what this library's tag
    # strings already say; Discogs files it under Rock 35 times to 2. Both
    # edges are true and the order decides only the tag, not the model.
    "Folk Rock": ("Folk", "Rock"),
    # ── country ───────────────────────────────────────────────────────────
    "Alternative Country": ("Country",),
    "Traditional Country": ("Country",),
    "Country Pop": ("Country", "Pop"),
    "Country Rock": ("Country", "Rock"),
    "Country Folk": ("Country", "Folk"),
    "Country Blues": ("Country", "Blues"),
    "Western Swing": ("Country", "Jazz"),
    "Contemporary Country": ("Country",),
    "Neo-traditional Country": ("Country",),
    "Progressive Country": ("Country",),
    "Honky Tonk": ("Country",),
    # ── pop ───────────────────────────────────────────────────────────────
    # New Age is deliberately **not** Electronic. Discogs files Enya there, but
    # much of it is acoustic — and her own self-titled record already reads
    # `Pop; Folk; New Age`, so Pop is what this library already says.
    "New Age": ("Pop",),
    "Christmas Music": ("Pop",),
    "French Pop": ("Pop",),
    "Ballad": ("Pop",),
    "Synth-pop": ("Pop", "Electronic"),
    "Synthpop": ("Pop", "Electronic"),
    "Indie Pop": ("Pop",),
    "Europop": ("Pop",),
    "Baroque Pop": ("Pop",),
    "Art Pop": ("Pop",),
    "Dance-pop": ("Pop", "Electronic"),
    "Easy Listening": ("Pop",),
    "Lounge": ("Pop",),
    "Vocal": ("Pop",),
    "Alt-pop": ("Pop",),
    "Alternative Pop": ("Pop",),
    "Chamber Pop": ("Pop",),
    "Progressive Pop": ("Pop",),
    "Psychedelic Pop": ("Pop",),
    "Teen Pop": ("Pop",),
    "Cabaret": ("Pop",),
    "Dream Pop": ("Pop", "Rock"),
    "Ambient Pop": ("Pop", "Electronic"),
    "Electropop": ("Pop", "Electronic"),
    "Glitch Pop": ("Pop", "Electronic"),
    # Celtic New Age is both halves of its name and neither is decorative.
    "Celtic New Age": ("Pop", "Folk"),
    # ── electronic ────────────────────────────────────────────────────────
    "Alternative Dance": ("Electronic",),
    "Dance": ("Electronic",),
    "House": ("Electronic",),
    "Deep House": ("Electronic",),
    "Progressive House": ("Electronic",),
    "French House": ("Electronic",),
    "Euro House": ("Electronic",),
    "Tech House": ("Electronic",),
    "Techno": ("Electronic",),
    "Melodic Techno": ("Electronic",),
    "Minimal Techno": ("Electronic",),
    "Melodic House & Techno": ("Electronic",),
    "Breakbeat": ("Electronic",),
    "Breaks": ("Electronic",),
    "Breakcore": ("Electronic",),
    "IDM": ("Electronic",),
    "Leftfield": ("Electronic",),
    "Downtempo": ("Electronic",),
    "Dubstep": ("Electronic",),
    "Bass Music": ("Electronic",),
    "UK Garage": ("Electronic",),
    "Big Beat": ("Electronic",),
    "Trance": ("Electronic",),
    "Psytrance": ("Electronic",),
    "Goa Trance": ("Electronic",),
    "Progressive Trance": ("Electronic",),
    "Tech Trance": ("Electronic",),
    "Acid": ("Electronic",),
    "Electro": ("Electronic",),
    "Electronica": ("Electronic",),
    "Drum & Bass": ("Electronic",),
    "Ambient": ("Electronic",),
    "Minimal": ("Electronic",),
    "Trip Hop": ("Electronic",),
    "Sound Collage": ("Electronic",),
    "Deep Techno": ("Electronic",),
    "Illbient": ("Electronic",),
    "New Beat": ("Electronic",),
    "Club": ("Electronic",),
    "Electro House": ("Electronic",),
    "Funky House": ("Electronic",),
    "Electro-disco": ("Electronic",),
    "Euro-disco": ("Electronic",),
    "Glitch": ("Electronic",),
    "Indietronica": ("Electronic", "Rock"),
    "Synth Funk": ("Electronic", "Funk"),
    # ── jazz ──────────────────────────────────────────────────────────────
    "Contemporary Jazz": ("Jazz",),
    "Jazz-rock": ("Jazz", "Rock"),
    "Free Jazz": ("Jazz",),
    "Vocal Jazz": ("Jazz",),
    "Soul Jazz": ("Jazz", "Funk"),
    "Soul-jazz": ("Jazz", "Funk"),
    "Latin Jazz": ("Jazz", "Latin"),
    "Afro-cuban Jazz": ("Jazz", "Latin"),
    "Big Band": ("Jazz",),
    "Swing": ("Jazz",),
    "Bossa Nova": ("Jazz", "Latin"),
    "Smooth Jazz": ("Jazz",),
    # ── blues ─────────────────────────────────────────────────────────────
    "Chicago Blues": ("Blues",),
    "Texas Blues": ("Blues",),
    "Piedmont Blues": ("Blues",),
    "Piano Blues": ("Blues",),
    "Electric Blues": ("Blues",),
    "Delta Blues": ("Blues",),
    # ── funk, soul, r&b ───────────────────────────────────────────────────
    "Soul": ("Funk",),
    "Blue-eyed Soul": ("Funk",),
    "Boogie": ("Funk",),
    "Rhythm & Blues": ("R&B",),
    "Contemporary R&B": ("R&B",),
    "Neo Soul": ("R&B",),
    # ── hip hop ───────────────────────────────────────────────────────────
    "Gangsta": ("Hip Hop",),
    "Gangsta Rap": ("Hip Hop",),
    "Hip-hop": ("Hip Hop",),
    "Pop Rap": ("Hip Hop", "Pop"),
    "G-funk": ("Hip Hop", "Funk"),
    "Turntablism": ("Hip Hop",),
    "Hardcore Hip Hop": ("Hip Hop",),
    "Hardcore Hip-hop": ("Hip Hop",),
    "Boom Bap": ("Hip Hop",),
    "Conscious": ("Hip Hop",),
    # ── reggae ────────────────────────────────────────────────────────────
    "Ska": ("Reggae",),
    "Dub": ("Reggae",),
    "Rocksteady": ("Reggae",),
    "Roots Reggae": ("Reggae",),
    "Dancehall": ("Reggae",),
    # ── latin ─────────────────────────────────────────────────────────────
    "Tango": ("Latin",),
    "Son": ("Latin",),
    "Trova": ("Latin",),
    "Danzon": ("Latin",),
    "Bolero": ("Latin",),
    "Latin Pop": ("Latin", "Pop"),
    # ── the rest ──────────────────────────────────────────────────────────
    "Modern Classical": ("Classical",),
    "Soundtrack": ("Stage & Screen",),
    # gmuse normalises a concept's label before minting its id, so `Non-Music`
    # and `Non-music` are one concept — and `Non-music` under `Non-Music` was a
    # self-edge that reached this library's log. One spelling, the library's.
    "Spoken Word": ("Non-music",),
    "Field Recording": ("Non-music",),
    "Nursery Rhymes": ("Children's",),
    "Educational": ("Children's",),
}

#: Terms deliberately left unplaced, and why. **Written down rather than
#: omitted**, so a later pass does not rediscover them as an oversight and
#: guess. A term here is not missing from the taxonomy; it is a judgement
#: nobody has made yet, and `genre*=` simply will not reach it.
CONTESTED: Final[dict[str, str]] = {
    "Gospel": "Folk, Blues and Funk depending on the record",
    "Disco": "Funk by lineage, Electronic by production",
    "Industrial": "Rock and Electronic, and the split is per artist",
    "Drone": "Electronic and Rock, and often Classical",
    "Avant-garde": "spans Rock, Electronic and Classical",
    "Avantgarde": "spans Rock, Electronic and Classical",
    "Lo-fi": "a production aesthetic, not a place in a taxonomy",
    "Roots": "roots reggae and roots music are different words",
    "Instrumental": "a description of a track, not a genre",
    "Chanson": "French song — Pop by production and Folk by tradition",
    "Tom Waits": "an artist name in a genre field — a tagging error, not a term",
}


def split_terms(value: str) -> list[str]:
    """Parse any of the three dialects in the library into a term list.

    The library holds ``A; B`` (the convention), ``A/B/C`` (a Discogs export)
    and bare single words, so the separator is whichever is present.
    """
    if not value:
        return []
    separator = ";" if ";" in value else ("/" if "/" in value else None)
    parts = value.split(separator) if separator else [value]
    return [fix_case(p.strip()) for p in parts if p.strip()]


def _key(term: str) -> str:
    """A loose identity, for matching two sources' spellings of one word.

    Punctuation and case are ignored, so `G-Funk` finds the curation written
    under `G-funk`. **Looser than gmuse's own normalisation on purpose** — this
    is for looking a judgement up, not for deciding whether two labels are one
    concept, which is what [`_same_concept`] is for.
    """
    return "".join(c for c in term.lower() if c.isalnum())


def _same_concept(a: str, b: str) -> bool:
    """Would gmuse mint one concept id from these two labels?

    It normalises whitespace and case and **nothing else**, so `Non-Music` and
    `Non-music` are one concept and `Hip Hop` and `Hip-hop` are two. Getting
    this wrong in either direction costs something: too loose and a real edge
    is dropped as a self-edge, which is how `Hip-hop` sat unplaced at
    `genre@1`; too tight and a cycle of one reaches the log, which is how
    `Non-music` did.
    """
    fold = lambda term: " ".join(term.split()).lower()  # noqa: E731
    return fold(a) == fold(b)


def implies() -> dict[str, str]:
    """The broad term the tag pipeline should put in front of each style.

    A `; ` value has room for one broad term, so the first parent wins — and
    **the walk continues until it reaches one that is actually broad**. The
    hierarchy is deeper than two in places: `Doom Metal` is under `Metal`,
    which is under `Rock`, and a tag reading `Metal; Doom Metal` would satisfy
    neither the convention nor `genre="*rock*"`.

    Derived rather than kept separately: two maps of the same judgement drift,
    and the term a tag gets and the term the model climbs to must be one.
    """
    out: dict[str, str] = {}
    for child in NARROWER:
        here, seen = child, {child}
        while (parents := NARROWER.get(here)) and parents[0] not in seen:
            here = parents[0]
            seen.add(here)
            if here in BROAD:
                break
        if here != child and here in BROAD:
            out[child] = here
    return out


def learned_from_cache(cache: Path) -> dict[str, collections.Counter[str]]:
    """What Discogs' own split teaches: style → the genres it appears under.

    **Only releases naming exactly one genre.** A release filed under both
    Electronic and Rock says nothing about which of its styles belongs where,
    and counting it would put `Techno` under `Rock` on the strength of one
    record that happened to have both.
    """
    out: dict[str, collections.Counter[str]] = collections.defaultdict(collections.Counter)
    if not cache.exists():
        return out
    rename = {"Funk / Soul": "Funk", "Folk, World, & Country": "Folk"}
    with sqlite3.connect(cache) as db:
        for table in ("lookup", "search"):
            for (payload,) in db.execute(f"select payload from {table}"):  # noqa: S608
                try:
                    row = json.loads(payload)
                except (TypeError, ValueError):
                    continue
                if not isinstance(row, dict):
                    continue
                genres = row.get("discogs_genres") or []
                if len(genres) != 1:
                    continue
                parent = rename.get(genres[0], genres[0])
                for style in row.get("discogs_styles") or []:
                    out[rename.get(style, style)][parent] += 1
    return out


#: How many single-genre releases must agree before an unreviewed edge is
#: proposed, and by what margin. Two is enough to rule out a typo; a bare
#: majority is enough because the ambiguous cases in this library are all
#: decisive — 35 to 2, 16 to 7 — and the close ones belong in review anyway.
SUPPORT: Final = 2


def terms_in(root: Path) -> collections.Counter[str]:
    """Every genre term the library's tags actually carry, and how often.

    **The review has to be driven by this, not by the cache.** A term only the
    library knows — one somebody typed, or one an authority no longer serves —
    would otherwise never appear in the review at all, and the first sign of it
    would be a style sitting at `genre@1` pretending to be a broad term.
    """
    out: collections.Counter[str] = collections.Counter()
    for path in walk(root):
        for raw in split_terms(tags_of(path).get("genre", "")):
            term = RENAME.get(raw, raw)
            if term and term.lower() not in DROP:
                out[term] += 1
    return out


def _curated_edges(seen: set[tuple[str, str]]) -> list[dict[str, object]]:
    """Every edge the curation states outright."""
    out: list[dict[str, object]] = []
    for child, parents in sorted(NARROWER.items()):
        for parent in parents:
            if (parent, child) in seen or _same_concept(parent, child):
                continue
            seen.add((parent, child))
            out.append({"broader": parent, "narrower": child, "source": "curated", "support": ""})
    return out


def _spelling_edges(
    held: collections.Counter[str] | None,
    curated_by_key: dict[str, tuple[str, ...]],
    seen: set[tuple[str, str]],
) -> list[dict[str, object]]:
    """The library's spelling of a term this curation decided under another.

    **Both get edges.** gmuse keys a concept by its normalised label, so two
    spellings are usually one concept — but where they are not, the library's
    would sit unplaced at `genre@1` with nothing saying why.
    """
    out: list[dict[str, object]] = []
    for term in sorted(held or ()):
        if term in NARROWER:
            continue
        for parent in curated_by_key.get(_key(term), ()):
            if _same_concept(parent, term) or (parent, term) in seen:
                continue
            seen.add((parent, term))
            out.append({"broader": parent, "narrower": term, "source": "curated (spelling)", "support": ""})
    return out


def propose(
    cache: Path, held: collections.Counter[str] | None = None
) -> tuple[list[dict[str, object]], list[dict[str, object]]]:
    """Every edge worth writing, and everything left for a person.

    Returns `(edges, review)`. An edge is `(broader, narrower)` with where it
    came from; a review row is a term nothing placed, with whatever evidence
    there is so the person deciding has it in front of them.
    """
    learned = learned_from_cache(cache)
    seen: set[tuple[str, str]] = set()
    # **Matched without regard to case and punctuation**, because a source
    # spells `G-Funk` where this library's tags say `G-funk`.
    curated_by_key = {_key(child): parents for child, parents in NARROWER.items()}
    broad_keys = {_key(b) for b in BROAD} | {_key(b) for parents in NARROWER.values() for b in parents}

    edges = _curated_edges(seen)
    review: list[dict[str, object]] = []

    for child, votes in sorted(learned.items()):
        if child in NARROWER or child in CONTESTED or _key(child) in broad_keys:
            # A term this library treats as broad is never somebody's child.
            # Discogs uses a few names as both a genre and a style, and the
            # co-occurrence would file `Hip Hop` under `Electronic` on one
            # record that happened to carry both.
            continue
        if _key(child) in curated_by_key:
            continue
        parent, support = votes.most_common(1)[0]
        evidence = ", ".join(f"{g} {n}" for g, n in votes.most_common())
        if _same_concept(parent, child):
            continue
        if support >= SUPPORT and support * 2 > sum(votes.values()):
            edges.append({"broader": parent, "narrower": child, "source": "discogs", "support": support})
            seen.add((parent, child))
        else:
            review.append({"term": child, "evidence": evidence, "broader": "", "apply": ""})

    edges.extend(_spelling_edges(held, curated_by_key, seen))

    # Terms the library holds that nothing above placed. **Counted**, because
    # how much music sits under an unplaced term is what decides whether it is
    # worth an argument.
    placed = {_key(str(e["narrower"])) for e in edges}
    for term, uses in sorted((held or collections.Counter()).items()):
        key = _key(term)
        if key in broad_keys or key in curated_by_key or key in placed or term in CONTESTED:
            continue
        review.append({"term": term, "evidence": f"{uses} tracks, nothing placed it", "broader": "", "apply": ""})

    for term, why in sorted(CONTESTED.items()):
        review.append({"term": term, "evidence": f"contested — {why}", "broader": "", "apply": ""})
    return edges, review


def withdrawn(edges: list[dict[str, object]], previous: Path) -> list[tuple[str, str]]:
    """Edges this pass proposed last time and no longer believes.

    **The log is append-only, so a revision is not a replacement.** The first
    run of this pass put `Doom Metal` under `Rock`; the second put it under
    `Metal`. Without a retraction it ended up directly under both, which made
    `Metal` one hop from the top and therefore a broad term at `genre@1`.

    What it can diff is its own last output — the file it wrote — which is the
    same trick `genre snapshot` uses to make an apply reversible. Edges written
    by hand or by another pass are nobody's business here.
    """
    if not previous.exists():
        return []
    was: set[tuple[str, str]] = set()
    for line in previous.read_text(encoding="utf-8").splitlines():
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("fact") == "narrowed":
            was.add((row["broader"], row["narrower"]))
    now = {(str(e["broader"]), str(e["narrower"])) for e in edges}
    return sorted(was - now)


def run(root: Path, *, write: bool) -> None:
    """Say what the hierarchy would be, and write the proposals when told."""
    held = terms_in(root.expanduser())
    edges, review = propose(out_path("genre-cache.sqlite"), held)
    gone = withdrawn(edges, out_path("genre-hierarchy.jsonl"))
    print(f"{len(held)} genre terms in the library")

    curated = sum(1 for e in edges if e["source"] == "curated")
    print(f"{len(edges)} edges: {curated} curated, {len(edges) - curated} from Discogs")
    print(f"{len(review)} terms left for a person")
    if gone:
        print(f"{len(gone)} edges withdrawn since the last run:")
        for parent, child in gone:
            print(f"    {parent} -> {child}")

    review_path = out_path("genre-hierarchy-review.tsv")
    write_tsv(review_path, review)
    print(f"wrote {review_path}")

    proposals = out_path("genre-hierarchy.jsonl")
    if not write:
        print(f"dry run — pass --write to produce {proposals}")
        return
    with proposals.open("w", encoding="utf-8") as handle:
        # **Retractions first.** Applying them after the edges would withdraw
        # one this run still believes, if a term moved and moved back.
        for parent, child in gone:
            handle.write(json.dumps({"fact": "narrowing_retracted", "broader": parent, "narrower": child}) + "\n")
        for edge in edges:
            handle.write(
                json.dumps({"fact": "narrowed", "broader": edge["broader"], "narrower": edge["narrower"]}) + "\n"
            )
    print(f"wrote {proposals} — apply it with `gmuse shelf apply`")


def register(app: typer.Typer) -> None:
    """Attach the hierarchy pass."""

    @app.command("hierarchy")
    def hierarchy_cmd(
        root: Path = typer.Argument(Path("~/MusicLibrary"), help="library root"),
        *,
        write: bool = typer.Option(default=False, help="write the proposal file"),
    ) -> None:
        """Propose the genre hierarchy: which term is narrower than which."""
        run(root, write=write)
