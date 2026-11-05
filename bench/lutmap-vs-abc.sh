#!/usr/bin/env bash
# chipsmith's LUT mapper against yosys' abc on the same gates.
#
#   bench/lutmap-vs-abc.sh <dir>
#
# Reads <dir>/gates.json, yosys' gates before LUT mapping, as
# `cargo run --release --example de2_synth -- --yosys <project> <dir>`
# writes them (the build maps chipsmith's own gates in memory), and
# prints the LUT count after `opt -fast` (which drops abc's buffer LUTs)
# for: chipsmith, abc's mapper alone (`strash; if -K 4`), and abc's full
# `abc -lut 4` script (with logic optimisation: choices, mfs2, lutpack).
# Run inside `nix develop` (yosys); CHIPSMITH names the binary.
set -euo pipefail
build=$(realpath "$1")
chipsmith=${CHIPSMITH:-$(dirname "$(realpath "$0")")/../target/release/chipsmith}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

"$chipsmith" lutmap "$build/gates.json" "$tmp/chipsmith.json" 2> /dev/null

# The cell library stays as gates.json's parameterless blackbox stubs: enough
# for mapping and counting.
luts() { # <label> <netlist> [abc command]
  yosys -q -p "read_json $2; hierarchy -top top; ${3:-}; opt -fast; tee -q -o $tmp/stat.txt stat" > /dev/null
  printf '%-28s %s LUTs\n' "$1" "$(awk '$2 == "$lut" { print $1 }' "$tmp/stat.txt")"
}
luts "chipsmith" "$tmp/chipsmith.json"
luts "abc mapper (if -K 4)" "$build/gates.json" "abc -script +strash;if,-K,4"
luts "abc -lut 4 (full script)" "$build/gates.json" "abc -lut 4"
