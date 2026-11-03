#!/usr/bin/env bash
# chipsmith's timing estimate against Vivado's, on chipsmith's own placement
# and routing of Zybo examples.
#
#   bench/zybo-timing.sh <example> ...
#
# Each example is built by chipsmith (CHIPSMITH_XC7_ENDPOINTS writing its
# estimate per path end), its bitstream written out as Vivado input by the
# xc7_vivado example (netlist, LOC/BEL, LOCK_PINS, every route as
# FIXED_ROUTE), and timed by Vivado through xc7-re's tools/import.sh. Then
# bench/zybo-timing.py holds the two endpoint by endpoint. Prints per
# example chipsmith's Fmax estimate and Vivado's (8 ns clock: 1 / (8 ns -
# WNS)), and the endpoint errors.
#
# Scratch goes to $ZYBO_TIMING_DIR (default /tmp/zybo-timing/<example>, or
# <example>-s<seed> per seed). $ZYBO_SEEDS (e.g. "0 1 2 3 4") builds each
# example once per placer seed (CHIPSMITH_XC7_SEED).
# Needs Vivado through xc7-re ($XC7_RE, default ../xc7-re). Runs itself in
# the devshell when not already in it.
set -euo pipefail
root=$(dirname "$(dirname "$(realpath "$0")")")
if [[ -z "${CHIPSMITH_PRJXRAY_DB:-}" ]]; then
    cd "$root"
    exec nix develop "$root" --command "$0" "$@"
fi
xc7_re=${XC7_RE:-$root/../xc7-re}
[[ -x $xc7_re/tools/import.sh ]] || xc7_re=/pool/dev/xc7-re
out=${ZYBO_TIMING_DIR:-/tmp/zybo-timing}
cd "$root"
# $ZYBO_TIMING_BIN: a directory holding chipsmith and xc7_vivado to run
# instead of this tree's (another build to compare).
if [[ -n "${ZYBO_TIMING_BIN:-}" ]]; then
    bin=$ZYBO_TIMING_BIN
else
    cargo build --release --quiet -p chipsmith --bin chipsmith --example xc7_vivado
    # Copies, so a rebuild during the run doesn't change what it measures.
    bin=$(mktemp -d "${TMPDIR:-/tmp}/zybo-timing-bin.XXXXXX")
    cp target/release/chipsmith target/release/examples/xc7_vivado "$bin/"
fi

dirs=()
seeds=${ZYBO_SEEDS:-0}
for name in "$@"; do
  for seed in $seeds; do
    s=$out/$name
    [[ $seeds == 0 ]] || s=$out/$name-s$seed
    rm -rf "$s"
    mkdir -p "$s/examples"
    cp -r "examples/$name" "$s/examples/"
    rm -rf "$s/examples/$name/build"
    for sib in $(grep -o '"\.\./[^/"]*' "examples/$name/chipsmith.toml" | cut -c5- | sort -u); do
        cp -r "examples/$sib" "$s/examples/"
    done
    CHIPSMITH_XC7_SEED=$seed CHIPSMITH_XC7_ENDPOINTS=$s/chipsmith.tsv systemd-run --user --scope -q -p MemoryMax=12G -p MemorySwapMax=0 \
        "$bin/chipsmith" -C "$s/examples/$name" build > "$s/build.log" 2>&1 \
        || { tail -5 "$s/build.log"; exit 1; }
    # The slowest clock from a pin (a BSCAN's TCK isn't exported).
    est=$(grep -E '^ *clock [^ .]+: ' "$s/build.log" | grep -o 'Fmax [0-9.]* MHz' | sort -k2 -n | head -1)
    "$bin/xc7_vivado" "$s/examples/$name" "$(ls "$s/examples/$name"/build/*.fasm)" "$s" 2> "$s/export.log" \
        || { tail -5 "$s/export.log"; exit 1; }
    systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0 "$xc7_re/tools/import.sh" "$s" > "$s/import.log" 2>&1 \
        || { tail -5 "$s/import.log"; exit 1; }
    wns=$(grep -A8 '^| Design Timing Summary' "$s/timing_summary.rpt" | tail -3 | head -1 | awk '{print $1}')
    viv=$(python3 -c "print(f'{1000 / (8 - $wns):.1f}')")
    echo "$name seed $seed: chipsmith ${est:-no estimate}, Vivado on chipsmith's routing Fmax $viv MHz (WNS $wns ns); $(head -1 "$s/import.txt")"
    dirs+=("$s")
  done
done
python3 bench/zybo-timing.py "${dirs[@]}"
