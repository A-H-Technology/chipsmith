#!/usr/bin/env bash
# Run a JTAG self-test example on the DE2 from a PLL at a chosen frequency
# (e.g. chipsmith's own Fmax estimate for it), and read its passes and
# errors twice, a second apart.
#
#   bench/de2-at-fmax.sh <example> <multiply> <divide> [scratch dir]
#
# The example's `wire clk = CLOCK_50;` becomes PLL_1's CLK0 at 50 MHz x
# multiply / divide (examples/mult-selftest, m4k-selftest: USER1 holds a
# tag, the passes in the top 16 bits of the count de2_user1 prints and the
# errors in the bottom 16). Prints chipsmith's estimate for the build and
# de2_user1's two reads. Runs itself in the devshell.
set -euo pipefail
root=$(dirname "$(dirname "$(realpath "$0")")")
if [[ -z "${CHIPSMITH_CYCLONE2_DB:-}" ]]; then
    cd "$root"
    exec nix develop "$root" --command "$0" "$@"
fi
example=$1 mul=$2 div=$3
dir=$(realpath -m "${4:-/tmp/de2-at-fmax/$example-$mul-$div}")
cd "$root"
cargo build --release --quiet --bin chipsmith
cargo build --release --quiet -p chipsmith-cyclone2 --example de2_user1
rm -rf "$dir"
mkdir -p "$dir"
cp -r "examples/$example/." "$dir/"
rm -rf "$dir/build"
grep -q 'wire clk = CLOCK_50;' "$dir/src/top.v"
pll="  wire clk;
  CY2_PLL #(.INCLK0_INPUT_FREQUENCY(20000), .CLK0_MULTIPLY_BY($mul), .CLK0_DIVIDE_BY($div)) pll (.INCLK(CLOCK_50), .CLK0(clk));"
python3 - "$dir/src/top.v" "$pll" <<'EOF'
import sys
p, pll = sys.argv[1], sys.argv[2]
s = open(p).read().replace("  wire clk = CLOCK_50;", pll, 1)
open(p, "w").write(s)
EOF
(cd "$dir" && "$root/target/release/chipsmith" build) > "$dir/build.log" 2>&1 || { tail -20 "$dir/build.log"; exit 1; }
echo "$example at $(python3 -c "print(f'{50 * $mul / $div:.2f}')") MHz; chipsmith:"
rg 'clock .*critical path' "$dir/build.log"
(cd "$dir" && "$root/target/release/chipsmith" flash) > "$dir/flash.log" 2>&1 || { tail -20 "$dir/flash.log"; exit 1; }
target/release/examples/de2_user1
