# tools

Out-of-tree helpers for the music library. **None of this is gmuse.**

§1 puts tag editing out of scope, and the pattern is the one rsgain and bliss already set: an external tool writes,
gmuse reads. Reading new files into the library is likewise a CLI job rather than something the app does. Python because
these are I/O-bound, edited between runs, and reviewed by a human — and because keeping them out of `Cargo.toml` is what
keeps the boundary real.

Checked the same way sdmxlib is: `ruff` on `select = ["ALL"]` and `pyrefly` on `preset = "all"`, subtract with a written
reason, **no suppressions**. Both are at zero findings. The full-preset pyrefly run found five defects here and every
one was real — a `str | None` leaking `Any` out of a `.get()` chain, two `list()` calls on unchecked JSON, an empty
container with no inferable type, and three imports that resolved only because Python puts a script's own directory on
`sys.path`. All fixed in code.

```sh
cd tools
./.venv/bin/python -m genre.propose ~/MusicLibrary   # genre/ is a package
./.venv/bin/python audit_library.py ~/MusicLibrary
ruff check . && ruff format --check . && pyrefly check
```

## genre/ — normalise genre tags (#52)

The convention is compound, `; `-separated, **broad term first**, so `genre="*electronic*"` finds every electronic
record. Half the library was bare single words and a fifth was a Discogs slash-export.

| step                      | what it does                                                                                                                         |
| ------------------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `genre.fetch`             | MusicBrainz release → its Discogs link → genres and styles. Cached in `genre-cache.sqlite`; re-running fetches only what is missing. |
| `genre.propose`           | cache + current tags → `review.tsv`. Writes no tags.                                                                                 |
| **you edit `review.tsv`** | change `proposed`, or clear `apply` to skip a row                                                                                    |
| `genre.apply`             | `review.tsv` → tags. **Dry run unless given `--write`.**                                                                             |
| `genre.snapshot`          | every file's genre _before_ a change, so the apply can be undone                                                                     |

`apply` edits in place and keeps no record of what was there, so take a snapshot first and keep it until you are happy:

```sh
./.venv/bin/python -m genre.snapshot ~/MusicLibrary
./.venv/bin/python -m genre.apply ~/MusicLibrary --write
# and if it was wrong:
./.venv/bin/python -m genre.snapshot ~/MusicLibrary --restore --write
```

MusicBrainz is the _bridge_, not the source: it had genres for 3 of 40 releases, but 33 of them carry a Discogs link.

### Rules the review taught us

Each of these was a bug that destroyed good data, caught by reading the proposal rather than by a test:

- **Union with the existing tags, never replace them.** A source knows things the tags do not; it does not know better
  than them.
- **Do not reorder a `; ` value.** The slash form is machine output sorted alphabetically, so its order means nothing;
  the `; ` form is a human's judgement. Same operation, opposite verdict, decided by provenance.
- **Compilations are per track, and are identified by `albumartist` alone.** Unioning a `Various Artists` album replaces
  several correct genres with one wrong shared one — but testing "do the `artist` tags vary" instead catches a _guest
  credit_, which silently exempted six real albums until the outcome was checked.
- **No cap on the number of terms.** The filter matches substrings, so more terms strictly improve recall; length is a
  rendering problem `{genre:.40}` already solves.

## audit_library.py — tags, naming, ReplayGain, NFC (#49)

```sh
./.venv/bin/python audit_library.py ~/MusicLibrary/Dance
```

Read ffprobe's **`stream_tags` as well as `format_tags`** — Vorbis comments live on the stream, and reading only the
latter reports every Opus file as completely untagged. That looked like a real finding for a while.

## audit_dj.py — do filenames agree with tags? (#51)

For the DJ crate, whose names are `BPM - KEY - Artist - Title`. 41% of them disagree with their own tags about the key.
Run it against a local copy; over SMB it takes minutes.

## Credentials

`common.secret()` reads, in order: the environment, the macOS Keychain, then a `.env` beside the scripts (gitignored,
and last because it is the one that gets committed by accident).

```sh
security add-generic-password -a "$USER" -s gmuse-discogs -w
```

Omitting `-w VALUE` prompts, so the token never enters shell history.
