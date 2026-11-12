// Two FPGA-to-HPS interrupt lines the HPS reads in GICD_ISPENDR1
// (0x1D000204), at rates no other build of the board's HPS shell has:
//   irq0  f2h_irq0[0], bit 17: cnt[23], 2.98 Hz
//   irq3  f2h_irq0[3], bit 20: cnt[24] & cnt[23], high a quarter of the time
// The HPS's other inputs keep the ties of the board's HPS shell (agilex5-re
// tools/hps_shell.sh): irq0[2] reads 0, the undriven irq0[1], [4] and [5]
// read 1. agilex5-re's designs/hpsf/open, which ran on the board 2026-11-09.
module irq (
    input  wire CLOCK1_50,
    output wire irq0,
    output wire irq3
);
   reg [24:0] cnt = 25'd0;
   always @(posedge CLOCK1_50) cnt <= cnt + 1'b1;
   assign irq0 = cnt[23];
   assign irq3 = cnt[24] & cnt[23];
endmodule
