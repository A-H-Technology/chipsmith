#!/usr/bin/env bash
# Build a ring-oscillator spec (bench/ring/ring.py), load it on the DE2 and
# read every ring's frequency, WINDOWS times each.
#
#   bench/ring/run.sh <spec.json> [scratch dir]
#
# Prints `ring <k> <count> <MHz>` lines (de2_ring) and leaves the routed
# connections (CHIPSMITH_DUMP_CONNECTIONS) in the scratch dir. Runs itself in
# the devshell.
set -euo pipefail
root=$(dirname "$(dirname "$(dirname "$(realpath "$0")")")")
if [[ -z "${CHIPSMITH_CYCLONE2_DB:-}" ]]; then
    cd "$root"
    exec nix develop "$root" --command "$0" "$@"
fi
spec=$(realpath "$1")
dir=${2:-/tmp/ring/$(basename "$spec" .json)}
cd "$root"
cargo build --release --quiet --example de2_synth --example de2_pnr -p chipsmith
cargo build --release --quiet --example de2_ring -p chipsmith-cyclone2
cargo build --release --quiet --bin chipsmith
rm -rf "$dir"
mkdir -p "$dir"
python3 bench/ring/ring.py gen "$spec" "$dir/project"
target/release/examples/de2_synth "$dir/project" "$dir/build" > "$dir/synth.log" 2>&1 \
    || { tail -20 "$dir/synth.log"; exit 1; }
python3 bench/ring/ring.py place "$spec" "$dir/build/placed.json"
CHIPSMITH_DUMP_CONNECTIONS="$dir/connections.txt" \
    target/release/examples/de2_pnr --route-only "$dir/project" "$dir/build" > "$dir/route.log" 2>&1 \
    || { tail -20 "$dir/route.log"; exit 1; }
target/release/chipsmith -C "$dir/project" flash "$dir/build/ring.rbf" > "$dir/flash.log" 2>&1 \
    || { tail -20 "$dir/flash.log"; exit 1; }
rings=$(python3 -c "import json; print(len(json.load(open('$spec'))))")
window=$(python3 -c "import json; print(json.load(open('$dir/project/ring.json'))['window'])")
target/release/examples/de2_ring "$rings" "$window" "${WINDOWS:-5}" | tee "$dir/counts.txt"
