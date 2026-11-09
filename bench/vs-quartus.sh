#!/usr/bin/env bash
# chipsmith against Quartus II 13.0sp1: full compiles of the same Verilog to
# a bitstream, timed per stage, with each tool's own critical path and
# quartus_sta's Fmax of both placements (docs/vs-quartus.md).
#
#   bench/vs-quartus.sh full <out dir> [design ...]
#   bench/vs-quartus.sh incr <out dir> [design ...]
#   bench/vs-quartus.sh table <out dir>
#
# `full` builds every design with every tool in $TOOLS, $REPS times,
# interleaved (rep 1: design A tool 1, A tool 2, ..., B tool 1, ...; the
# tool order rotates with the rep), each from a fresh copy of the example,
# one at a time. Tools:
#
#   q        Quartus as it comes: quartus_map, quartus_fit (Auto Fit, its
#            own NUM_PARALLEL_PROCESSORS), quartus_asm (SOF)
#   q-all    the same with NUM_PARALLEL_PROCESSORS ALL (not in the
#            default list: the Web Edition disables it, warning 20028)
#   q-fast   the same with FITTER_EFFORT "FAST FIT"
#   cs       `chipsmith build` of $CS (default: target/release/chipsmith)
#   gpu-cpu  `chipsmith build` of $CS_GPU, CHIPSMITH_ACCEL=cpu
#   gpu-cuda `chipsmith build` of $CS_GPU, CHIPSMITH_ACCEL=cuda
#
# Every run is its own `systemd-run --user --scope` (20 GB, no swap), whose
# cgroup gives the CPU time (user + sys) of everything in it, Quartus'
# sandbox included. Each stage is timed: Quartus' by running its programs
# one by one, chipsmith's by stamping every line of its report with the
# wall clock and the cgroup's CPU counter as it arrives. quartus_sta
# (Fmax summary, slow model) runs after Quartus' compile as its own stage,
# outside the compile's total; chipsmith's total includes its timing
# analysis. The 1-minute load average is sampled every 5 s during a run and
# averaged. On rep 1, each chipsmith placement is also assembled by Quartus
# (cyclone2-re's `cram quartus-export` and oracle, as bench/quartus-timing.sh)
# and timed by the same quartus_sta, untimed itself; XCHECK=0 skips that
# (picorv32x24's takes about 15 min; on 2026-10-31's routing it didn't finish in 3 h).
#
# `incr` times edit-and-rebuild: a cold build, then $REPS rebuilds, each
# after toggling one line of logic (EDIT below) on or off, so every
# rebuild sees a real change. Quartus rebuilds in the same project
# directory (its db/ and incremental_db/ kept).
#
# Rows go to <out dir>/runs.tsv (or incr.tsv), stages to stages.tsv; `table`
# prints the Markdown tables. Runs itself in the devshell when not already
# in it. CYCLONE2_RE points at a cyclone2-re checkout with tools/cram built.
set -euo pipefail
export LC_ALL=C
root=$(dirname "$(dirname "$(realpath "$0")")")
self=$(realpath "$0")
if [[ -z "${CHIPSMITH_CYCLONE2_DB:-}" ]]; then
    cd "$root"
    exec nix develop "$root" --command "$self" "$@"
fi
re=$(realpath "${CYCLONE2_RE:-/pool/dev/cyclone2-re}")
# A cram older than cyclone2-re's staleness check (b5eb9e7) doesn't know
# source-digest; a newer one refuses to run when its checkout's sources
# changed since it was built. Either way: not the export of this checkout.
"$re/tools/cram/target/release/cram" source-digest > /dev/null \
    || { echo "$re/tools/cram/target/release/cram: not built from that checkout's tools/cram; rebuild it" >&2; exit 1; }
py() { python3 "$root/bench/vs-quartus.py" "$@"; }
REPS=${REPS:-3}
read -r -a TOOLS <<< "${TOOLS:-q q-fast cs gpu-cpu gpu-cuda}"
CS=$(realpath "${CS:-$root/target/release/chipsmith}")
CS_GPU=${CS_GPU:+$(realpath "$CS_GPU")}

# A chipsmith binary runs with its own checkout's devshell databases (the
# CHIPSMITH_* paths, CUDA's): two branches may lock different
# cyclone2-re databases. <worktree>/target/release/chipsmith -> its
# variables, written once to <out dir>/env-<tool kind>.sh.
devshell_env() {
    local bin=$1 file=$2
    [[ -s $file ]] && return
    nix develop "$(dirname "$(dirname "$(dirname "$bin")")")" --command bash -c \
        'export -p | grep -E "^declare -x (CHIPSMITH_[A-Z0-9_]+|CUDA_PATH|LD_LIBRARY_PATH)="' > "$file.tmp" 2>/dev/null
    mv "$file.tmp" "$file"
}

# One line of logic per design, in its src/top.v, that `incr` toggles:
# "<line as it is>|<line edited>". The core's loop counts by 2 instead of
# 1; sha256's seed steps by 3.
declare -A EDIT
EDIT[picorv32]="2: rom = 32'h00110113; // addi x2, x2, 1|2: rom = 32'h00210113; // addi x2, x2, 2"
EDIT[picorv32x24]=${EDIT[picorv32]}
EDIT[sha256]="seed <= seed + 1'b1;|seed <= seed + 2'd3;"

# Replace the literal string $2 with $3 in file $1, exactly once.
replace_once() {
    python3 -c 'import sys
p, a, b = sys.argv[1:]
s = open(p).read()
if s.count(a) != 1:
    sys.exit(f"{p}: {a!r} found {s.count(a)} times")
open(p, "w").write(s.replace(a, b))' "$@"
}

# CPU time (µs, user + sys) of this process's cgroup.
cgroup_cpu() {
    local k v u=0 s=0
    while read -r k v; do
        case $k in user_usec) u=$v ;; system_usec) s=$v ;; esac
    done < "/sys/fs/cgroup$(cut -d: -f3 /proc/self/cgroup)/cpu.stat"
    echo "$u $s"
}

# Inside the scope: run one tool's compile in <dir>, append
# "stage wall_s user_s sys_s" lines to <dir>/stages.txt.
stage_run() {
    local tool=$1 dir=$2 name t0 c0 t1 c1
    : > "$dir/stages.txt"
    case $tool in
    q*)
        cd "$dir/q"
        for s in map fit asm sta; do
            t0=$EPOCHREALTIME c0=$(cgroup_cpu)
            if [[ $s == sta ]]; then
                "$re/tools/quartus.sh" quartus_sta -t "$dir/fmax.tcl" > sta.log 2>&1 || return 1
            else
                "$re/tools/quartus.sh" "quartus_$s" top > "$s.log" 2>&1 || return 1
            fi
            t1=$EPOCHREALTIME c1=$(cgroup_cpu)
            awk -v s="$s" -v t0="$t0" -v t1="$t1" -v c0="$c0" -v c1="$c1" 'BEGIN {
                split(c0, a, " "); split(c1, b, " ")
                printf "%s %.2f %.2f %.2f\n", s, t1 - t0, (b[1] - a[1]) / 1e6, (b[2] - a[2]) / 1e6
            }' >> "$dir/stages.txt"
        done
        ;;
    *)
        local bin=$CS envfile=$BENCH_OUT/env-cs.sh
        case $tool in
        gpu-cpu) bin=$CS_GPU envfile=$BENCH_OUT/env-gpu.sh; export CHIPSMITH_ACCEL=cpu ;;
        gpu-cuda) bin=$CS_GPU envfile=$BENCH_OUT/env-gpu.sh; export CHIPSMITH_ACCEL=cuda ;;
        esac
        # shellcheck source=/dev/null
        source "$envfile"
        name=$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$dir/src/chipsmith.toml" | head -1)
        t0=$EPOCHREALTIME
        # Each report line as "<s since start> <user µs> <sys µs> <line>".
        "$bin" -C "$dir/src" build 2>&1 | while IFS= read -r line; do
            echo "$EPOCHREALTIME $(cgroup_cpu) $line"
        done | awk -v t0="$t0" '{ $1 = sprintf("%.3f", $1 - t0); print }' > "$dir/flow.log"
        [[ ${PIPESTATUS[0]} == 0 && -f "$dir/src/build/$name.rbf" ]] || return 1
        ;;
    esac
}

# chipsmith's stages from a stamped report, each ended by its report
# line: elaborate, synth, pack, graph (loading the routing graph), place,
# route, timing (the timing report) and bitstream (to the last line).
cs_stages() {
    awk '
        function stage(s) { printf "%s %.2f %.2f %.2f\n", s, $1 - t, ($2 - u) / 1e6, ($3 - y) / 1e6; t = $1; u = $2; y = $3 }
        NR == 1 { t = 0 }
        / elaborated /                 { stage("elaborate"); next }
        / gates \(/                    { stage("synth"); next }
        / clocks in /                  { stage("pack"); next }
        /routing graph in [0-9.]+ s/   { stage("graph"); next }
        / placed [0-9]+ LEs/           { stage("place"); next }
        / routed [0-9]+ nets/          { stage("route"); next }
        / Fmax .* \(estimate\)/        { timing = 1; next }
        timing && !done                { stage("timing"); done = 1 }
        END                            { stage("bitstream") }
    ' "$1"
}

loadavg_mean() { awk '{ s += $1; n++ } END { if (n) printf "%.1f", s / n; else print "-" }' "$1"; }

# Build <design> with <tool> into <run dir>; prints a runs.tsv row.
run_one() {
    local tool=$1 design=$2 run=$3 kind=$4 status=ok
    mkdir -p "$run"
    if [[ $kind == fresh ]]; then
        rm -rf "$run"/*
        cp -r "$root/examples/$design" "$run/src"
        rm -rf "$run/src/build"
        # picorv32x24 takes its core from ../picorv32.
        sed -i "s|\"\.\./|\"$root/examples/|g" "$run/src/chipsmith.toml"
        if [[ $tool == q* ]]; then
            case $tool in q) flavour=default ;; *) flavour=${tool#q-} ;; esac
            py project "$run/src" "$run/q" "$flavour"
            cat > "$run/fmax.tcl" <<TCL
project_open top
create_timing_netlist
read_sdc
update_timing_netlist
report_clock_fmax_summary -panel_name Fmax -file $run/q/fmax.txt
TCL
        fi
    fi
    ( while :; do cut -d' ' -f1 /proc/loadavg; sleep 5; done ) > "$run/load.txt" &
    local sampler=$!
    systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0 \
        "$self" __stage "$tool" "$run" > "$run/stage.log" 2>&1 || status=failed
    kill "$sampler"; wait "$sampler" 2>/dev/null || true
    [[ $tool != q* && -f $run/flow.log ]] && cs_stages "$run/flow.log" > "$run/stages.txt"
    local wall=- cpu=- stages
    if [[ $status == ok ]]; then
        # Quartus' total leaves out quartus_sta; chipsmith's is the whole
        # build, from its start to its last report line.
        if [[ $tool == q* ]]; then
            read -r wall cpu < <(awk '$1 != "sta" { w += $2; c += $3 + $4 } END { printf "%.2f %.2f\n", w, c }' "$run/stages.txt")
        else
            read -r wall cpu < <(tail -n 1 "$run/flow.log" | awk '{ printf "%.2f %.2f\n", $1, ($2 + $3) / 1e6 }')
        fi
    fi
    stages=$(awk '{ printf "%s%s=%s/%.2f", sep, $1, $2, $3 + $4; sep = "," }' "$run/stages.txt" 2>/dev/null)
    printf '%s\t' "$design" "$tool" "$kind" "$status" "$wall" "$cpu" "$(loadavg_mean "$run/load.txt")" "${stages:--}"
}

# The quality column: key=value pairs from the tool's reports.
quality() {
    local tool=$1 run=$2
    if [[ $tool == q* ]]; then py quartus-row "$run/q"; else py chipsmith-row "$run/flow.log" "$run/xq/fmax.txt"; fi
}

# chipsmith's placement and routing through Quartus' assembler and
# quartus_sta (bench/quartus-timing.sh's steps), for one timing model.
cross_time() {
    local run=$1 name
    name=$(sed -n 's/^name *= *"\(.*\)"/\1/p' "$run/src/chipsmith.toml" | head -1)
    rm -rf "$run/xq"
    mkdir -p "$run/xq"
    py project "$run/src" "$run/xq/p" default
    "$re/tools/cram/target/release/cram" quartus-export "$run/src/build/routed.json" "$run/xq/q" > "$run/xq/export.log" 2>&1 || return 1
    cp "$run/xq/p/top.sdc" "$run/xq/q/top.sdc"
    echo "set_global_assignment -name SDC_FILE ../top.sdc" >> "$run/xq/q/assignments.qsf"
    systemd-run --user --scope -q -p MemoryMax=20G -p MemorySwapMax=0 \
        "$re/tools/oracle.sh" "$run/xq/q" > "$run/xq/oracle.log" 2>&1 || return 1
    cat > "$run/xq/fmax.tcl" <<TCL
project_open top
create_timing_netlist
read_sdc
update_timing_netlist
report_clock_fmax_summary -panel_name Fmax -file $run/xq/fmax.txt
TCL
    (cd "$run/xq/q/build" && "$re/tools/quartus.sh" quartus_sta -t "$run/xq/fmax.tcl" > "$run/xq/sta.log" 2>&1)
    if cmp -s "$run/src/build/$name.rbf" "$run/xq/q/build/output_files/top.rbf"; then
        echo identical > "$run/xq/identical"
    else
        echo differs > "$run/xq/identical"
    fi
}

# A new chipsmith binary flattens the chip database into its routing graph
# once (about a minute) and caches it next to itself; an untimed blinky
# build per chipsmith tool keeps that out of the times, as Quartus'
# installed device database is.
warm_up() {
    local out=$1 tool
    devshell_env "$CS" "$out/env-cs.sh"
    if [[ -n $CS_GPU ]]; then devshell_env "$CS_GPU" "$out/env-gpu.sh"; fi
    for tool in "${TOOLS[@]}"; do
        [[ $tool == q* ]] && continue
        run_one "$tool" blinky-verilog "$out/warm/$tool" fresh > /dev/null
    done
}

designs_or_default() {
    if [[ $# -gt 0 ]]; then printf '%s\n' "$@"; else
        # Not m4k-selftest: Quartus 13.0 infers two of its nine memories
        # (w32, c9) as registers, 16,296 LEs to chipsmith's 405.
        printf '%s\n' blinky-verilog crc32 femtorv32 picorv32 mult-selftest sdram-selftest sha256 picorv32x24
    fi
}

HEADER="design	tool	kind	status	wall	cpu	load	stages	quality"

cmd=${1:-}
case $cmd in
__stage) stage_run "$2" "$3" ;;
full)
    out=$(realpath -m "$2"); shift 2
    export BENCH_OUT=$out
    mapfile -t designs < <(designs_or_default "$@")
    mkdir -p "$out"
    [[ -s $out/runs.tsv ]] || echo "$HEADER" > "$out/runs.tsv"
    warm_up "$out"
    for rep in $(seq 1 "$REPS"); do
        for design in "${designs[@]}"; do
            n=${#TOOLS[@]}
            for i in $(seq 0 $((n - 1))); do
                tool=${TOOLS[$(((i + rep - 1) % n))]}
                run=$out/runs/$design/$tool/$rep
                [[ -f $run/done ]] && continue
                row=$(run_one "$tool" "$design" "$run" fresh)
                # The untimed cross-check comes after the timed run.
                if [[ ${XCHECK:-1} == 1 && $rep == 1 && $tool != q* && $row == *$'\tok\t'* ]]; then
                    cross_time "$run" || echo "$design $tool: Quartus cross-check failed, $run/xq" >&2
                fi
                echo "$row$(quality "$tool" "$run")" | tee -a "$out/runs.tsv"
                touch "$run/done"
            done
        done
    done
    ;;
incr)
    out=$(realpath -m "$2"); shift 2
    export BENCH_OUT=$out
    mapfile -t designs < <(designs_or_default "$@")
    mkdir -p "$out"
    [[ -s $out/incr.tsv ]] || echo "$HEADER" > "$out/incr.tsv"
    warm_up "$out"
    for design in "${designs[@]}"; do
        [[ -n ${EDIT[$design]:-} ]] || { echo "$design: no edit defined" >&2; exit 1; }
        before=${EDIT[$design]%%|*} after=${EDIT[$design]#*|}
        for tool in "${TOOLS[@]}"; do
            run=$out/incr/$design/$tool
            echo "$(run_one "$tool" "$design" "$run" fresh)$(quality "$tool" "$run")" | tee -a "$out/incr.tsv"
        done
        for rep in $(seq 1 "$REPS"); do
            for tool in "${TOOLS[@]}"; do
                run=$out/incr/$design/$tool
                # Quartus' copy of top.v with the JTAG pins, if it has one.
                for f in "$run/src/src/top.v" "$run/q/top.v"; do
                    [[ -f $f ]] || continue
                    if (( rep % 2 )); then
                        replace_once "$f" "$before" "$after"
                    else
                        replace_once "$f" "$after" "$before"
                    fi
                done
                echo "$(run_one "$tool" "$design" "$run" "edit$rep")$(quality "$tool" "$run")" | tee -a "$out/incr.tsv"
            done
        done
    done
    ;;
table)
    py table "$2/runs.tsv" fresh
    if [[ -f $2/incr.tsv ]]; then echo; py table "$2/incr.tsv" fresh; echo; py table "$2/incr.tsv" edit; fi
    ;;
*)
    sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'
    exit 1
    ;;
esac
