#!/usr/bin/env bash
# The DE25 router's A* weight against routing time and the delay model's
# Fmax: each design built once per weight, its routing time, iterations and
# CLOCK1_50 critical path printed.
#
#   bench/de25-astar.sh "2 4 6 8" examples/de25-lw examples/de25-burst ...
#
# A design is a project directory (copied to scratch, built from clean).
# Runs itself in the devshell when not already in it.
set -euo pipefail
root=$(dirname "$(dirname "$(realpath "$0")")")
if [[ -z "${CHIPSMITH_AGILEX5_DB:-}" ]]; then
    cd "$root"
    exec nix develop "$root" --command "$0" "$@"
fi
weights=$1; shift
cd "$root"
cargo build --release --quiet -p chipsmith --bin chipsmith
bin=$(mktemp -d)
cp target/release/chipsmith "$bin/"
printf 'design\tweight\troute_s\titerations\tcritical_ns\n'
for d in "$@"; do
  for w in $weights; do
    s=$(mktemp -d)
    cp -r "$d/." "$s/"
    rm -rf "$s/build"
    log=$(CHIPSMITH_CACHE=0 CHIPSMITH_A5_ASTAR=$w "$bin/chipsmith" -C "$s" build 2>&1 || true)
    # || true: a failed build has no route line, and that's a row (fail),
    # not the end of the sweep.
    r=$(grep -o 'route: [0-9]* nets, [0-9]* hops, [0-9]* iterations, [0-9.]* s' <<<"$log" | tail -1 || true)
    it=$(awk '{print $6}' <<<"$r"); t=$(awk '{print $8}' <<<"$r")
    ns=$(grep -o 'clock [^:]*: [0-9.]* ns' <<<"$log" | head -1 | awk '{print $3}' || true)
    printf '%s\t%s\t%s\t%s\t%s\n' "$(basename "$d")" "$w" "${t:-fail}" "${it:-}" "${ns:-}"
    rm -rf "$s"
  done
done
