# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

`gmuse` (Gabriel's MUSical Experience) is a cmus-shaped terminal music player in Rust with a pure-Rust audio path —
except for a bundled, statically linked libopus.

**Read [`docs/design/music-player-tui.md`](docs/design/music-player-tui.md) before making design decisions.** It is the
project brief and it is authoritative. Sections marked **INVARIANT** are not to be revised without recording an explicit
decision in its Decision Log (§13).

§1's "decided out" list is a set of decisions, and a decision can be reopened — with a Decision Log entry, not on a
whim. **Do not argue against a direction on scope-creep grounds**; this is a library manager as much as a player and its
scope is meant to grow. Argue about whether the design is right. §1 names the directions already decided in — musical
key, harmonic recommendation, embeddings and fingerprinting held by the library model.

[`docs/design/library-api.md`](docs/design/library-api.md) is the contract: **nothing is implicit.** Import, relink and
forget are named operations; a scan reconciles holdings and reads no tags; the UI edits library metadata and writing
tags is a separate export. There is no tag editor.

[`docs/design/ingestion-flow.md`](docs/design/ingestion-flow.md) is **how it works today**: which tool touches a file
and in what order, from a download to a placed row. Read it before importing anything by hand — two orderings are
load-bearing, `ingest` and `identify` are both MusicBrainz and are not the same stage, and §4 lists what the stages do
not cover.

[`docs/design/identification-lessons.md`](docs/design/identification-lessons.md) is a **working document**: what twenty
years of retagging did to this library's metadata, and the rules that came out of being wrong about it. Read it before
writing any rule that reads a tag, a filename or a credit — every entry is a plausible rule that failed on real data,
with the measurement that killed it. §11 is the rules in one place.

[`docs/design/layers.md`](docs/design/layers.md) says which system may talk to which — the librarian, the player, the
scanner and the `tools/` scripts. Read it before adding a caller in either direction.

[`docs/design/info-panel.md`](docs/design/info-panel.md) is a **proposal**, not built: the surface that shows what the
library says about one thing and who said it. Read it before adding anything that displays metadata — the rule it turns
on is that a claim is never a value.

[`docs/design/domain-model.md`](docs/design/domain-model.md) is a **proposal**, not built: what a library entry is
versus what a file is. Read it before touching `Store`, `scan.rs` or anything keyed by a path.

[`docs/design/storage-layout.md`](docs/design/storage-layout.md) is **settled design, not built**: where files live. One
rule — **the storage tree may use structural facts only**, who made it and what release it is on, never genre, era,
catalog or tempo, because those are browsing and a tree has one dimension. `{file_under}/{album}/{track} {title}`, and
nothing else. Read it before writing anything that puts a file somewhere.

## Workflow

```sh
just                 # list all recipes
just hooks-install   # do this once per clone
just ci              # the full gate: fmt + lint + typecheck + test + build
just ui [root]       # launch the TUI
just play <file>     # play one file at the line prompt
just bench           # resampler comparison table
```

`just ci` is the quality gate and it runs **locally**. GitLab CI is deliberately minimal (a manual `verify` job) to
avoid spending shared-runner minutes re-running checks the hooks already made.

### Git hooks

`just hooks-install` points `core.hooksPath` at `.githooks/`:

- **pre-commit** — `just fmt-check` only, so committing stays fast
- **pre-push** — the full `just ci`

Bypass with `--no-verify` sparingly, and say why in the message.

## Conventions

### Commits

**Conventional Commits, enforced by the changelog.** `cliff.toml` sets `filter_unconventional = true`, so a commit that
does not parse is silently dropped from `CHANGELOG.md` — a badly typed commit does not fail loudly, it just vanishes.

Types in use: `feat`, `fix`, `perf`, `refactor`, `style`, `test`, `docs`, `build`, `ci`, `chore`. Use `!` or a
`BREAKING CHANGE:` footer for breaking changes.

The initial commit predates this convention and is filtered out of the changelog. History is conventional from
`chore/dev-workflow` onward.

### Merge requests

`main` is protected: **push access is "No one"**, so every change lands through an MR. The project is set to
fast-forward merges with squash always on, and source branches delete themselves. Branch names follow the commit type:
`feat/…`, `fix/…`, `docs/…`, `chore/…`.

Because merges are fast-forward, an MR that has fallen behind `main` needs a rebase before it can merge.

**The MR title must itself be a Conventional Commit**, because squash-on-merge uses the _title_ and throws the commit
body away. A well-typed commit under a bare title lands on `main` untyped, and `filter_unconventional` then drops it
from the changelog without a word — the same silent failure the section above describes, arriving by a different door.
It has happened once (`8b84fd3`, the #151 fix), and it is not repairable through an MR: rewriting a merged commit is a
force-push by definition, and `main` is protected precisely so that cannot happen. **So the title is the last moment
this is fixable, and after the merge the entry is simply gone.**

That is a cost worth accepting rather than working around. The initial commit is already filtered out for the same
reason, and a `cliff.toml` preprocessor special-casing one subject line would be a permanent wart describing a
historical accident.

### Configuration and theming

`config.toml` holds **settings** and **theme** as TOML, and **key bindings as command strings** — one command language
for the config, `:bind` and the `:` line, per §6. Adding a setting means adding a `SettingKey`, not a new config path.

The UI must never name a colour. Ask `theme.get(Role::…)` and add a `Role` if none fits; `Color::` literals belong only
in `theme.rs`'s defaults. See `docs/inspiration/` for where this came from. That folder is read for **navigation and
layout, keybindings, and TUI architecture only** — the player and audio path are settled by §1–§5, and nothing there is
adopted for storage, streaming or audio backends.

### Formatting

- **Rust** — `cargo fmt`, edition 2024 (`rustfmt.toml`)
- **Markdown** — `prettier`, `proseWrap: always`, `printWidth: 120` (`.prettierrc`)

`CHANGELOG.md` is in `.prettierignore`: git-cliff owns that file.

Run `just fmt` to fix both. Markdown tables get normalised by prettier, so do not hand-align them.

### Releases

git-cliff builds the changelog from commit history:

```sh
just changelog-preview   # what would land
just next-version        # what the version would become
just bump v0.1.0         # version + changelog + commit + tag, no push
just release             # push branch and tag, create the GitLab release
```

The first release needs an explicit version because there is no tag to bump from.

## Code style

From §12 of the design doc, and these are load-bearing:

- Prefer explicit typed layers over abstraction. A trait with one implementor is a struct.
- Parse at the boundary — strings become typed values at the edge, and nothing downstream handles a raw string.
- **No async.** Threads and channels. No `tokio`, no `async-std`.
- No `unsafe` outside `src/audio/`, and there only where cpal requires it.
- `thiserror` enums per module. `anyhow` only in `main`.
- **Do not add a dependency without recording it in the Decision Log** with a one-line rationale.

## The audio callback is real-time — INVARIANT

Inside the cpal callback, these are forbidden without exception: heap allocation, locking, any syscall (including
logging), panicking, and unbounded loops. It may only read the `rtrb` consumer, write to the output slice, and update
atomics. On underrun it writes silence and bumps a counter.

Violating this produces clicks and dropouts that **will not show up in tests** — only in listening. Anything expensive
(decoding, resampling) belongs on the decoder thread.

## Measurement

Two lessons already paid for, recorded in the design doc's Decision Log:

- **Measure inside the running player, not in a throughput loop.** A benchmark that resamples as fast as possible
  understated the real cost by 3–5×, because the decoder thread sleeps between packets and filter tables go cold.
- **Target: steady-state playback under ~1% of one core**, at or below `afplay`. This runs in the background while the
  user works.

Verify playback against `ffprobe` durations and require an underrun count of zero.

## CLI

Options are parsed with clap (derive), and each also reads an env var as a fallback:

| Flag             | Env              | Effect                                                        |
| ---------------- | ---------------- | ------------------------------------------------------------- |
| `-d, --device`   | `GMUSE_DEVICE`   | Output device, matched by substring on the id                 |
| `-r, --rate`     | `GMUSE_RATE`     | Force the device sample rate (to measure SRC cost)            |
| `-q, --resample` | `GMUSE_RESAMPLE` | `fast`, `balanced`, or `high`                                 |
| `-v, --volume`   | —                | Initial fader position, 1.0 is unity (see §13)                |
| `--remote <CMD>` | —                | Send one command to the running player, print the reply, exit |
| `--no-lock`      | —                | Start even if another gmuse holds the lock                    |

**Use clap with the derive API for anything CLI-shaped.** Parsing belongs at the boundary (§12): flags become typed
values in `cli.rs` and travel down as `PlayerConfig`, so nothing below reads the environment or handles a raw string.
The same rule applies to the `:` line — a line becomes a typed `Command` before anything acts on it.

## Building

Needs `cmake` for the bundled libopus. `cargo build --release --no-default-features` drops Opus and the C dependency.
Always build and test in **release** mode — debug builds are too slow to play audio without dropouts.
