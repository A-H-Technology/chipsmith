#!/usr/bin/env bash
# Compile speed and quality of a chipsmith binary over the DE2 examples:
# wall time per stage and the numbers placement and routing are judged by.
#
#   bench/speed.sh <chipsmith binary> <out dir> [example ...]
#
# Copies examples/ into <out dir>/examples (so builds of two binaries never
# share a build dir, and picorv32x24 finds ../picorv32), builds each
# example with `chipsmith -C <copy> build` one at a time (the stages are
# timed, so builds don't run side by side), keeps each build's log as
# <out dir>/<example>.flow.log and prints one tab-separated row per
# example, also written to <out dir>/rows.tsv:
#
#   example elaborate synth pack graph place route bitstream_db bitstream
#   wall les placed_wirelength pips iterations placed_critical
#   routed_critical fmax
#
# Seconds for the stages (`route` includes its per-iteration timing
# analysis), nanoseconds for the critical paths, MHz for Fmax. An example
# that fails gets `failed` and its log. Without examples: every example
# whose manifest targets the EP2C35F672C6. Runs itself in the devshell
# when not already in it.
set -euo pipefail
export LC_ALL=C
root=$(dirname "$(dirname "$(realpath "$0")")")
if [[ -z "${CHIPSMITH_CYCLONE2_DB:-}" ]]; then
    cd "$root"
    exec nix develop "$root" --command "$0" "$@"
fi
bin=$(realpath "$1")
out=$(realpath -m "$2")
shift 2
mkdir -p "$out"
rm -rf "$out/examples"
cp -r "$root/examples" "$out/examples"
find "$out/examples" -maxdepth 2 -name build -type d -exec rm -rf {} +

if [[ $# -gt 0 ]]; then
    examples=("$@")
else
    examples=()
    for toml in "$root"/examples/*/chipsmith.toml; do
        grep -q '^device *= *"EP2C35F672C6"' "$toml" || continue
        examples+=("$(basename "$(dirname "$toml")")")
    done
fi

# The number before " s" on the line matching $2, or "-".
secs() { sed -n "/$2/s/.* in \([0-9.]*\) s.*/\1/p" "$1" | head -1 | grep . || echo -; }

: > "$out/rows.tsv"
for name in "${examples[@]}"; do
    log=$out/$name.flow.log
    t0=$(date +%s.%N)
    if "$bin" -C "$out/examples/$name" build > "$log" 2>&1; then
        status=ok
    else
        status=failed
    fi
    wall=$(awk -v a="$t0" -v b="$(date +%s.%N)" 'BEGIN { print b - a }')
    if [[ $status == failed ]]; then
        row="$name	failed	$(printf '%.1f' "$wall")"
    else
        place=$(sed -n 's/.*placed \([0-9]*\) LEs.*wirelength [0-9]* -> \([0-9]*\) in \([0-9.]*\) s; critical path \([0-9.]*\) ns.*/\1 \2 \3 \4/p' "$log")
        route=$(sed -n 's/.*routed [0-9]* nets, \([0-9]*\) pips, \([0-9]*\) iterations in \([0-9.]*\) s; critical path \([0-9.]*\) ns.*/\1 \2 \3 \4/p' "$log")
        read -r les wl place_s place_crit <<< "${place:-- - - -}"
        read -r pips iters route_s route_crit <<< "${route:-- - - -}"
        fmax=$(awk -v c="$route_crit" 'BEGIN { if (c > 0) printf "%.1f", 1000 / c; else print "-" }')
        row=$(printf '%s\t' "$name" \
            "$(secs "$log" elaborated)" "$(secs "$log" ' gates (')" "$(secs "$log" ' clocks in ')" \
            "$(secs "$log" 'routing graph in')" "$place_s" "$route_s" \
            "$(secs "$log" 'device database in')" "$(secs "$log" 'replicated-frame')" \
            "$(printf '%.1f' "$wall")" "$les" "$wl" "$pips" "$iters" "$place_crit" "$route_crit")
        row="$row$fmax"
    fi
    echo "$row" | tee -a "$out/rows.tsv"
done
