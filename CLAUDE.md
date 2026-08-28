# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

`gmuse` (Gabriel's MUSical Experience) is a cmus-shaped terminal music player in Rust with a pure-Rust audio path —
except for a bundled, statically linked libopus.

**Read [`docs/design/music-player-tui.md`](docs/design/music-player-tui.md) before making design decisions.** It is the
project brief and it is authoritative. Sections marked **INVARIANT** are not to be revised without recording an explicit
decision in its Decision Log (§13). Anything in the §1 "out of scope" list stays out, even when a crate makes it trivial
— scope creep is the named failure mode for this project.

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

| Flag             | Env              | Effect                                             |
| ---------------- | ---------------- | -------------------------------------------------- |
| `-d, --device`   | `GMUSE_DEVICE`   | Output device, matched by substring on the id      |
| `-r, --rate`     | `GMUSE_RATE`     | Force the device sample rate (to measure SRC cost) |
| `-q, --resample` | `GMUSE_RESAMPLE` | `fast`, `balanced`, or `high`                      |
| `-v, --volume`   | —                | Initial volume, 0.0–2.0                            |

**Use clap with the derive API for anything CLI-shaped.** Parsing belongs at the boundary (§12): flags become typed
values in `cli.rs` and travel down as `PlayerConfig`, so nothing below reads the environment or handles a raw string.
The same rule applies to the stdin driver in `repl.rs` — a line becomes a typed `Line` before anything acts on it.

## Building

Needs `cmake` for the bundled libopus. `cargo build --release --no-default-features` drops Opus and the C dependency.
Always build and test in **release** mode — debug builds are too slow to play audio without dropouts.
