// chipsmith's serial flash loader for the DE2: a JTAG-to-SPI bridge onto
// the EP2C35's configuration flash (the EPCS16 on ASDO, nCSO, DCLK, DATA0).
//
// With USER0 (IR 0x00c) in the instruction register, every Shift-DR is one
// SPI transaction: nCSO goes low for the shift, each TCK is one DCLK (ASDO
// = TDI, sampled by the flash on the rising edge, as the TAP samples TDI),
// and DATA0 comes back on TDO. The host sends each byte MSB first.
//
// Purely combinational, so it needs no clock from the core: shiftuser
// changes on the falling edge of TCK (IEEE 1149.1's ShiftDR), so gating
// TCK with it gives exactly one DCLK per bit shifted and no runt pulse.
module top (output [1:0] LEDG);
  wire tck, tdi, shift, usr1, data0;
  // USER0 only: USER1 is left for anything else on the TAP.
  wire selected = shift & ~usr1;

  CY2_JTAG jtag (
    .TDOUSER(data0),
    .TCKUTAP(tck),
    .TDIUTAP(tdi),
    .SHIFTUSER(shift),
    .USR1USER(usr1)
  );
  CY2_ASMI asmi (
    .DCLKIN(tck & selected),
    .SCEIN(~selected),
    .SDOIN(tdi),
    .DATA0OUT(data0)
  );

  // LEDG0: the loader is in; LEDG1: a flash transaction is running.
  assign LEDG = {selected, 1'b1};
endmodule
