// The signatures the RTL defines: src/top.v (BSCANE2 left out) run in
// Verilator for as many cycles as the board runs, two-state with every
// register and memory word starting at 0, as the block RAMs' INIT and the
// registers' INIT give on the board. Prints USER1's words, LSB first, as
// run.sh reads them from the board.
#include <cstdio>
#include "Vtop.h"
#include "Vtop___024root.h"

int main() {
    Vtop top;
    top.clk = 0;
    top.eval();
    // 256 cycles before the start (top.v's settle), and some past the stop.
    for (int i = 0; i < (1 << 20) + 512; i++) {
        top.clk = 1;
        top.eval();
        top.clk = 0;
        top.eval();
    }
    const auto &r = top.rootp->top__DOT__result;
    for (size_t w = 0; w < r.Words; w++) printf("%08x\n", r[w]);
    return 0;
}
