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
# where sta is 1000 / (20 - slack): every example here has a 50 MHz
# CLOCK_50 and nothing else (sdram-selftest's PLL clocks aside; its
# worst path is reported against the clock it ends on). Needs the
# devshell (CHIPSMITH_CYCLONE2_DB) and CYCLONE2_RE as quartus-timing.sh.
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
        est=$(sed -n 's/.*clock CLOCK_50 .*Fmax \([0-9.]*\) MHz.*/\1/p' "$dir/build.log" | tail -1)
        printf 'create_clock -name {CLOCK_50} -period 20 [get_ports {CLOCK_50}]\n' > "$dir/top.sdc"
        if grep -q CY2_PLL "$dir"/src/*.v 2>/dev/null; then
            echo "derive_pll_clocks" >> "$dir/top.sdc"
        fi
        rbf=$(ls "$dir"/build/*.rbf)
        q=$("$root/bench/quartus-timing.sh" "$dir/build/routed.json" "$rbf" "$dir/quartus" "$dir/top.sdc" 2> "$dir/quartus.err") \
            || q="identical - slack -"
        read -r _ id _ slack _ <<< "$q"
        sta=-
        [[ $slack != - ]] && sta=$(awk -v s="$slack" 'BEGIN { printf "%.2f", 1000 / (20 - s) }')
        echo "$name seed $seed les $les est $est identical $id sta $sta"
    done
done
