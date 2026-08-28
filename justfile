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
    cargo build --release

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

# Play one file at the line prompt (the TUI cannot start playback until M5)
[group('dev')]
play file:
    cargo run --release -- --repl "{{ file }}"

# Open the line prompt with nothing loaded; type `help` there
[group('dev')]
repl:
    cargo run --release -- --repl

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

# Rebuild the library cache from scratch, ignoring stored mtimes
[group('dev')]
rescan dir:
    echo quit | cargo run --release -- --library "{{ dir }}" --rescan --repl

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
    cargo clippy --all-targets --all-features -- -D warnings

# Run the test suite
[group('test')]
test:
    cargo test --release

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
