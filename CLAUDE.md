# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

`gmuse` (Gabriel's MUSical Experience) is a terminal music player and library manager in Rust with a pure-Rust audio
path — except for a bundled, statically linked libopus.

**cmus is the origin and no longer the authority.** The interaction model started there and the reference is still
useful, but the design has outgrown it in three directions that matter when deciding something: the library is an
append-only fact log rather than a tag reader, the player is one view onto an engine that other clients drive, and the
transport vocabulary is derived from DJ software (`docs/design/transport-keymap-spec.md`). "cmus does it this way" is
evidence, not a decision.

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
with the measurement that killed it. §16 is the rules in one place.

[`docs/design/layers.md`](docs/design/layers.md) says which system may talk to which — the librarian, the player, the
scanner and the `tools/` scripts. Read it before adding a caller in either direction.

[`docs/design/playback-programme.md`](docs/design/playback-programme.md) is **settled design, not built**: who decides
what plays next. Read it before touching the queue, shuffle, repeat or "up next" — the rule it turns on is that **a
playback mode is a rule the librarian answers, not state the engine holds.** The librarian owns the ordered programme;
the engine holds a window of two or three `(token, path)` pairs and no ordering policy, so there is no `repeat` message
and nothing for a UI to read from the engine. `deck.loop` is a different feature, not the same one at its extremes.

[`docs/design/access-layers.md`](docs/design/access-layers.md) is a **proposal, partly built**: which layer inside the
librarian may talk to which — the log, the projection, the query, the adapters. Read it before adding anything to a read
path. The rule it turns on is that **a read never touches the filesystem and never folds the log**.

`shelf query` used to do both, and an `artist=` filter returning four rows re-read 7,267 files' tags. It goes through
`librarian::reading` now — snapshot first, fold only on a cache miss, never a scan — and a one-row filter on the live
library costs **0.22s warm, 0.46s cold** (measured 2026-09-13). So the rule holds on that path in the sense that
matters, and "folds only on a miss" is still not "never folds". **The other read paths have not been measured**, which
is what is actually left of this proposal.

[`docs/design/info-panel.md`](docs/design/info-panel.md) is a **proposal**, not built: the surface that shows what the
library says about one thing and who said it. Read it before adding anything that displays metadata — the rule it turns
on is that a claim is never a value.

[`docs/design/domain-model.md`](docs/design/domain-model.md) is a **proposal**, not built: what a library entry is
versus what a file is. Read it before touching `Store`, `scan.rs` or anything keyed by a path. **§8 also says where a
name comes from** — fact names follow the event-sourcing convention, vocabulary terms are MusicBrainz's verbatim, and a
distinction borrowed from SKOS or MADS/RDF is named MusicBrainz's way. Read it before coining a term; the one that
already exists is cheap to miss.

[`docs/design/storage-layout.md`](docs/design/storage-layout.md) is **settled design, not built**: where files live. One
rule — **the storage tree may use structural facts only**, who made it and what release it is on, never genre, era,
catalog or tempo, because those are browsing and a tree has one dimension. `{file_under}/{album}/{track} {title}`, and
nothing else. Read it before writing anything that puts a file somewhere.

## Reported, never decided — do not act on a report

Several surfaces exist to **find** something and explicitly refuse to resolve it: `shelf copies`, `shelf slots`,
`shelf duplicates`, `shelf names`, `shelf artists`, `joined survey`, and the review files the `tools/` passes write.
Each says so in its own doc, and it is a design position rather than an unfinished feature — the failure mode is not
symmetric. A split is one merge from repaired; a wrong merge is not. A retired copy needs a restore; a deleted one needs
a re-download.

**Their output is a decision brief, not a work list.** Summarise, count, name the trade-off, and let the person choose.
A bucket that "looks unambiguous" is exactly where this goes wrong, because what looks redundant depends on what the
library is _for_, which the tool does not know.

**The standing example.** `shelf copies` lists one recording held as several encodings, and names the case in its own
help: _"a FLAC bought over an MP3 already held… That is a supported shape… which to keep, or whether to keep both, is
not decidable from bitrate alone."_ Holding one track in several distributions is **wanted** — a FLAC to listen to and
an MP3 that travels, the same record on an album and on a compilation. 53 files were retired on the reasoning that the
library never plays the lossy copy, which was a guess about purpose dressed as a fact about quality. `replace` (#159)
settled that two encodings of one recording is the supported shape, and history hangs off the recording so it crosses
whichever copy is kept.

The same rule holds for `shelf slots`: a disc really can play one recording twice, and a compilation really can carry
two versions.

### A measurement is how a file is marked for replacing — not a reason to move it

`quality` has a `quarantine` that moves flagged album folders to `~/LowQualityMusic`, and it was run once on 56 albums.
Every one of them is back. That is the right outcome and the wrong mechanism: **the finding was real and the response
destroyed the thing it was reporting on.**

A quarantined album is out of the library, so nothing can play it, nothing counts it, and the reason it left lives in a
manifest beside the tool rather than in the log. A file whose spectrum says `looks like a 128k source` is still the only
copy you have of that record until a better one arrives — and the moment it does, `replace` (#159) is the move, not a
restore from a folder somebody has to remember exists.

**So the measurement is the record.** `Fact::EncodingMeasured` says what the bytes are, it is keyed by encoding so each
copy speaks for itself, and it is queryable — which is what "these want replacing" actually needs to be: a filter you
can run, not a folder you moved things to. `analyser` guards a re-measure, so the answer improves without anything
moving.

The 56-album analysis of 2026-09-01 existed only as a TSV in `tools/out/spent/` for nine days, because nothing turned it
into facts. Read `quality-albums.tsv` before assuming the library has never been measured.

## The book

`just docs` builds both books; `just docs-serve` serves one with live reload (`BOOK=interaction just docs-serve` for the
other). A push to `main` publishes them to GitLab Pages.

**They hold no second copy of anything.** `tools/build_book.py` derives the chapters from `docs/design/` and
`docs/guides/`, splitting the long documents on their own `##` headings — so a renamed section renames a chapter rather
than breaking one, and there is no paraphrase to go stale. The generator **fails** when a design document is neither
published nor listed in `NOT_IN_ANY_BOOK` with a reason, so nothing goes unpublished by being forgotten.

`tools/book_guard.py` inspects the built HTML, because **`mdbook build` reports a broken include as a warning and exits
0** — a book with a hole in it would otherwise publish itself.

**Two books**, published at `/model/` and `/interaction/` with a landing page between them. The interaction book also
publishes `docs/guides/` — the seven files the binary `include_str!`s as view 7, so the guide on the web and the guide
at `7` cannot disagree.

`NOT_IN_ANY_BOOK` lists what is in neither and why; the build fails if a document is neither published nor listed.

## Workflow

```sh
just                 # list all recipes
just hooks-install   # do this once per clone
just ci              # the full gate: fmt + lint + typecheck + test + build
just ui [root]       # launch the TUI
just play <file>     # play one file at the line prompt
just bench           # resampler comparison table
just docs            # build both books; `docs-serve` for live reload
```

`just ci` is the quality gate and it runs **locally**. GitLab CI is deliberately minimal (a manual `verify` job) to
avoid spending shared-runner minutes re-running checks the hooks already made.

### Git hooks

`just hooks-install` points `core.hooksPath` at `.githooks/`:

- **commit-msg** — the subject must be a Conventional Commit, because the changelog is built from it
- **pre-commit** — `just fmt-check` only, so committing stays fast
- **pre-push** — the full `just ci`

Bypass with `--no-verify` sparingly, and say why in the message.

## Conventions

### Commits

**Conventional Commits, enforced at commit time.** `.githooks/commit-msg` rejects a subject that does not parse, which
is the only moment it is still free to fix — see below for what it cost to learn that.

Types in use: `feat`, `fix`, `perf`, `refactor`, `style`, `test`, `docs`, `build`, `ci`, `chore`. Use `!` or a
`BREAKING CHANGE:` footer for breaking changes. Adding a type means adding it to the hook **and** to `cliff.toml`, or
the commit passes the gate and then lands in `Other`.

The initial commit predates this convention and is skipped by name. History is conventional from `chore/dev-workflow`
onward.

**A commit that does not parse is listed under `Other`, not dropped.** `filter_unconventional` was `true`, so a badly
typed subject did not fail loudly — it vanished from `CHANGELOG.md` without a word. Measured: **44 of 383 commits**,
dated 2026-09-03 to 09-07 and interleaved with well-formed ones, were invisible; all of them already merged onto a
protected `main`, where a subject cannot be corrected. The catch-all that surfaces them is a _general_ rule and names no
commit, which is what separates it from the per-subject preprocessor rejected below.

The same pass found `cliff.toml`'s merge-skip rule matching `^\w+(\(.*\))?!?: merge `. This project is fast-forward
only, so it has **no merge commits at all** and that rule could never do its job — its one hit in 383 commits was
`feat(model): merge by redirect` (#68), a real feature it dropped. It now matches what git actually writes,
`^Merge (branch|remote-tracking|pull request)`.

### Merge requests

`main` is protected: **push access is "No one"**, so every change lands through an MR. The project is set to
fast-forward merges, **squash off**, and source branches delete themselves. Branch names follow the commit type:
`feat/…`, `fix/…`, `docs/…`, `chore/…`.

Because merges are fast-forward, an MR that has fallen behind `main` needs a rebase before it can merge.

**Every commit on the branch lands on `main`, so every commit must be a well-formed Conventional Commit** — not just the
MR title. `filter_unconventional = true` drops a badly typed one from the changelog without a word, and that is now a
per-commit risk rather than a per-MR one.

In exchange, **the bodies survive**. This repository puts its reasoning in commit messages — the measurement, the thing
that was tried and did not work, the rule that came out of being wrong — and squash kept only the subject line. Ten
well-typed commits are ten changelog entries and ten explanations, rather than one of each.

**One branch per issue, cut from `main`.** Under squash this was a tidiness preference; now it is not, because a branch
carrying two issues' work lands as two sets of commits with no MR boundary between them in the history. Reusing a
long-lived branch for successive MRs is how `ddd51bb` came to contain `shelf compact` and six design-doc commits under a
title about blast radii — which was unfixable, for the reason below.

**A merged commit cannot be corrected.** Rewriting one is a force-push by definition and `main` is protected precisely
so that cannot happen. It has bitten twice: `8b84fd3` (the #151 fix, an untyped title) and `ddd51bb` (a squash that
swallowed a whole branch). That is a cost worth accepting rather than working around — a `cliff.toml` preprocessor
special-casing a subject line would be a permanent wart describing a historical accident.

**That still holds, and it is not what the `Other` group is.** A preprocessor rewrites one named subject so it pretends
to parse; the catch-all names nothing and claims nothing, it just declines to throw away a commit it cannot type. The 44
stay untyped in the changelog, which is accurate — they _were_ untyped. What changed is that the failure is now visible
instead of silent, and `.githooks/commit-msg` means the bucket describes history rather than licensing more of it.

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
- **YAML** — `yamllint`, 120 columns (`.yamllint.yml`), run by `just lint-yaml` and part of `just check`

`CHANGELOG.md` is in `.prettierignore`: git-cliff owns that file.

Run `just fmt` to fix both. Markdown tables get normalised by prettier, so do not hand-align them.

### Releases

git-cliff builds the changelog from commit history:

```sh
just changelog-preview   # what would land
just next-version        # what the version would become
just bump                # cut chore/release-vX.Y.Z, bump every crate, write CHANGELOG.md, commit, tag
git push -u origin HEAD && glab mr create --fill
just release             # on main, once merged: push the tag, create the GitLab release
```

**A release is an MR, because nothing can push to `main`.** This is where the flow differs from plv's: `bump` runs on a
branch it cuts itself, the bump commit lands through a merge request like any other, and `release` refuses until the tag
is an ancestor of `origin/main`. The tag survives the merge because merges are fast-forward — the commit it points at is
the commit that reaches `main`. A squash would orphan it, which is one more reason squash stays off.

`bump` uses `cargo set-version --workspace`: every crate carries its own `version`, and without the flag only the root
moves. The first release needs an explicit version (`just bump v0.1.0`) because there is no tag to bump from; after that
git-cliff derives it from the commit types, and a hand-picked one overrides it when that answer is wrong.

## Code style

From §12 of the design doc, and these are load-bearing:

- Prefer explicit typed layers over abstraction. A trait with one implementor is a struct.
- Parse at the boundary — strings become typed values at the edge, and nothing downstream handles a raw string.
- **No async.** Threads and channels. No `tokio`, no `async-std`.
- **No `unsafe`, with two exceptions that are not the one this rule used to name.** It said "outside `src/audio/`, and
  there only where cpal requires it", and #213 found that doubly wrong: there is **no** `unsafe` in the audio path at
  all — cpal's callback API never needed any — and there are two elsewhere, both forced by a signature rather than
  chosen. `rt_guard.rs` must write `unsafe impl GlobalAlloc` because the trait is unsafe, and `engine/socket.rs`'s tests
  must wrap `std::env::set_var`, which edition 2024 made unsafe. Adding a third needs a Decision Log entry.
- `thiserror` enums per module. `anyhow` only in `main`.
- **Do not add a dependency without recording it in the Decision Log** with a one-line rationale.

## The audio callback is real-time — INVARIANT

Inside the cpal callback, these are forbidden without exception: heap allocation, locking, any syscall (including
logging), panicking, and unbounded loops. It may only read the `rtrb` consumer, write to the output slice, and update
atomics. On underrun it writes silence and bumps a counter.

Violating this produces clicks and dropouts that **will not show up in tests** — only in listening. Anything expensive
(decoding, resampling) belongs on the decoder thread.

## A test is not finished until it has been seen to fail

**Revert the fix, run the test, watch it fail, put the fix back.** A minute, and it is the only thing that separates a
test from a comment. Five tests in one day ran, passed, and could not have failed — the Decision Log entry of 2026-09-12
lists them and why this codebase invites it.

The shape is always one of two. A predicate that is **vacuously true**: `assert!(0 <= 0)` because the thing it counted
was empty, or "every X is named" over a list that parsed to nothing. Or a harness that **silently does less than it
claims**: no test matched the filter, no client connected, no lock was taken, no process started.

So write the second assertion, the one that proves the instrument was attached: `assert!(variants.len() > 50)` before
checking which are named, `assert!(found >= 10)` before checking what they call, `assert!(before > 0)` before checking
that a count did not grow. Four of the five would have died to that line.

This bites here rather than everywhere because the interesting claims in this project are **absences** — no path crosses
the wire, no second appender, no command accepted and ignored — and an absence is asserted by showing a set is empty.
`assert!(x.is_empty())` passes identically whether nothing is wrong or nothing was looked at.

**And the second assertion has to be computed some other way than the thing it vouches for** — the 2026-09-24 entry,
which found five more green-against-broken-code tests where **four already had one**. The instrument must be able to
fail while the claim passes, and the claim while the instrument passes; if one can only fail when the other does, it is
the claim written twice. Ask `under_cursor` directly rather than comparing two focuses; assert
`View::ALL[3] != View::Playlist` as pure data rather than comparing two `Ui`s; read the version before the change rather
than hard-coding the one you expect.

**The tell:** sabotage the fix, and if the _instrument_ is what fires — with a message blaming the fixture — the two are
the same assertion. And **make the sabotage targeted**: a test can survive deleting the whole function and still be
vacuous for the line that matters, so break the smallest thing that makes the claim false.

## A claim that nothing checks is the fourth way to be green and wrong

§13's testing rule covers a predicate that is vacuously true and a harness that does less than it claims. **A sentence
in a comment is the one with no instrument at all** — it fails silently, and an audit found ten of them in one afternoon
(#250, #252).

Two checks run in `just ci`, in `tests/the_claims_we_can_check.rs`:

- **An unreachable `pub fn` must say why it is here.** `dead_code` never fires for a `pub` item in a library crate, so
  `clippy -D warnings` cannot see one — which is how §7.10's disconnect policy went unimplemented with only an
  unreferenced `over_limit` to show for it. A `//` comment or a `reason =` both count; the requirement is that a reader
  gets an explanation.
- **A deferral must name an issue.** Not that the issue is open — only that one is cited, because a deferral with no
  owner can never be checked.

`just stale-claims` asks the tracker which of those issues have closed. **A recipe and not a test**, because a test
needing the network gets ignored and an ignored test looks like coverage. Its output is a brief, not a work list: a
comment may cite the issue that created a rule as well as the one that would finish it.

None of this catches the §4.2 shape — a gate that named a thing that existed rather than a capability that did not. That
one has no mechanism, only the advice: **name the condition, not the successor.**

## Measurement

Two lessons already paid for, recorded in the design doc's Decision Log:

- **Measure inside the running player, not in a throughput loop.** A benchmark that resamples as fast as possible
  understated the real cost by 3–5×, because the decoder thread sleeps between packets and filter tables go cold.
- **Target: steady-state playback under ~1% of one core**, at or below `afplay`. This runs in the background while the
  user works.

Verify playback against `ffprobe` durations and require an underrun count of zero.

## CLI

Options are parsed with clap (derive), and each also reads an env var as a fallback:

| Flag             | Env              | Effect                                                         |
| ---------------- | ---------------- | -------------------------------------------------------------- |
| `-d, --device`   | `GMUSE_DEVICE`   | Output device, matched by substring on the id                  |
| `-r, --rate`     | `GMUSE_RATE`     | Force the device sample rate (to measure SRC cost)             |
| `-q, --resample` | `GMUSE_RESAMPLE` | `fast`, `balanced`, or `high`                                  |
| `-v, --volume`   | —                | Initial fader position, 1.0 is unity (see §13)                 |
| `--no-attach`    | `GMUSE_ATTACH=0` | Keep playback in this process; attaching is the default (#233) |
| `--no-lock`      | —                | Start even if another gmuse holds the lock                     |

**This table is the env-var-backed options, not the CLI.** The subcommands are `shelf` (18 library operations),
`engine`, `ctl` and `events`, and `gmuse --help` is the authority on all of them — #247 is where the docs stop lagging
it.

**`--remote` is gone** (#243). `gmuse ctl <method-or-line>` replaced it, and the two overlapped for longer than intended
because `ctl` existing was mistaken for `ctl` being able to do `--remote`'s job: a status bar polls, and no method
reported playback state until `playback.get`. `gmuse ctl engine.shutdown` stops a running engine;
`gmuse ctl playback.get '{"format":"{title}"}'` is what a status bar asks. Params are a JSON word after the method,
because everything after the command is taken as part of the command.

**Use clap with the derive API for anything CLI-shaped.** Parsing belongs at the boundary (§12): flags become typed
values in `cli.rs` and travel down as `PlayerConfig`, so nothing below reads the environment or handles a raw string.
The same rule applies to the `:` line — a line becomes a typed `Command` before anything acts on it.

## Building

Needs `cmake` for the bundled libopus. `cargo build --release --no-default-features` drops Opus and the C dependency.
Always build and test in **release** mode — debug builds are too slow to play audio without dropouts.
