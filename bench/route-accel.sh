#!/usr/bin/env bash
# Route one placement with each router engine (docs/gpu-pnr.md, "Routing"):
#   bench/route-accel.sh <design> <seed> <bench dir> <mode>...
# modes: astar (the default router), serial (the batched integer router on
# one thread), cpu (on rayon's threads), cuda. The placement is made once
# per seed with the default placer, in <bench dir>/<design>/s<seed>, from
# <bench dir>/<design>/synth/packed.json (de2_synth). Prints, per mode,
# de2_pnr's route line; the trace goes to <mode>.log beside routed.json.
# Extra CHIPSMITH_ROUTE_* settings pass through the environment. Run it in
# the devshell, or put the devshell's variables in <bench dir>/env.txt
# (`nix develop --command env | grep -E '^(CHIPSMITH_|LD_LIBRARY_PATH|CUDA_PATH)'`).
set -euo pipefail
design=$1 seed=$2 bench=$3
shift 3
if [ -f "$bench/env.txt" ]; then
    set -a
    . "$bench/env.txt"
    set +a
fi
here=$(cd "$(dirname "$0")/.." && pwd)
pnr=${PNR:-$here/target/release/examples/de2_pnr}
dir=$bench/$design/s$seed
mkdir -p "$dir"
if [ ! -f "$dir/placed.json" ]; then
    cp "$bench/$design/synth/packed.json" "$dir/"
    env -u CHIPSMITH_ROUTE_ACCEL CHIPSMITH_PLACE_SEED="$seed" \
        "$pnr" --place-only "$here/examples/$design" "$dir" > "$dir/place.log" 2>&1
fi
for mode in "$@"; do
    run=$dir/$mode
    mkdir -p "$run"
    cp "$dir/placed.json" "$run/"
    case $mode in
        astar) accel=() ;;
        serial) accel=(CHIPSMITH_ROUTE_ACCEL=cpu RAYON_NUM_THREADS=1) ;;
        cpu) accel=(CHIPSMITH_ROUTE_ACCEL=cpu) ;;
        cuda | hip) accel=(CHIPSMITH_ROUTE_ACCEL=$mode) ;;
        *) echo "unknown mode $mode" >&2; exit 1 ;;
    esac
    env -u CHIPSMITH_ROUTE_ACCEL "${accel[@]}" CHIPSMITH_ROUTE_TRACE=${CHIPSMITH_ROUTE_TRACE:-1} \
        "$pnr" --route-only "$here/examples/$design" "$run" > "$run/out.log" 2> "$run/trace.log"
    echo "$design s$seed $mode: $(sed 's/.*; route/route/' "$run/out.log")"
done
