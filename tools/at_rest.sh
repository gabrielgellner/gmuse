#!/usr/bin/env bash
# What the running gmuse processes cost — CPU over a window, and physical footprint (#255).
#
# `ps` RSS is not this program's memory: it counts the dyld shared cache in every process and it
# tracks the peak, not what is held. `footprint` reports the physical footprint, which is the
# number the kernel charges. CPU is a delta of `cputime` over the window, so a startup scan that
# happens to overlap the window shows up as what it is — a 65% burst — rather than as a lifetime
# average that hides it.
#
# Usage: tools/at_rest.sh [seconds]      (default 20)
#
# Samples every `gmuse` process it finds and insists there is at least one, because a
# measurement that reports nothing and exits 0 is the failure `just _bench` exists to catch.
set -euo pipefail

secs=${1:-20}
pids=$(pgrep -x gmuse || true)
if [ -z "$pids" ]; then
    echo "at-rest: no gmuse process is running — start an engine, or a view, or both, then measure." >&2
    exit 1
fi

# cputime as hundredths of a second, so the delta is exact.
hundredths() {
    ps -o cputime= -p "$1" | awk -F'[:.]' '{print ($1 * 60 + $2) * 100 + $3}'
}

declare -A t0
for p in $pids; do t0[$p]=$(hundredths "$p"); done
sleep "$secs"

printf "%-8s %8s   %-12s  %s\n" pid "cpu" "footprint" "command"
for p in $pids; do
    t1=$(hundredths "$p")
    cpu=$(awk -v a="${t0[$p]}" -v b="$t1" -v s="$secs" 'BEGIN { printf "%.2f%%", (b - a) / s }')
    if command -v footprint >/dev/null; then
        fp=$(footprint -p "$p" 2>/dev/null \
            | awk '/Footprint: /{ for (i = 1; i <= NF; i++) if ($i == "Footprint:") { print $(i+1) " " $(i+2); exit } }')
    else
        fp="n/a"
    fi
    cmd=$(ps -o args= -p "$p" | cut -c1-60)
    printf "%-8s %8s   %-12s  %s\n" "$p" "$cpu" "${fp:-gone}" "$cmd"
done
echo "(${secs}s window; cpu is % of one core)"
