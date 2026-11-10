#!/usr/bin/env bash
# The open build of a ring spec (RING_OPEN=1 bench/ring/ring.py: same
# placement, stage 0 fed by a register, no loop) timed by quartus_sta on
# chipsmith's placement and routing: per ring, report_path's delay from the
# register to the last stage's output, as `RING <k> <register> <ns>` lines
# in <dir>/sta.txt, next to the build's connections.txt.
#
#   bench/ring/sta.sh <spec.json> <dir>
set -euo pipefail
root=$(dirname "$(dirname "$(dirname "$(realpath "$0")")")")
if [[ -z "${CHIPSMITH_CYCLONE2_DB:-}" ]]; then
    cd "$root"
    exec nix develop "$root" --command "$0" "$@"
fi
export CYCLONE2_RE=${CYCLONE2_RE:-/pool/dev/cyclone2-re}
spec=$(realpath "$1")
dir=$(realpath -m "$2")
cd "$root"
rm -rf "$dir"
mkdir -p "$dir"
RING_OPEN=1 python3 bench/ring/ring.py gen "$spec" "$dir/project"
target/release/examples/de2_synth "$dir/project" "$dir/build" > "$dir/synth.log" 2>&1 \
    || { tail -20 "$dir/synth.log"; exit 1; }
python3 bench/ring/ring.py place "$spec" "$dir/build/placed.json"
CHIPSMITH_DUMP_CONNECTIONS="$dir/connections.txt" \
    target/release/examples/de2_pnr --route-only "$dir/project" "$dir/build" > "$dir/route.log" 2>&1 \
    || { tail -20 "$dir/route.log"; exit 1; }
bench/quartus-timing.sh "$dir/build/routed.json" "$dir/build/ring.rbf" "$dir/quartus" "$dir/build/top.sdc" \
    > "$dir/quartus.log" 2>&1 || { tail -20 "$dir/quartus.log"; exit 1; }
python3 bench/ring/ring.py sta-tcl "$spec" "$dir/build/routed.json" "$dir/paths.tcl"
(cd "$dir/quartus/build/q/build" && "$CYCLONE2_RE/tools/quartus.sh" quartus_sta -t "$dir/paths.tcl") \
    | rg '^RING ' > "$dir/sta.txt"
cp "$spec" "$dir/spec.json"
cat "$dir/sta.txt"
