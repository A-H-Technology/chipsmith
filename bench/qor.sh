#!/usr/bin/env bash
# Quality of results of one chipsmith binary: LEs and Fmax, the latter
# both chipsmith's estimate and quartus_sta's on the same placement.
#
#   bench/qor.sh <out dir> [design ...]
#
# Builds each example from a fresh copy with $CS (default:
# target/release/chipsmith), then has Quartus assemble and time that
# placement and routing (cyclone2-re's `cram quartus-export` and oracle,
# then quartus_sta's Fmax summary, slow model, as bench/vs-quartus.sh's
# cross-check does). Prints one line per design:
#
#   design LEs <n> estimate <MHz> quartus_sta <MHz> rbf identical|differs
#
# and appends it to <out dir>/qor.tsv. bench/qor-stats.py breaks a
# design's LEs down (<out dir>/<design>/src/build/packed.json). XCHECK=0
# skips Quartus. CALIB=1 also keeps what bench/timing-calib.py reads
# (`bench/timing-calib.py <out dir>/<design>` per design): every
# connection's modelled delay (connections.txt) and quartus_sta's 300
# worst paths with their routing (quartus/paths.txt). Runs itself in the
# devshell when not already in it.
# CYCLONE2_RE points at a cyclone2-re checkout with tools/cram built.
set -euo pipefail
export LC_ALL=C
root=$(dirname "$(dirname "$(realpath "$0")")")
self=$(realpath "$0")
if [[ -z "${CHIPSMITH_CYCLONE2_DB:-}" ]]; then
    cd "$root"
    exec nix develop "$root" --command "$self" "$@"
fi
re=$(realpath "${CYCLONE2_RE:-/pool/dev/cyclone2-re}")
CS=$(realpath "${CS:-$root/target/release/chipsmith}")
out=$(realpath -m "$1")
shift
designs=("$@")
[[ ${#designs[@]} -gt 0 ]] || designs=(femtorv32 picorv32 sha256 mult-selftest sdram-selftest)
mkdir -p "$out"

for design in "${designs[@]}"; do
    run="$out/$design"
    rm -rf "$run"
    mkdir -p "$run"
    cp -r "$root/examples/$design" "$run/src"
    rm -rf "$run/src/build"
    # picorv32x24 takes its core from ../picorv32.
    sed -i "s|\"\.\./|\"$root/examples/|g" "$run/src/chipsmith.toml"
    name=$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$run/src/chipsmith.toml" | head -1)
    dump=()
    [[ ${CALIB:-0} == 1 ]] && dump=(env "CHIPSMITH_DUMP_CONNECTIONS=$run/connections.txt")
    if ! (cd "$run/src" && systemd-run --user --scope -q -p MemoryMax=12G -p MemorySwapMax=0 \
        "${dump[@]}" "$CS" build) > "$run/flow.log" 2>&1; then
        printf '%s build failed (%s)\n' "$design" "$run/flow.log" | tee -a "$out/qor.tsv"
        continue
    fi
    les=$(sed -n 's/.*placed \([0-9]*\) LEs.*/\1/p' "$run/flow.log" | tail -1)
    est=$(sed -n "s/.*Fmax \([0-9.]*\) MHz.*/\1/p" "$run/flow.log" | sort -g | head -1)
    sta=- identical=-
    if [[ ${XCHECK:-1} != 0 ]]; then
        x="$run/xq"
        mkdir -p "$x"
        python3 "$root/bench/vs-quartus.py" project "$run/src" "$x/p" default
        if "$re/tools/cram/target/release/cram" quartus-export "$run/src/build/routed.json" "$x/q" \
            > "$x/export.log" 2>&1; then
            cp "$x/p/top.sdc" "$x/q/top.sdc"
            echo "set_global_assignment -name SDC_FILE ../top.sdc" >> "$x/q/assignments.qsf"
            if systemd-run --user --scope -q -p MemoryMax=12G -p MemorySwapMax=0 \
                "$re/tools/oracle.sh" "$x/q" > "$x/oracle.log" 2>&1; then
                cat > "$x/fmax.tcl" <<TCL
project_open top
create_timing_netlist
read_sdc
update_timing_netlist
report_clock_fmax_summary -panel_name Fmax -file $x/fmax.txt
TCL
                if [[ ${CALIB:-0} == 1 ]]; then
                    mkdir -p "$run/quartus"
                    echo "report_timing -setup -npaths 300 -detail full_path -show_routing -file $run/quartus/paths.txt" >> "$x/fmax.tcl"
                    ln -sfn src/build "$run/build"
                fi
                (cd "$x/q/build" && "$re/tools/quartus.sh" quartus_sta -t "$x/fmax.tcl" > "$x/sta.log" 2>&1) || true
                # The slowest clock's Fmax (the first row is the slowest
                # only by chance; take the minimum).
                sta=$(awk -F';' '/MHz/ { gsub(/ |MHz/, "", $2); print $2 }' "$x/fmax.txt" 2>/dev/null \
                    | sort -g | head -1)
                sta=${sta:--}
                if cmp -s "$run/src/build/$name.rbf" "$x/q/build/output_files/top.rbf"; then
                    identical=identical
                else
                    identical=differs
                fi
            else
                identical="oracle-failed"
            fi
        else
            identical="export-failed"
        fi
    fi
    printf '%s LEs %s estimate %s quartus_sta %s rbf %s\n' "$design" "$les" "$est" "$sta" "$identical" \
        | tee -a "$out/qor.tsv"
done
