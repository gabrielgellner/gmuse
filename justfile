# gmuse — development workflow
#
# The quality gate is local: `just ci`, enforced by the pre-push hook
# (`just hooks-install`). GitHub Actions repeats the Rust half on each pull
# request — see .github/workflows/.

# List available recipes
default:
    @just --list

# ── Build & run ───────────────────────────────────────────────────────────────

# Build in release mode (debug builds are too slow for glitch-free audio)
[group('dev')]
build:
    cargo build --release --workspace

# The root is remembered in the cache, so `just ui` alone is the usual way. A
# first argument starting with `-` is a flag rather than a root: `just ui --log`
# was passing `--log` to `--library` and failing, which is the kind of thing a
# positional-with-a-default cannot tell apart on its own.
#
# `just --list` shows the *last* comment line, so the one-liner goes last.

# Launch the TUI: just ui [library-root] [flags...]
[group('dev')]
ui dir="" *flags:
    #!/usr/bin/env bash
    set -euo pipefail
    args=()
    case "{{ dir }}" in
        "") ;;
        -*) args+=("{{ dir }}") ;;
        *) args+=(--library "{{ dir }}") ;;
    esac
    # `${a[@]+...}` because bash 3.2 — which is what macOS ships — treats an
    # empty array as unset under `set -u`.
    cargo run --release -- ${args[@]+"${args[@]}"} {{ flags }}

# Launch the TUI with session logging, for a listening test
[group('dev')]
listen dir="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{ dir }}" ]; then
        cargo run --release -- --log --library "{{ dir }}"
    else
        cargo run --release -- --log
    fi

# Show what the last listening session recorded
[group('dev')]
log:
    #!/usr/bin/env bash
    set -euo pipefail
    LOG="${XDG_STATE_HOME:-$HOME/.local/state}/gmuse/session.log"
    if [ ! -f "$LOG" ]; then
        echo "no log yet — run 'just listen' first"
        exit 0
    fi
    echo "$LOG"
    echo
    tail -50 "$LOG"
    echo
    echo "underruns recorded: $(grep -c '^\[.*underrun' "$LOG" || true)"

# Rebuild the cache from scratch: opens the TUI, which scans on startup, then `q`
[group('dev')]
rescan dir:
    cargo run --release -- --library "{{ dir }}" --rescan

# Build without Opus support, dropping the only C dependency (needs no cmake)
[group('dev')]
build-no-opus:
    cargo build --release --no-default-features

# ── Format ────────────────────────────────────────────────────────────────────

# Format Rust and Markdown in place
[group('dev')]
fmt:
    cargo fmt --all
    prettier --write "**/*.md"

# Check formatting without writing (used by `just check`)
[group('dev')]
fmt-check:
    cargo fmt --all -- --check
    prettier --check "**/*.md"

# ── Lint & test ───────────────────────────────────────────────────────────────

# Lint the YAML — 120 columns, per .yamllint.yml
lint-yaml:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! command -v yamllint >/dev/null 2>&1; then
        echo "lint-yaml needs yamllint: pipx install yamllint" >&2
        exit 1
    fi
    yamllint .

# Clippy across all targets and features, warnings are errors
[group('dev')]
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Build the Python extension module — #197.
#
# `cargo` cannot link it: an extension module's Python symbols resolve at load
# time inside the interpreter, so a plain `cargo build --features python` fails
# at the link step. `maturin` passes the flags that make that fine. `just ci`
# still *type-checks* it, through `clippy --all-features`, which is what stops
# it rotting between builds.
[group('build')]
wheel:
    uvx maturin build --release

# Build it into the tools' environment, for working on them.
#
# **`uv sync`, not `maturin develop`.** `tools/pyproject.toml` depends on the
# crate as a path with `editable = false`, so every `uv run` resyncs the
# environment and reinstalls its own cached build of gmuse-core — quietly
# undoing anything maturin put there. That looks exactly like a Rust change
# not taking effect, and it cost three debugging rounds on #204. Rebuilding
# through uv means the thing that resyncs is the thing that built it.
[group('build')]
wheel-dev:
    cd tools && uv sync --reinstall-package gmuse-core

# Run the test suite
[group('test')]
test:
    cargo test --release --workspace

# Run one bench test and insist that it ran.
#
# **`--workspace`, and a check that something matched.** These were
# `cargo test --release <filter>`, which searches the root package only — so
# when #213 moved `seam.rs` into `gmuse-library`, `bench-fold` started printing
# nothing and exiting 0. The bench still ran under `just ci`, which is
# workspace-wide; what was lost was the recipe whose entire purpose is to show
# the table. A bench that reports nothing and succeeds is the same failure as a
# boundary test that reads no files.
_bench filter:
    #!/usr/bin/env bash
    set -euo pipefail
    out=$(cargo test --release --workspace {{filter}} -- --nocapture 2>&1)
    echo "$out"
    if ! grep -qE "^test .*{{filter}}.* \.\.\. ok" <<<"$out"; then
        echo "" >&2
        echo "bench: no test matched {{filter}} — it has moved or been renamed." >&2
        exit 1
    fi

# Build the book: the information model and the layers — #247.
#
# `tools/build_book.py` derives `docs/book/src/` from `docs/design/`, so there is no
# second copy of any claim. The guard below is not ceremony: **mdBook reports a broken
# include as a WARN and exits 0**, leaving the literal `{{{{#include …}}` in the published
# page — so a renamed document would silently produce a book with a hole. The generator
# avoids includes entirely, and this checks the artifact rather than trusting it.

# Build both books — open docs/books/<book>/book/index.html
docs:
    #!/usr/bin/env bash
    set -euo pipefail
    command -v mdbook >/dev/null 2>&1 || { echo "docs needs mdbook: cargo install mdbook" >&2; exit 1; }
    command -v mdbook-mermaid >/dev/null 2>&1 || { echo "docs needs mdbook-mermaid: cargo install mdbook-mermaid" >&2; exit 1; }
    python3 tools/build_book.py
    for b in model interaction; do
        # Drops `mermaid.min.js` beside each book. Generated rather than committed —
        # 2.5 MB of vendored javascript the tool reproduces — and idempotent on a
        # `book.toml` that already names the preprocessor, so this is safe to repeat.
        mdbook-mermaid install "docs/books/$b"
        mdbook build "docs/books/$b"
        python3 tools/book_guard.py "docs/books/$b/book"
    done

# Serve a book with live reload (BOOK=model|interaction, default model)
docs-serve:
    #!/usr/bin/env bash
    set -euo pipefail
    python3 tools/build_book.py
    book="${BOOK:-model}"
    mdbook-mermaid install "docs/books/$book"
    # `BOOK=interaction just docs-serve` for the other one.
    mdbook serve "docs/books/$book" --open

# Report deferrals whose issue has been closed — #252.
#
# **A recipe and not a test, and that split is the decision.** Whether `#123` is still
# open is a question for the issue tracker, and a test that needs the network is one
# that gets `#[ignore]`d the first time it fails on a train. What *is* local —
# that every deferral names an issue at all, and that no unreachable `pub fn` is
# silent — lives in `tests/the_claims_we_can_check.rs` and runs in `ci`.
#
# So this is the half that cannot be automated honestly, made cheap enough to run
# deliberately. The audit it replaces took an afternoon and found ten.

# Report deferrals whose issue has since closed (needs gh)
stale-claims:
    #!/usr/bin/env bash
    set -uo pipefail
    command -v gh >/dev/null 2>&1 || { echo "stale-claims needs gh" >&2; exit 1; }
    echo "Deferrals that name an issue, and whether that issue is still open:"
    echo
    # Forward-looking only. "was called `qualifier` until #117" is a record of something
    # finished and correct as written; conflating the two grammars is what turned three
    # findings into a reported seven (§13, 2026-09-13).
    grep -rn -iE 'stays until|is not wired up|has no caller yet|gets a caller' \
        --include='*.rs' src/ crates/ \
        | grep -v 'tests/the_claims_we_can_check.rs' \
        | while IFS= read -r hit; do
        where="${hit%%:*}"
        rest="${hit#*:}"
        line="${rest%%:*}"
        # The issue may be named on a neighbouring line of the same comment.
        window=$(sed -n "$((line>3?line-3:1)),$((line+4))p" "$where" 2>/dev/null)
        issues=$(grep -oE '#[0-9]+' <<<"$window" | sort -u | tr -d '#')
        if [ -z "$issues" ]; then
            printf '  %-58s %s\n' "$where:$line" "NAMES NO ISSUE"
            continue
        fi
        for n in $issues; do
            # The GitHub tracker kept GitLab's numbers (every issue was imported in
            # order), so a `#n` written before the move still names the same issue.
            state=$(gh issue view "$n" --json state --jq .state 2>/dev/null)
            case "$state" in
                CLOSED) printf '  %-58s #%-5s %s  <-- look\n' "$where:$line" "$n" "closed" ;;
                OPEN)   printf '  %-58s #%-5s %s\n' "$where:$line" "$n" "open" ;;
                *)      printf '  %-58s #%-5s unknown\n' "$where:$line" "$n" ;;
            esac
        done
    done
    echo
    echo "Reported, never decided. A closed issue is a reason to look, not a verdict:"
    echo "a comment may cite the issue that CREATED the rule as well as the one that"
    echo "would finish it, and both read the same to a grep. What each closed hit"
    echo "means is one of:"
    echo
    echo "  - the work was done and the caveat is stale     -> delete the caveat"
    echo "  - the issue closed without doing this part      -> re-own it, or reopen"
    echo "  - the number is a citation, not an owner        -> nothing to do"
    echo
    echo "Only the middle one is a bug, and only reading the code tells you which."

# Show what it costs to draw a view (#121). Runs in `ci` too — it is fast.
bench-shelf:
    @just _bench drawing_a_view

# Show what it costs to rebuild the librarian (#123). Runs in `ci` too.
bench-fold:
    @just _bench rebuilding_the_librarian

# Show what it costs to walk a projection to a thing (#281). Runs in `ci` too.
bench-reveal:
    @just _bench revealing_a_track

# Show what the info window costs a keystroke (#285). Runs in `ci` too.
bench-panel:
    @just _bench the_info_window_costs_a_still_cursor_nothing

# Show that a page costs the same with crates as without (#289). Runs in `ci`.
bench-crates:
    @just _bench a_page_does_not_cost_more_because_the_library_has_crates

# Show that a focus reaches its recordings by lookup, not by scan (#287).
bench-reach:
    @just _bench covers_costs_less_than_the_scan_it_replaced

# Show what one appended fact costs the librarian (#223). Runs in `ci` too.
bench-write:
    @just _bench one_rating_is_not_a_whole_rebuild

# A feature rather than a flag: a counter on every allocation is two atomics a
# release build should not pay to answer a question nobody asks at runtime.
#
# Where the engine's memory goes: live and peak at each startup step (#255)
[group('test')]
memory:
    cargo run --release --features measure-memory -- engine --null-audio

# What the running processes cost at rest: CPU over a 20s window and physical footprint
# (#255). Needs a running engine — and a view, or a playing track, if that is the
# shape being measured. `ps` RSS is deliberately not printed: it double-counts the
# dyld shared cache and follows the peak, which is how #255 opened at "1.1 GB".
[group('test')]
at-rest secs="20":
    tools/at_rest.sh {{secs}}

# What a query costs, and what the locator costs it (#261). Needs the real library.
[group('test')]
query-cost data="~/MusicLibrary/.gmuse":
    cargo run --release --example where_a_query_goes -- {{data}}

# What a row costs read over the socket (#261). Needs a running engine.
[group('test')]
read-cost:
    cargo run --release --example what_a_read_costs

# What changing the view costs over the socket (#261). Needs a running engine.
[group('test')]
view-cost:
    cargo run --release --example what_a_view_costs

# What pairing recordings to copies costs over the socket (#261). Needs a running engine.
[group('test')]
pairing-cost:
    cargo run --release --example what_the_pairing_costs

# Which half of a tree costs the 0.4s: the grouping or the flatten (#261). Needs the real library.
[group('test')]
tree-cost data="~/MusicLibrary/.gmuse":
    cargo run --release --example where_the_tree_goes -- {{data}}

# What the info panel's history costs, step by step — the K-over-an-artist lag. Needs
# the real library.
[group('test')]
history-cost data="~/MusicLibrary/.gmuse":
    cargo run --release --example what_history_costs -- {{data}}

# What each part of a fold costs, attributed (#255). Needs the real library.
[group('test')]
memory-parts data="~/MusicLibrary/.gmuse" roots="~/MusicLibrary":
    cargo run --release --features gmuse-library/testing \
        --example where_the_memory_goes -- {{data}} {{roots}}

# Show the resampler benchmark table (not part of `ci` — it takes ~30s)
[group('test')]
bench:
    cargo run --release --example bench_resample

# Formatting + lint + a type check, no tests
[group('test')]
check: fmt-check lint lint-yaml leak-check
    cargo check --all-targets --all-features

# **No report data in the repository.** The repo is public and the library is not.
#
# Three scan logs listing 3,490 of the library's files were committed by accident in
# September 2026, deleted a week later, and were still in the history when the
# repository went public; so were a 66-line album list and a genre sample. Removing
# them meant rewriting the history and recreating the repository. A tool's output goes
# wherever its caller says (`library-api.md` §7), so the repository is where it must be
# refused: `.gitignore` ignores the report formats, and this refuses whatever got past
# it — a `git add -f`, a new format, a report pasted into a document.
#
# Reads the index, not the working tree, so as a pre-commit check it judges exactly
# what is about to be committed.
[group('test')]
leak-check:
    #!/usr/bin/env bash
    set -euo pipefail
    status=0
    # Report formats. A fixture that has to be one is listed here by name, with a reason.
    allowed='^tests/transcripts/[^/]+\.ndjson$'
    data=$(git ls-files | grep -E '\.(tsv|csv|jsonl|log|sqlite|db|postcard|zip|tar|tgz|gz|m3u8?|pls|xspf)$' \
        | grep -vE "$allowed" || true)
    if [ -n "$data" ]; then
        echo "leak-check: report or data files are tracked:" >&2
        sed 's/^/  /' <<<"$data" >&2
        status=1
    fi
    # A file naming more than five library paths is a listing, not an example. The
    # most any document needs today is four.
    listings=$(git grep --cached -c -E 'MusicLibrary/[^ `"]|/Volumes/Media/[A-Z]' -- . ':!CHANGELOG.md' \
        | awk -F: '$NF > 5' || true)
    if [ -n "$listings" ]; then
        echo "leak-check: files that list the library (path:count):" >&2
        sed 's/^/  /' <<<"$listings" >&2
        status=1
    fi
    if [ "$status" -ne 0 ]; then
        echo "leak-check: reports belong in tools/out/ or outside the repository — see tools/README.md." >&2
    fi
    exit "$status"

# The tools' own tests — #298.
#
# **In the gate, because nothing ran them.** `pytest` was a dev dependency with
# a config section and no caller, so 223 tests were coverage in the sense that
# they existed. That is the shape `CLAUDE.md` names about `just stale-claims`:
# a check nothing runs looks exactly like a check that passes.
#
# `--reinstall-package` first, because the thing under test is a compiled
# extension built from the crate beside it and nothing else resyncs it. An
# 18-day-old `gmuse_core` is what #298 turned out to be, and a gate that tests
# the Python against a stale build of the Rust is testing two different
# libraries.
[group('test')]
tools-test:
    cd tools && uv sync --quiet --reinstall-package gmuse-core && uv run pytest -q

# Full local gate: format, lint, typecheck, tests, release build.
# Enforced by the pre-push hook — see `just hooks-install`.
[group('test')]
ci: check test tools-test build

# ── Hooks ─────────────────────────────────────────────────────────────────────

# Install the git hooks: fmt check on commit, full `just ci` on push
[group('dev')]
hooks-install:
    #!/usr/bin/env bash
    set -euo pipefail
    git config core.hooksPath .githooks
    echo "core.hooksPath -> .githooks"
    echo "  commit-msg: Conventional Commit subject  (bypass: git commit --no-verify)"
    echo "  pre-commit: 'just fmt-check'  (bypass: git commit --no-verify)"
    echo "  pre-push:   'just ci'         (bypass: git push --no-verify)"

# Stop running the git hooks
[group('dev')]
hooks-uninstall:
    git config --unset core.hooksPath
    @echo "hooks disabled"

# ── Release ───────────────────────────────────────────────────────────────────

# Preview the changelog for unreleased commits
[group('release')]
changelog-preview:
    git-cliff --unreleased

# Regenerate CHANGELOG.md from all history
[group('release')]
changelog-full:
    git-cliff -o CHANGELOG.md
    @echo "CHANGELOG.md updated"

# Print the next version that `just bump` would produce
[group('release')]
next-version:
    @git-cliff --bumped-version

# First release needs an explicit version: just bump v0.1.0
#
# **`main` is protected, so a release is a pull request like anything else.**
# Nothing pushes to `main` directly (see CLAUDE.md), which is what makes plv's
# `git push origin HEAD` fail here. So this cuts `chore/release-vX.Y.Z` from
# `main` when run there, and the bump commit lands through a pull request.
#
# **No tag here.** GitHub's rebase-merge writes the commit again, so a tag made
# on the branch would point at a commit `main` never holds. `release` tags the
# merged commit instead.
#
# Does NOT push — review with `git log --oneline -5`, then `gh pr create --fill`.
#
# Bump version: every crate's Cargo.toml + CHANGELOG.md, commit — on a branch
[group('release')]
bump version="":
    #!/usr/bin/env bash
    set -euo pipefail

    if [ -n "{{ version }}" ]; then
        NEXT="{{ version }}"
        [[ "$NEXT" == v* ]] || NEXT="v$NEXT"
    else
        NEXT=$(git-cliff --bumped-version 2>/dev/null) || {
            echo ""
            echo "ERROR: could not calculate next version (no tags found?)."
            echo "For the first release run:  just bump v0.1.0"
            exit 1
        }
    fi

    VER="${NEXT#v}"
    [ -z "$(git status --porcelain)" ] || { echo "ERROR: working tree is not clean." >&2; exit 1; }
    # git-cliff answers a stale version when nothing has landed since the
    # last tag ("There is nothing to bump"), so this is also the no-op guard.
    ! git rev-parse -q --verify "refs/tags/$NEXT" >/dev/null || { echo "ERROR: tag $NEXT already exists — nothing to release?" >&2; exit 1; }

    if [ "$(git branch --show-current)" = "main" ]; then
        git switch -c "chore/release-$NEXT"
    fi
    echo "Bumping to $NEXT on $(git branch --show-current)"

    # Every crate carries its own `version`, so bump them together — without
    # `--workspace` only the root moves and the members stay at 0.1.0 forever.
    cargo set-version --workspace "$VER"
    # CHANGELOG.md is in .prettierignore: git-cliff owns that file.
    git-cliff --tag "$NEXT" -o CHANGELOG.md

    git add Cargo.toml Cargo.lock crates/*/Cargo.toml CHANGELOG.md
    git commit -m "chore(release): $NEXT"

    echo ""
    echo "Created the release commit for $NEXT (untagged until it merges)."
    echo "Review with:  git log --oneline -5"
    echo "Then:         git push -u origin HEAD && gh pr create --fill"
    echo "Once merged:  git switch main && git pull --ff-only && just release"

# Run on `main` after the bump pull request has merged. The tag goes on the
# release commit as `main` holds it, which is the only copy a rebase-merge leaves.
#
# Tag the merged release commit, push the tag, create the GitHub release
[group('release')]
release:
    #!/usr/bin/env bash
    set -euo pipefail

    git fetch origin main --quiet
    VER=$(cargo metadata --no-deps --format-version 1 \
        | python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "gmuse"))')
    TAG="v$VER"
    ! git rev-parse -q --verify "refs/tags/$TAG" >/dev/null || { echo "ERROR: $TAG is already tagged." >&2; exit 1; }
    COMMIT=$(git log origin/main --format=%H -n 1 --grep="^chore(release): $TAG\$")
    [ -n "$COMMIT" ] || {
        echo "ERROR: no 'chore(release): $TAG' on origin/main yet — merge the release pull request first." >&2
        exit 1
    }

    git tag -a "$TAG" -m "Release $TAG" "$COMMIT"
    git push origin "$TAG"

    # Release notes = this tag's section of the changelog, from the first
    # group heading on. The version heading is dropped: the release is already
    # titled for its version, so repeating it inside the notes is noise.
    NOTES=$(git-cliff --latest --strip all | awk 'f || /^###/ { f = 1; print }')

    gh release create "$TAG" \
        --title "Release $TAG" \
        --notes "$NOTES"

    echo "Released $TAG to GitHub."

# Copy the library to the archive — `storage-layout.md` §8. Dry run by default.
[group('dev')]
archive write="":
    #!/usr/bin/env bash
    set -euo pipefail

    LIB="${HOME}/MusicLibrary"
    # **Over ssh, not the SMB mount** (#328). rsync runs on both ends, so the
    # NAS compares its own disk: a dry run of ~12,000 files is 0.5s against
    # 118s through `/Volumes/Media`, and the copy moved 34 MB/s against 3.5-6.5.
    ARCHIVE="${GMUSE_ARCHIVE:-asnas:/volume1/Media/GMuseLibrary/}"
    HOST="${ARCHIVE%%:*}"

    # **The archive is the device whose holdings are complete** (§8), so this
    # mirrors rather than adds: several hundred files are *renamed* in an
    # ordinary week, rsync cannot see a rename, and without --delete the NAS
    # keeps both the new path and the old one for every one of them.
    #
    # **The log travels with the music** and both halves together: history.toml
    # is the base and events-*.log is everything since, so copying one without
    # the other silently loses recent plays and ratings.
    ssh -o BatchMode=yes -o ConnectTimeout=5 "$HOST" true \
      || { echo "cannot reach $HOST over ssh"; exit 1; }

    # **`--no-perms`, because the share's modes are SMB's.** Everything written
    # through the mount is 777 on the NAS, and `-a` alone would chmod ~9,400
    # files to this machine's 644/755 — a change nobody asked for. Times are
    # still kept, and over ssh they stick, which the SMB route could not do:
    # it re-stamped every transferred file afterwards so the next run would
    # not send them all again.
    #
    # The accented-name false alarm is gone with the mount (#328): the share
    # decomposed names, and ssh stores what it is sent.
    RSYNC=(rsync -a --no-perms --delete)

    if [ "{{write}}" != "write" ]; then
        echo "dry run — pass 'write' to copy"
        { "${RSYNC[@]}" --dry-run --itemize-changes "$LIB/" "$ARCHIVE" || [ $? -eq 24 ]; } \
          | awk '/^\*deleting/ {d++} /^[<>]f/ {s++} END {printf "  %d to send, %d to delete\n", s+0, d+0}'
        exit 0
    fi

    # **24 is a warning, not a failure**, and `set -e` treated it as one. It
    # means a file vanished between the file list and the transfer, which here
    # is the log being appended to while the copy runs.
    "${RSYNC[@]}" --stats "$LIB/" "$ARCHIVE" || [ $? -eq 24 ]

# Render the library for Plex onto the NAS — `renderings.md`, #336. Dry run by default.
[group('dev')]
plex write="":
    #!/usr/bin/env bash
    set -euo pipefail

    # **A rendering, not a copy**: the best copy of each track, lossless
    # encoded to MP3 320 on this machine where the FLAC is, every file tagged
    # with what the library states. The archive above is the masters byte for
    # byte; this is what Plex reads, and nothing reads it back.
    #
    # Over ssh like `archive` (#328), into its own directory beside it, which
    # the renderer refuses to write into unless it holds its own manifest.
    TARGET="${GMUSE_PLEX:-asnas:/volume1/Media/PlexMusic}"
    WRITE=()
    if [ "{{write}}" = "write" ]; then WRITE=(--write); fi
    cd tools && uv run gmuse-tools render "$TARGET" --encode mp3-320 "${WRITE[@]}"
