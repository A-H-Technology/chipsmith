#!/usr/bin/env bash
# Check chipsmith's packer against the nextpnr one it was ported from.
#
#   bench/pack-equiv.sh <dir>...
#
# Each dir holds a yosys netlist (netlist.json) and its pins (pins.qsf, which
# only nextpnr reads). Packs it with `nextpnr-himbaechel --pack-only` (the
# cyclone2-re uarch; NEXTPNR overrides the binary, e.g. from
# `nix build github:cramt/cyclone2-re#nextpnr-cyclone2`) and with
# `chipsmith pack`, then compares cells, parameters, connectivity and net
# id order (`chipsmith pack-diff`).
set -euo pipefail
nextpnr=${NEXTPNR:-nextpnr-himbaechel}
chipsmith=${CHIPSMITH:-$(dirname "$(realpath "$0")")/../target/release/chipsmith}
status=0
for dir in "$@"; do
  "$nextpnr" --device EP2C35F672C6 --json "$dir/netlist.json" --pack-only \
    --write "$dir/nextpnr.json" -o "qsf=$dir/pins.qsf" --log "$dir/nextpnr.log" -q
  "$chipsmith" pack "$dir/netlist.json" "$dir/packed.json" 2> /dev/null
  out=$("$chipsmith" pack-diff "$dir/packed.json" "$dir/nextpnr.json" 2>&1) || status=1
  echo "$(basename "$dir"): $out"
done
exit $status
