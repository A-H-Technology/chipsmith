#!/usr/bin/env bash
# Have Quartus time a chipsmith-routed design, for checking the timing model.
#
#   bench/quartus-timing.sh <routed.json> <rbf> <work dir> [sdc]
#
# <routed.json> and <rbf> are a chipsmith build's routing and bitstream
# (de2_synth's scratch dir has both, and top.sdc when the manifest names a
# clock). The cyclone2-re oracle (Quartus II 13.0sp1) assembles the same
# placement and routing; the RBFs are compared, and Quartus' worst setup path
# is reported. Prints one line,
#
#   identical yes|no slack <ns> data_delay <ns> skew <ns>
#
# and leaves the full critical path (cells and routing elements) in
# <work dir>/path.txt (a diff of the RBFs in compare.log when they differ).
# CYCLONE2_RE points at a cyclone2-re checkout with
# tools/cram built.
set -euo pipefail
re=${CYCLONE2_RE:-$(dirname "$(realpath "$0")")/../../cyclone2-re}
routed=$(realpath "$1")
rbf=$(realpath "$2")
work=$(realpath -m "$3")
rm -rf "$work"
mkdir -p "$work/build"
cp "$routed" "$work/build/routed.json"
cp "$rbf" "$work/build/top.rbf"
cram="$re/tools/cram/target/release/cram"
# tools/validate.sh's steps, less back-annotation and the simulation netlist,
# which timing doesn't need (they're half its run time).
q="$work/build/q"
"$cram" quartus-export "$work/build/routed.json" "$q" > "$work/export.log" 2>&1
if [[ -n "${4:-}" ]]; then
    cp "$4" "$q/top.sdc"
    echo "set_global_assignment -name SDC_FILE ../top.sdc" >> "$q/assignments.qsf"
fi
"$re/tools/oracle.sh" "$q" > "$work/oracle.log" 2>&1 || { tail -30 "$work/oracle.log" >&2; exit 1; }
identical=no
if cmp -s "$work/build/top.rbf" "$q/build/output_files/top.rbf"; then
    identical=yes
else
    "$cram" compare "$work/build/top.rbf" "$q/build/output_files/top.rbf" > "$work/compare.log" 2>&1 || true
fi
cat > "$work/path.tcl" <<TCL
project_open top
create_timing_netlist
read_sdc
update_timing_netlist
report_timing -setup -npaths 1 -detail summary -file $work/summary.txt
report_timing -setup -npaths 1 -detail full_path -file $work/path.txt
TCL
(cd "$q/build" && "$re/tools/quartus.sh" quartus_sta -t "$work/path.tcl" > "$work/sta.log" 2>&1)
# Summary columns: slack, from, to, launch, latch, relationship, skew, data delay.
awk -F';' -v id="$identical" '/^; [-0-9.]+ *;/ && !done {
    gsub(/ /, ""); print "identical " id " slack " $2 " data_delay " $9 " skew " $8; done = 1
} END { if (!done) print "identical " id " slack - data_delay - skew -" }' "$work/summary.txt"
