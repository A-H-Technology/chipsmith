#!/usr/bin/env bash
# quartus_sta's worst paths into the register inputs and blocks the 300
# worst paths of a design rarely reach (SDATA, SLOAD, SCLR, I/O cells,
# M4K and multiplier registers, and out of them), for bench/ring/path_fit.py:
# <qor run dir>/quartus/paths-extra.txt.
#
#   bench/ring/extra-paths.sh <qor run dir> ...
set -euo pipefail
re=${CYCLONE2_RE:-/pool/dev/cyclone2-re}
for run in "$@"; do
    q="$run/xq/q/build"
    [[ -d $q ]] || { echo "$run: no Quartus project" >&2; continue; }
    out="$run/quartus/paths-extra.txt"
    rm -f "$out"
    tcl="$run/xq/extra.tcl"
    {
        echo "project_open top"; echo "create_timing_netlist"; echo "read_sdc"; echo "update_timing_netlist"
        for to in '*|sdata' '*|sload' '*|sclr' '*|datain'; do
            echo "report_timing -setup -npaths 100 -detail full_path -show_routing -to [get_pins -nowarn {$to}] -append -file $out"
        done
    } > "$tcl"
    (cd "$q" && "$re/tools/quartus.sh" quartus_sta -t "$tcl" > "$run/xq/extra.log" 2>&1) || echo "$run: quartus_sta failed" >&2
    echo "$run $(rg -c 'Data Arrival Path' "$out" 2>/dev/null || echo 0) paths"
done
