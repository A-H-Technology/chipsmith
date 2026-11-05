#!/usr/bin/env bash
# One chipsmith binary on DE2 examples over several placement seeds, each
# placement timed by Quartus (bench/quartus-timing.sh): for comparing a
# change's binary against the one before it.
#
#   bench/fmax-seeds.sh <chipsmith binary> <work dir> <seeds> <example>...
#
# <seeds> is a comma list (0,1,2). Prints one line per example and seed,
#
#   <example> seed <s> les <n> est <MHz> identical yes|no sta <MHz>
#
# est is the slowest constrained clock's estimated Fmax, and sta is
# quartus_sta's worst setup path's, 1000 / (relationship - slack): every
# example here has a 50 MHz CLOCK_50, and PLL clocks are derived from it
# (sdram-selftest's worst path is on its 100 MHz one). Needs the devshell
# (CHIPSMITH_CYCLONE2_DB) and CYCLONE2_RE as quartus-timing.sh.
set -euo pipefail
root=$(dirname "$(dirname "$(realpath "$0")")")
bin=$(realpath "$1")
work=$(realpath -m "$2")
IFS=, read -ra seeds <<< "$3"
shift 3
mkdir -p "$work"
for name in "$@"; do
    for seed in "${seeds[@]}"; do
        dir=$work/$name-$seed
        rm -rf "$dir"
        cp -r "$root/examples/$name" "$dir"
        rm -rf "$dir/build"
        if ! CHIPSMITH_PLACE_SEED=$seed "$bin" -C "$dir" build > "$dir/build.log" 2>&1; then
            echo "$name seed $seed build-failed"
            continue
        fi
        les=$(sed -n 's/^ *\([0-9]*\) LEs, .*pins.*/\1/p' "$dir/build.log" | tail -1)
        est=$(sed -n 's/.*clock .*Fmax \([0-9.]*\) MHz, slack.*/\1/p' "$dir/build.log" | sort -g | head -1)
        printf 'create_clock -name {CLOCK_50} -period 20 [get_ports {CLOCK_50}]\n' > "$dir/top.sdc"
        if grep -q CY2_PLL "$dir"/src/*.v 2>/dev/null; then
            echo "derive_pll_clocks" >> "$dir/top.sdc"
        fi
        rbf=$(ls "$dir"/build/*.rbf)
        q=$("$root/bench/quartus-timing.sh" "$dir/build/routed.json" "$rbf" "$dir/quartus" "$dir/top.sdc" 2> "$dir/quartus.err") \
            || q="identical - slack -"
        read -r _ id _ _ <<< "$q"
        # Summary columns: slack, from, to, launch, latch, relationship, ...
        sta=$(awk -F';' '/^; [-0-9.]+ *;/ { printf "%.2f", 1000 / ($7 - $2); exit }' "$dir/quartus/summary.txt" 2>/dev/null)
        sta=${sta:--}
        echo "$name seed $seed les $les est $est identical $id sta $sta"
    done
done
