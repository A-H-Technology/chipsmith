#!/usr/bin/env bash
# Have Quartus time a chipsmith-routed design, for checking the timing model.
#
#   bench/quartus-timing.sh <routed.json> <work dir> [sdc]
#
# Packs the routed design, has the cyclone2-re oracle assemble the same
# placement and routing (which needs Quartus II 13.0sp1 installed), checks the
# RBFs are byte-identical, and prints Quartus' worst setup slack and the
# critical path's data delay. CYCLONE2_RE points at a cyclone2-re checkout.
set -euo pipefail
re=${CYCLONE2_RE:-$(dirname "$(realpath "$0")")/../../cyclone2-re}
routed=$(realpath "$1")
work=$(realpath -m "$2")
rm -rf "$work"
mkdir -p "$work/build"
cp "$routed" "$work/build/routed.json"
[[ -n "${3:-}" ]] && cp "$3" "$work/top.sdc"
"$re/tools/cram/target/release/cram" pack - "$work/build/routed.json" "$work/build/top.rbf" > /dev/null
"$re/tools/validate.sh" "$work" > "$work/validate.log" 2>&1
grep -q "BYTE-IDENTICAL" "$work/validate.log" || { echo "not byte-identical; see $work/validate.log"; exit 1; }
cat > "$work/path.tcl" <<TCL
project_open top
create_timing_netlist
read_sdc
update_timing_netlist
report_timing -setup -npaths 1 -detail summary -file $work/path.txt
TCL
(cd "$work/build/q/build" && "$re/tools/quartus.sh" quartus_sta -t "$work/path.tcl" > "$work/sta.log" 2>&1)
awk -F';' '/^; [-0-9.]+ *;/ && !done { gsub(/ /,""); print "slack " $2 " ns, data delay " $9 " ns"; done=1 }' "$work/path.txt"
