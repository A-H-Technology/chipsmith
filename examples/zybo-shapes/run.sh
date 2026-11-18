#!/usr/bin/env bash
# The shape benches on the Zybo: Verilator's signatures for src/top.v, then
# LOADS loads of the board over JTAG (default 3), each read back from USER1
# and compared shape by shape. Run from anywhere, inside chipsmith's
# `nix develop` (cargo, libusb); Verilator comes from nixpkgs.
#
#   examples/zybo-shapes/run.sh [LOADS]
set -euo pipefail
cd "$(dirname "$(realpath "$0")")"
loads=${1:-3}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

python3 gen.py | diff -q - src/top.v >/dev/null || { echo "src/top.v is stale: ./gen.py > src/top.v" >&2; exit 1; }
d=../../crates/chipsmith-xc7/tests/designs
nix shell nixpkgs#verilator nixpkgs#gcc nixpkgs#gnumake --command \
    verilator --cc --exe --build -O3 -j 8 -Wno-fatal -Wno-lint -Wno-style \
    --x-assign 0 --x-initial 0 --public-flat-rw --top-module top -Mdir "$work/v" \
    src/top.v "$d"/bram/*.v "$d"/dsp/*.v sim.cpp -o vsim >"$work/verilator.log" 2>&1 \
    || { cat "$work/verilator.log" >&2; exit 1; }
"$work/v/vsim" >"$work/expected"

read -ra names <<<"tag cycles $(sed -n 6p src/top.v | sed 's#//##; s/,//g')"
cargo="cargo run -q --release --manifest-path ../../Cargo.toml"
fails=0
for i in $(seq 1 "$loads"); do
    $cargo -p chipsmith -- flash >"$work/flash.log" 2>&1 || { cat "$work/flash.log" >&2; exit 1; }
    sleep 1  # 2^20 cycles at 125 MHz is 8.4 ms
    $cargo -p chipsmith-jtag --example xc7_dr -- 704 >"$work/board"
    bad=$(paste "$work/expected" "$work/board" | awk -v n="${names[*]}" \
        'BEGIN { split(n, a, " ") } $1 != $2 { printf "%s(%s, want %s) ", a[NR], $2, $1 }')
    echo "load $i: ${bad:-all ${#names[@]} words match}"
    [[ -z $bad ]] || fails=$((fails + 1))
done
echo "$fails of $loads loads wrong"
[[ $fails -eq 0 ]]
