#!/usr/bin/env bash
# de25-f2h on the board: load the core over ssh, and only if the FPGA
# manager then says `operating`, run f2htest (16 pages both ways, stream 11)
# RUNS times (default 3). f2htest's first access is a lightweight-bridge
# read, and with no core answering that hangs the HPS bus until a reset, so
# a failed or unconfirmed load stops here. Run inside chipsmith's
# `nix develop`.
#
#   examples/de25-f2h/run.sh [HOST [RUNS]]
set -euo pipefail
cd "$(dirname "$(realpath "$0")")"
host=${1:-mercury}
runs=${2:-3}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

nix shell nixpkgs#zig --command zig cc -target aarch64-linux-musl -static -O2 f2htest.c -o "$work/f2htest"
cargo run -q --release --manifest-path ../../Cargo.toml -p chipsmith -- flash --ssh "$host"
state=$(ssh "$host" cat /sys/class/fpga_manager/fpga0/state)
if [[ $state != operating ]]; then
    echo "FPGA manager says '$state' after the load: not touching the bridges" >&2
    exit 1
fi
scp -q "$work/f2htest" "$host:/tmp/f2htest"
for i in $(seq 1 "$runs"); do
    printf "run %d: " "$i"
    ssh "$host" sudo /tmp/f2htest | tail -1
done
