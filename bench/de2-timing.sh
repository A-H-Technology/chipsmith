#!/usr/bin/env bash
# chipsmith's timing estimate against Quartus', on every DE2 example with a
# clock.
#
#   bench/de2-timing.sh [example ...]
#
# Each example under examples/ whose chipsmith.toml targets the EP2C35F672C6
# and has a [clocks] entry is built by de2_synth (chipsmith's front end and
# synthesis, packer, placer, router, assembler), then timed by Quartus on the
# same placement and routing (bench/quartus-timing.sh). Prints one row per
# design: chipsmith's critical path, slack and Fmax; Quartus' slack, data
# delay, critical path (period - slack) and Fmax; the estimate's error
# against Quartus' critical path; whether the RBFs are byte-identical.
#
# Scratch goes to $DE2_TIMING_DIR (default /tmp/de2-timing/<example>),
# $JOBS designs at a time (default 3). Needs Quartus II 13.0sp1 through
# cyclone2-re ($CYCLONE2_RE, default ../cyclone2-re). Runs itself in the
# devshell when not already in it.
set -euo pipefail
root=$(dirname "$(dirname "$(realpath "$0")")")
if [[ -z "${CHIPSMITH_CYCLONE2_DB:-}" ]]; then
    # From the root: the devshell keeps the routing graph cache in ./target.
    cd "$root"
    exec nix develop "$root" --command "$0" "$@"
fi
export CYCLONE2_RE=${CYCLONE2_RE:-$root/../cyclone2-re}
export DE2_TIMING_DIR=${DE2_TIMING_DIR:-/tmp/de2-timing}
cd "$root"
cargo build --release --quiet --example de2_synth

if [[ $# -gt 0 ]]; then
    examples=("$@")
else
    examples=()
    for toml in examples/*/chipsmith.toml; do
        grep -q '^device *= *"EP2C35F672C6"' "$toml" || continue
        grep -q '^\[clocks\]' "$toml" || continue
        examples+=("$(basename "$(dirname "$toml")")")
    done
fi

one() {
    local name=$1 dir=$DE2_TIMING_DIR/$1
    rm -rf "$dir"
    mkdir -p "$dir"
    local t0=$SECONDS
    # The model's critical path and every connection's routing, for comparing
    # element by element with Quartus' path.txt.
    if ! CHIPSMITH_CRITICAL_PATH=1 CHIPSMITH_DUMP_CONNECTIONS="$dir/connections.txt" \
        target/release/examples/de2_synth "examples/$name" "$dir/build" > "$dir/de2_synth.log" 2>&1; then
        echo "$name build-failed"
        return
    fi
    local build_s=$((SECONDS - t0))
    if [[ ! -f "$dir/build/top.sdc" ]]; then
        echo "$name no-clock"
        return
    fi
    # "... critical path 3.39 ns, Fmax 294.9 MHz, slack 16.61 ns; ..."
    local est
    est=$(sed -n 's/.*critical path \([0-9.]*\) ns, Fmax \([0-9.]*\) MHz, slack \([-0-9.]*\) ns.*/\1 \2 \3/p' "$dir/de2_synth.log")
    local period
    period=$(sed -n 's/.*-period \([0-9.]*\) .*/\1/p' "$dir/build/top.sdc")
    local rbf
    rbf=$(ls "$dir"/build/*.rbf)
    t0=$SECONDS
    local q
    q=$(bench/quartus-timing.sh "$dir/build/routed.json" "$rbf" "$dir/quartus" "$dir/build/top.sdc" 2> "$dir/quartus.err") \
        || q="identical - slack - data_delay - skew -"
    echo "$name $period $est $q $build_s $((SECONDS - t0))"
}
export -f one

mkdir -p "$DE2_TIMING_DIR"
printf '%s\n' "${examples[@]}" | xargs -P "${JOBS:-3}" -I{} bash -c 'one {}' > "$DE2_TIMING_DIR/rows.txt"

printf '| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n' design \
    "chipsmith critical" "chipsmith slack" "chipsmith Fmax" \
    "Quartus slack" "Quartus data delay" "Quartus critical" "Quartus Fmax" \
    "error" "RBF byte-identical"
echo "|---|---|---|---|---|---|---|---|---|---|"
sort "$DE2_TIMING_DIR/rows.txt" | awk '
    NF < 12 { printf "| %s | %s |||||||||\n", $1, $2; next }
    {
        # name period crit fmax slack identical yes slack S data_delay D skew K build quartus
        name = $1; period = $2; crit = $3; fmax = $4; slack = $5
        id = $7; qs = $9; qd = $11
        if (qs == "-") { printf "| %s | %.2f ns | %.2f ns | %.1f MHz | (Quartus failed) |||||\n", name, crit, slack, fmax; next }
        qc = period - qs
        printf "| %s | %.2f ns | %.2f ns | %.1f MHz | %.2f ns | %.2f ns | %.2f ns | %.1f MHz | %+.1f%% | %s |\n",
            name, crit, slack, fmax, qs, qd, qc, 1000 / qc, 100 * (crit - qc) / qc, id
    }'
awk 'NF >= 12 { b += $(NF - 1); q += $NF } END { printf "\nbuild %d s, Quartus %d s (summed over designs)\n", b, q }' "$DE2_TIMING_DIR/rows.txt"
