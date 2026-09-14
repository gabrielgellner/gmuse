# gmuse — development workflow
#
# The quality gate is local: `just ci`, enforced by the pre-push hook
# (`just hooks-install`). GitLab CI is deliberately minimal — see .gitlab-ci.yml.

# List available recipes
default:
    @just --list

# ── Build & run ───────────────────────────────────────────────────────────────

# Build in release mode (debug builds are too slow for glitch-free audio)
[group('dev')]
build:
    cargo build --release --workspace

# Launch the TUI: just ui [library-root]   (the root is remembered in the cache)
[group('dev')]
ui dir="":
    #!/usr/bin/env bash
    set -euo pipefail
    if [ -n "{{ dir }}" ]; then
        cargo run --release -- --library "{{ dir }}"
    else
        cargo run --release
    fi

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

# Report deferrals whose issue has since closed (needs glab)
stale-claims:
    #!/usr/bin/env bash
    set -uo pipefail
    command -v glab >/dev/null 2>&1 || { echo "stale-claims needs glab" >&2; exit 1; }
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
            state=$(glab api "projects/:id/issues/$n" 2>/dev/null \
                | python3 -c 'import json,sys; print(json.load(sys.stdin).get("state","?"))' 2>/dev/null)
            case "$state" in
                closed) printf '  %-58s #%-5s %s  <-- look\n' "$where:$line" "$n" "$state" ;;
                opened) printf '  %-58s #%-5s %s\n' "$where:$line" "$n" "$state" ;;
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

# What a query costs, and what the locator costs it (#261). Needs the real library.
[group('test')]
query-cost data="~/MusicLibrary/.gmuse":
    cargo run --release --example where_a_query_goes -- {{data}}

# What a row costs read over the socket (#261). Needs a running engine.
[group('test')]
read-cost:
    cargo run --release --example what_a_read_costs

# What pairing recordings to copies costs over the socket (#261). Needs a running engine.
[group('test')]
pairing-cost:
    cargo run --release --example what_the_pairing_costs

# Which half of a tree costs the 0.4s: the grouping or the flatten (#261). Needs the real library.
[group('test')]
tree-cost data="~/MusicLibrary/.gmuse":
    cargo run --release --example where_the_tree_goes -- {{data}}

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
check: fmt-check lint lint-yaml
    cargo check --all-targets --all-features

# Full local gate: format, lint, typecheck, tests, release build.
# Enforced by the pre-push hook — see `just hooks-install`.
[group('test')]
ci: check test build

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

# Bump version: update Cargo.toml + CHANGELOG.md, commit, tag.
# First release needs an explicit version: just bump v0.1.0
# Does NOT push — review with `git log --oneline -5`, then `just release`.
[group('release')]
bump version="":
    #!/usr/bin/env bash
    set -euo pipefail

    if [ -n "{{ version }}" ]; then
        NEXT="{{ version }}"
        [[ "$NEXT" == v* ]] || NEXT="v$NEXT"
    else
        NEXT=$(git-cliff --bumped-version) || {
            echo ""
            echo "ERROR: could not calculate next version (no tags found?)."
            echo "For the first release run:  just bump 0.1.0"
            exit 1
        }
    fi

    VER="${NEXT#v}"
    echo "Bumping to $NEXT"

    cargo set-version "$VER"
    git-cliff --tag "$NEXT" -o CHANGELOG.md
    prettier --write CHANGELOG.md >/dev/null 2>&1 || true

    git add Cargo.toml Cargo.lock CHANGELOG.md
    git commit -m "chore(release): $NEXT"
    git tag -a "$NEXT" -m "Release $NEXT"

    echo ""
    echo "Created commit and tag $NEXT."
    echo "Review with:  git log --oneline -5"
    echo "Then run:     just release"

# Push the current branch + latest tag, then create the GitLab release.
[group('release')]
release:
    #!/usr/bin/env bash
    set -euo pipefail

    TAG=$(git describe --tags --abbrev=0)

    git push origin HEAD
    git push origin "$TAG"

    NOTES=$(git-cliff --latest --strip all)

    glab release create "$TAG" \
        --name "Release $TAG" \
        --notes "$NOTES"

    echo "Released $TAG to GitLab."

# Copy the library to the archive — `storage-layout.md` §8. Dry run by default.
[group('dev')]
archive write="":
    #!/usr/bin/env bash
    set -euo pipefail

    LIB="${HOME}/MusicLibrary"
    NAS="/Volumes/Media/GMuseLibrary"

    # **The archive is the device whose holdings are complete** (§8), so this
    # mirrors rather than adds: several hundred files are *renamed* in an
    # ordinary week, rsync cannot see a rename, and without --delete the NAS
    # keeps both the new path and the old one for every one of them.
    #
    # **The log travels with the music** and both halves together: history.toml
    # is the base and events-*.log is everything since, so copying one without
    # the other silently loses recent plays and ratings.
    [ -d "$NAS" ] || { echo "not mounted: $NAS"; exit 1; }

    # **The accented paths always read as "to delete", and they are not.**
    #
    # §5 renders every name NFC and the share stores it decomposed, so
    # `readdir` hands back a decomposed spelling of a name we sent composed,
    # and rsync does not recognise a name it just wrote. Measured: 104 paths
    # flagged every run, 180 accented paths on each side, and every one of
    # them present on both —
    # the lookup is normalisation-insensitive, so nothing is actually removed.
    #
    # `--iconv` does not fix it here and makes it worse: this rsync has no
    # `UTF8-MAC` (it fails outright, which reads as a clean run because there
    # is no output to count), and `UTF-8,UTF-8-MAC` mangles 49 names into
    # paths it then reports as vanished. Left alone deliberately — a false
    # alarm in a report costs less than a translation that damages names.

    if [ "{{write}}" != "write" ]; then
        echo "dry run — pass 'write' to copy"
        { rsync -a --delete --dry-run --itemize-changes "$LIB/" "$NAS/" || [ $? -eq 24 ]; } \
          | awk '/^\*deleting/ {d++} /^[<>]f/ {s++} END {printf "  %d to send, %d to delete\n", s+0, d+0}'
        exit 0
    fi

    # **24 is a warning, not a failure**, and `set -e` treated it as one — so
    # the pass below never ran and the sync stayed as expensive as the first.
    # It means a file vanished between the file list and the transfer, which
    # here is the log being appended to while the copy runs.
    rsync -a --delete --stats "$LIB/" "$NAS/" || [ $? -eq 24 ]

    # **Then set the times ourselves, because rsync cannot on this share.**
    #
    # Measured: `touch` sets an mtime on the mount fine, and `rsync -a` — which
    # includes -t — leaves every file it *transferred* stamped with the moment
    # it was copied. A file rsync skips keeps the time it already had, so the
    # damage is exactly the set that was just written, and the next run sees
    # all of them as changed and sends them again.
    #
    # Left alone that makes every sync cost the whole library: the first run
    # here moved 34.9 GB and the second would have moved the same 34.9 GB with
    # nothing at all having changed. This pass is metadata only and takes about
    # a minute over SMB.
    python3 - "$LIB" "$NAS" <<'EOF'
    import os, sys
    lib, nas = sys.argv[1], sys.argv[2]
    fixed = 0
    for root, _, files in os.walk(lib):
        for f in files:
            src = os.path.join(root, f)
            dst = os.path.join(nas, os.path.relpath(src, lib))
            try:
                s, d = os.stat(src), os.stat(dst)
            except OSError:
                continue
            # A second of slack: SMB stores whole seconds and the source has
            # sub-second precision, so an exact comparison never matches.
            if abs(s.st_mtime - d.st_mtime) > 1:
                try:
                    os.utime(dst, (s.st_atime, s.st_mtime)); fixed += 1
                except OSError:
                    pass
    print(f"{fixed} timestamp(s) corrected, so the next sync sends only what changed")
    EOF
