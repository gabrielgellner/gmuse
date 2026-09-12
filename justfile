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

# Show what it costs to draw a view (#121). Runs in `ci` too — it is fast.
bench-shelf:
    cargo test --release drawing_a_view -- --nocapture

# Show what it costs to rebuild the librarian (#123). Runs in `ci` too.
bench-fold:
    cargo test --release rebuilding_the_librarian -- --nocapture

# Show the resampler benchmark table (not part of `ci` — it takes ~30s)
[group('test')]
bench:
    cargo run --release --example bench_resample

# Formatting + lint + a type check, no tests
[group('test')]
check: fmt-check lint
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
