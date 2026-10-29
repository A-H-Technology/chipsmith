# chipsmith

An open FPGA toolchain: HDL in, programmed chip out, no vendor software.

```
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl build
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl flash
nix run github:A-H-Technology/chipsmith/open -- -C examples/blinky-vhdl flash --persist  # DE2: into the EPCS
nix run github:A-H-Technology/chipsmith/open -- -C examples/zybo-jtag-counter flash --ssh venus  # Zybo, through its Linux
```

The first run builds cyclone2-re's device database package and flattens
its chip database into the router's graph. After that a build of blinky-vhdl takes about 6 s. A DE2 build
runs no program but chipsmith between the Verilog and the chip: no yosys,
no nextpnr, no cram. GHDL translates VHDL in front of it.

## Status

| device | board | build | on hardware |
|---|---|---|---|
| EP2C35F672C6 (Cyclone II) | Terasic DE2 | yes, with no external tool but GHDL for VHDL (2026-10-25: chipsmith's own Verilog front end and synthesis): LUT/carry/register logic, M4K RAM blocks (below), global clocks from pins and from logic (a register, the JTAG block's TCK; one clock per LAB), PLLs (`CY2_PLL`; both on the board 2026-10-26, examples/de2-jtag-counter on TCK and examples/de2-pll-counter at 100 and 40 MHz from CLOCK_50); pins input, output, tri-state, bidirectional, open drain, at 3.3-V LVTTL (any current) or LVCMOS or 2.5-V inputs, with pull-up or bus hold and I/O-cell registers (Manifest, below) | yes (2026-10-25, chipsmith's own synthesis end to end, checked over JTAG: examples/de2-jtag-counter (its earlier CLOCK_50-sampled version) reads back `"DE2!"` at 50.00 MHz after a JTAG load and after `flash --persist` (loader synthesized by chipsmith too, EPCS written, verified and reloaded from; 2026-10-26 also after a power cycle); picorv32 with the same readback added runs its loop at 3.33 MHz. 2026-10-24, synthesized by yosys: picorv32 built with chipsmith's LUT mapper by the nix package, flashed by chipsmith, counting on the LEDs; examples/bidir driving, releasing and reading back its pins; examples/ram-leds stepping its ROM and RAM on the LEDs; `flash --persist` writes the EPCS16 and the design runs from it, roadmap step 6) |
| xc7z010clg400-1 (Zynq-7010, PL only) | Digilent Zybo (original, "legacy") | yes: what yosys `synth_xilinx` and openXC7's nextpnr's packer handle, LVCMOS33 pins; chipsmith places LUT/register/carry logic, RAM32Ms, pins, BUFGs, BSCANs and the PS7 and routes the interconnect (below) | yes (2026-10-24: zybo-jtag-counter placed and routed by chipsmith, read back over JTAG at 125.00 MHz, loaded over JTAG and with `flash --ssh` through Linux on the board; a bitstream loaded at every boot through nixos-fpga) |

`examples/picorv32` is a RISC-V core (PicoRV32, RV32I) counting on the
LEDs, 1,171 LEs; a build takes about 42 s, half of it placement.

`examples/de2-jtag-counter` answers over the JTAG cable, so a load can be
checked without watching the board: the Cyclone II's JTAG block makes it a
64-bit USER1 register holding `"DE2!"` and a Gray-coded 50 MHz counter.
`cargo run --release -p chipsmith-cyclone2 --example de2_user1` reads it
twice, a second apart. Read on 2026-10-26, clocked by TCK:

```
USER1 444532210c0ec0a4: tag "DE2!", counts 134971192
USER1 444532210e8c6078: tag "DE2!", counts 185090128
counter 0: 50118936 counts in 1.002 s: 50.00 MHz
```

Since 2026-10-26 the register is clocked by TCK itself: the JTAG block's
TCK output goes through the core to a clock control and onto a global
clock (cyclone2-re README, "Global clocks from the core"), so the reader
shifts in byte mode. Quartus assembles chipsmith's placement and routing
of it into the same RBF (`bench/quartus-validate.sh`), and on the board
(2026-10-26) it reads back `"DE2!"` counting at 50.00 MHz. The cable
samples TDO after the shifting edge in byte mode too, so the reader still
gets the register from its bit 1. Before, the DE2 back end clocked registers from pins only, so the
register ran on CLOCK_50 and sampled TCK through two flip-flops, and the
reader had to bit-bang its shift slowly enough for that sampling, its
first bit being the register's bit 1.

PLLs: a design instantiates `CY2_PLL` (`crates/chipsmith-cyclone2/src/synth/cells.v`)
with altpll's parameters, the input period and each output's
multiply/divide ratio, phase shift and duty cycle, and chipsmith finds the
counters (`crates/chipsmith-cyclone2/src/pll.rs`, ADR-0008). Its INCLK must
come from a dedicated clock pin, which picks the PLL: CLOCK_50 (PIN_N2)
feeds PLL_1, CLOCK_27 PLL_3, EXT_CLOCK PLL_2. Outputs CLK0-CLK2 reach the
registers on global clocks; LOCKED, and the ARESET and PFDENA inputs, are
logic. The timing report times each output at its frequency.

`examples/de2-pll-counter` measures its PLL over JTAG: PLL_1 makes 100 MHz
and 40 MHz from CLOCK_50, each clock runs a Gray-coded counter, and a
128-bit USER1 register clocked by TCK holds `"PLL!"` and the three counts.
`cargo run --release -p chipsmith-cyclone2 --example de2_user1 -- --words 4`
prints counter 1 at 2.000000 and counter 2 at 0.800000 times counter 0
(CLOCK_50), observed on the DE2 on 2026-10-26: 50.00, 100.0000 and
40.0000 MHz; LEDG[0] is the lock. Quartus assembles chipsmith's placement
and routing of it byte-identically (setup slack 6.21 ns at 100 MHz in
quartus_sta, 6.20 ns estimated).

`examples/bidir` is cyclone2-re's tri-state test: expansion-header and
SRAM data pins driven with their output enable from a switch, an
inverted switch, logic or a register, one open drain, all read back onto
the red LEDs, and a tri-state green LED. An inout must be written
`assign p = oe ? d : 1'bz` (synthesis' `tribuf` and pin buffers make it
one `CY2_IOBUF`); one without a tri-state driver is an error, and so is a
driving pin on a dedicated clock input. With every switch down on the DE2
(2026-10-24, camera): LEDR[1] blinks with the counter it drives out,
LEDR[4] and LEDG[1] follow the open drain, the released pins read 1 and
LEDG[0] stays off. Don't load it with SW[4] up and the SRAM driving: the
FPGA then drives SRAM_DQ too.

`examples/de2-io` sets its pins up from the manifest (Manifest, below):
the high switches through input registers onto five red LEDs through
output registers, green LEDs at 4 mA through output registers (one
powering up lit, behind an inverter), weak pull-ups and bus hold on
expansion-header inputs shown on the LEDs, HEX0 at 3.3-V LVCMOS, the keys
read as 2.5-V inputs, and GPIO_0[4] driven through its output and OE
registers while KEY[0] is held, read back through its input register.
Safe to load with nothing plugged into the GPIO_0 header: everything else
it drives is an LED or the 7-segment digit. Not yet tried on the board.

Pin settings are checked against Quartus the same way
(`bench/quartus-validate.sh`, `bench/io-fuzz.py`), 2026-10-26: examples/de2-io
and 12 random projects (`io-fuzz.py` seeds 1-12: 1,377 pins on all four
sides, 777 I/O registers, 445 non-default LVTTL currents, 204 LVCMOS and 70
2.5-V pins, 280 pull-ups, 233 bus holds, all three unused-pin modes), none
of which any rule was fit on, assemble to the same RBF as Quartus: 13 of
13. The DE2 examples that existed before (blinky-verilog, bidir, ram,
ram_widths, ram-leds, de2-jtag-counter, picorv32) build to the same RBFs
as before. The I/O registers' timing (`chipsmith-route`'s `timing.rs`) is
quartus_sta's on de2-io, where the critical path now ends at GPIO_0[4]'s
OE register: 16.29 ns of slack estimated, 15.67 ns in Quartus.

M4K RAM blocks (4,608 bits each, 105 on the EP2C35) come from chipsmith's
synthesis (`crates/chipsmith-cyclone2/src/synth/m4k.rs`, yosys'
`memory_libmap` as the reference for what a block takes;
`crates/chipsmith-cyclone2/src/m4k.rs` is the block's configuration as one
type the synthesis, packer, bitstream and Quartus export share). Every
memory with synchronous reads that a block can hold goes into blocks, as
Quartus does, unless its declaration says `ramstyle`/`ram_style` "logic":

- single port x1 to x18 (a read of the word being written returns the new
  data), simple dual port x1 to x36 (old data), true dual port up to x18
  (each port sees its own write, the other's old data; written as Quartus'
  template, `q <= we ? d : mem[a]`), ROMs, initial contents;
- mixed widths: a port reading or writing several words at once (Verilog's
  `{mem[{a, 2'd1}], mem[{a, 2'd0}]}`), any pair of widths;
- byte enables (`mem[a][15:8] <= d` under its own condition), packed eight
  bits per nine columns as Quartus packs them;
- clock enables: a read enable the block's clock enable can carry (the
  write under it too, or the read on clock 1 of the same clock); one it
  can't (a write outside it) holds the read data in LE registers;
- a second register on the read data, with an asynchronous clear and an
  enable of its own: the block's output register (clock 0 or 1, clear 0
  or 1);
- two clocks: written on one, read on another, as Quartus builds it (a
  true dual port reading on port A, writing on port B; Quartus 13.0 has no
  simple dual port on two clocks, so up to x18 a block, wider ones split);
- memories deeper than a block (blocks in rows, the write enable decoded
  and the read data multiplexed by the registered top address bits) and
  wider (blocks side by side), and shallower: their top address bit is
  repeated on the block's unused address pins, so every word has an
  address of its own;
- a synchronous reset on the read data (LE logic after the block);
- both ports writing one word on one edge (a true dual port's write
  collision): the block writes neither reliably, so the netlist has one
  port write each bit, the data yosys gives priority (as `memory_map`
  has it). Where the losing port's word fits in the winner's, the loser's
  write enable is masked; where the winner's fits in the loser's wider
  word, the winner's is masked and the loser writes the winner's data
  there. A port that reads its own write's new data then reads something
  else from the block on that edge, so LE registers put its write data
  back on those bits. A collision neither way covers (byte enables on the
  port that would be masked by one signal) leaves the memory in logic.
  Where one port's write enable already requires the two addresses to
  differ (`web & (ab != aa)`, as Quartus' template has it), there are no
  collisions and no such logic: Quartus would find it dead and sweep it.

Not taken: asynchronous reads (logic), a read register that powers up to
anything but 0, a write enable tied high (an unconnected write enable reads
as no write), Quartus' packed single port x32/x36 (two x18 blocks
instead), a same-port read of the old data in a true dual port, more
than two ports per block (no duplication of a memory per read port).
The packer moves a `CY2_M4K` onto the block's physical pins (some address
bits enter on data pins), the placer swaps blocks between M4K sites, and
the router reaches their inputs through the block's LAB lines, the clocks
through row clocks, clock enables and clears through the block's control
inputs, and the read data from the M4K buffers. A data input written as a
constant gets an LE driving it (the block doesn't read an unconnected one
as 0); a yosys-flow `CY2_M4K` doesn't say which columns are memory bits
(the flow pads with 0), so there it is the columns whose read data is
used.

- `examples/ram`: one memory of each shape (7 blocks), written from a
  counter and the switches, read data folded onto the LEDs.
- `examples/ram_widths`: the widths `ram` doesn't use (10 blocks).
- `examples/ram-leds`: a hardware demo. A 256 x 18 ROM steps a bouncing
  light and a filling bar across LEDR about 12 times a second. A 512 x 9
  RAM, initialised to a Gray code, is read at the same step onto LEDG.
  KEY[0] writes SW[8:0] at the address shown, KEY[1] holds the step so the
  value can be seen coming back. Runs on the DE2, from JTAG and from the
  EPCS.

- `examples/m4k-bytes`, `m4k-enables`, `m4k-outreg`, `m4k-tdp`,
  `m4k-mixed`, `m4k-dualclock`, `m4k-deep`, `m4k-shallow`: one mode each
  (the file heads say which memory is which block shape), written from a
  counter and the switches, read data folded onto the LEDs. Not run on the
  board yet.
- `examples/m4k-selftest`: a hardware test read back over JTAG like
  de2-jtag-counter. Eight memories (simple and true dual port, four blocks
  deep, byte enables, mixed widths, an output register, a ROM under a read
  enable, a shallow one, 11 blocks) are written with a pattern of the pass
  number and read back, over and over; USER1 holds `"M4K!"`, the passes and
  the reads that differed, so `de2_user1` prints them as its count (passes
  in the top 16 bits, errors in the bottom 16: 0 on a good board);
  LEDR[3:1] show the pass count's top bits, LEDR[0] lights on an error.
  Run on the DE2 2026-10-28 (chipsmith bfdfb93, 358 LEs, 11 M4Ks, loaded
  over JTAG): `"M4K!"`, the passes rising about 6,100 a second, 0 errors
  over tens of thousands of passes (`de2_user1` can't see bit 0 of the
  low word, so it reads 0 or 1; a real mismatch recurs every pass), and
  LEDR[3:1] counting with LEDR[0] off. Since then a ninth memory, a true
  dual port whose ports write one word on every write edge (the
  collision above), checks each port reads back its own write and the
  word holds port B's. Run with it on the DE2 2026-10-29 (chipsmith
  816ddd5, 12 M4Ks, loaded over JTAG): `"M4K!"`, the passes rising,
  0 errors.

How the M4K mapping is checked (2026-10-28, after merging `open` 5b22b89;
`crates/chipsmith-cyclone2/tests/synth.rs`, times on luna with two checks
at a time):

- **Proven, one block per configuration** (`m4k_shape_*`,
  `tests/support/m4k_shapes.rs`): a memory alone in a design, its ports on
  the design's, for each class: single port x1, x2, x18, simple dual port
  x4, x9, x36, ROM x36, ROM under a read enable, byte enables x16 and x32,
  mixed widths (write x32 read x8, write x8 read x32), true dual port x9
  and x16/x4 (collisions masked in the RTL), the same two with both ports
  writing one word on one edge, two write ports beside one read (the
  idiom `if (a) m[x] <= d1; if (b) m[y] <= d2; q <= a ? d1 : m[x];`), two
  clocks, a clock enable over the whole port, a read enable
  on its own clock, one held in LEs, an output register with a clear and
  an enable, a synchronous reset, a shallow memory. yosys proves the
  netlist equivalent to the RTL with the memory as words on both sides: the
  RTL's through `memory_map`, the block through a model of it
  (`tests/support/mod.rs`, `model_blocks`: contents as registers named after
  the memory words they hold, the block's read and write behaviour, its
  INIT checked against the memory's contents). 3.5 to 7 minutes each
  (ROMs and the shallow memory 8 to 10 s), the true dual ports and
  collisions 20 to 40 minutes; all 24 pass.
- **Proven around the blocks, every design** (`check_staged`): the RTL
  against synthesis' last stage before the M4K mapping, memories compared
  at their ports (read registers outside on both sides, `memory_nordff`),
  and the stage right after the mapping against the final netlist, the
  blocks compared at their pins and parameters. What this leaves out is
  the mapping itself: the blocks and the soft logic between them (row
  decoding, read multiplexers, held read data), which the per-configuration
  proofs cover for one block and co-simulation for several.
- **Co-simulated** (`support::cosim`, iverilog): the RTL against the final
  netlist with the blocks as the same model, 10,000 cycles of random inputs
  (a fixed seed per design, printed on a difference), every output bit the
  RTL defines compared every cycle. Run on the eight examples, the 16
  random designs as built for the DE2, the same 16 with their memory's
  ports on the design's (every address reached every cycle), and the
  examples' large memories alone (2048 x 9 over four blocks, 8192 x 2 over
  four, 256 x 40 over three, the three mixed-width ones). It catches a
  wrong mapping: two address pins of a block swapped, or the write enables
  of two block rows swapped, fail within 200 cycles
  (`m4k_cosim_catches_wrong_blocks`). The three collision shapes are
  co-simulated too (`m4k_cosim_collisions`); before the collision fix,
  one port's read of its own write failed on them. The "ports direct"
  runs and the large memories are separate syntheses of separate designs:
  they check the same mapping code, not the netlist an example ships.
- **What the checks can't see.** yosys' induction has no base case here:
  it proves the step from any state alike on both sides, so power-up
  values (the blocks' 0 output registers, INIT against `initial`) are
  checked only by co-simulation from cycle 0 and by Quartus building the
  same bits. It also steps every register on one edge, so a second
  clock (a write on one clock, a read on another) is co-simulated, not
  proven; and write priority between two ports is proven only in the
  three collision shapes, elsewhere co-simulated.
- **Quartus byte-identical**: `cram quartus-export` of chipsmith's placement
  and routing, assembled by Quartus 13.0sp1, compared with chipsmith's RBF
  (cyclone2-re `tools/validate.sh`).

The earlier check proved whole designs with every memory as words; it took
15 to 70 minutes a design, didn't finish on m4k-deep in 22 hours, and one
run grew to 7 GB.

Results:

All 88 tests of the cyclone2 synthesis suite passed on one binary, 85
minutes under a 20 GB memory cap, two at a time. The collision fix then
changed the true dual ports only; re-run on its final binary: the five
true dual port and collision shape proofs, the collision co-simulation,
m4k-tdp, m4k-random-1 and -5 (both now writing colliding words) with
their direct co-simulations, and the yosys-flow cells. Every other DE2
design built to the same RBF before and after it; m4k-tdp, m4k-random-1,
-5 and m4k-selftest changed and are Quartus byte-identical. Per design, below: the
check around the blocks (time of the proof), the co-simulation (output
bits compared, time with the build), and for the random designs the
co-simulation with the memory's ports on the design's. A low bit count
means the RTL leaves most outputs x (memories read before they are
written). Weakly covered by co-simulation as shipped: `ram` (108 bits),
m4k-mixed (60), m4k-random-4 (1,080), m4k-dualclock (1,512) and m4k-bytes
(2,136); their memories' shapes are co-simulated well only in separate
designs (the large memories: 2048 x 9 over four blocks 56,142 bits,
8192 x 2 20,000, 256 x 40 389,680, write x32 read x8 78,096, write x8
read x32 286,968, write x2 read x1 6,254; and the ports-direct column). serv, m4k-selftest and every design below are
Quartus byte-identical.

| design | memories | M4Ks | Quartus | proven around the blocks | co-simulated | co-simulated, ports direct |
|---|---|---|---|---|---|---|
| m4k-bytes | byte enables: 256 x 16 (2), 128 x 32 (4) | 2 | byte-identical | 1356 $equiv, 5s | 2,136 bits, 15s |  |
| m4k-deep | 2048 x 9 over 4 blocks, 8192 x 2 ROM over 4, 256 x 40 over 3 | 11 | byte-identical | 1804 $equiv, 27s | 179,982 bits, 37s |  |
| m4k-dualclock | written on CLOCK_50, read on CLOCK_27 (x9; x36 as two x18) | 3 | byte-identical | 1104 $equiv, 4s | 1,512 bits, 16s |  |
| m4k-enables | clock enable, read enables on ENA0/ENA1, one held in LEs | 4 | byte-identical | 990 $equiv, 7s | 80,562 bits, 13s |  |
| m4k-mixed | write x32 read x8, write x8 read x32, write x2 read x1 | 3 | byte-identical | 1080 $equiv, 5s | 60 bits, 14s |  |
| m4k-outreg | output registers: plain, cleared, own enable; a synchronous reset | 4 | byte-identical | 1102 $equiv, 6s | 67,356 bits, 19s |  |
| m4k-shallow | 64 x 8, 32 x 36, 16 x 4 ROM | 3 | byte-identical | 912 $equiv, 4s | 7,776 bits, 15s |  |
| m4k-tdp | true dual port x9/x9 and x4/x18 | 2 | byte-identical | 994 $equiv, 6s | 247,662 bits, 13s |  |
| m4k-random-1 | TrueDual 600 x 5, colliding writes | 2 | byte-identical | 720 $equiv, 7s | 257,015 bits, 14s | 76,425 bits |
| m4k-random-2 | SimpleDual 16 x 24, byte enables, two clocks | 2 | byte-identical | 957 $equiv, 4s | 44,600 bits, 12s | 237,824 bits |
| m4k-random-3 | SimpleDual 16 x 5, write x1 read x4 words | 1 | byte-identical | 497 $equiv, 3s | 82,545 bits, 8s | 199,515 bits |
| m4k-random-4 | SimpleDual 1024 x 32, byte enables, output register (plain) | 8 | byte-identical | 1915 $equiv, 7s | 1,080 bits, 21s | 201,472 bits |
| m4k-random-5 | TrueDual 2048 x 2, colliding writes | 1 | byte-identical | 564 $equiv, 7s | 268,536 bits, 9s | 35,982 bits |
| m4k-random-6 | Rom 1024 x 32, contents, output register (cleared) | 8 | byte-identical | 1072 $equiv, 6s | 269,973 bits, 22s | 320,000 bits |
| m4k-random-7 | SimpleDual 1024 x 36, read enable, write under it, sync reset | 8 | byte-identical | 1481 $equiv, 10s | 67,635 bits, 23s | 292,536 bits |
| m4k-random-8 | SingleNew 1024 x 36, contents | 8 | byte-identical | 1344 $equiv, 8s | 270,000 bits, 24s | 360,000 bits |
| m4k-random-9 | SingleNew 64 x 32 | 2 | byte-identical | 680 $equiv, 3s | 269,892 bits, 11s | 318,592 bits |
| m4k-random-10 | SimpleDual 512 x 4, write x2 read x1 words, output register (cleared) | 1 | byte-identical | 534 $equiv, 5s | 244,912 bits, 8s | 39,560 bits |
| m4k-random-11 | SimpleDual 4096 x 9, contents | 8 | byte-identical | 1238 $equiv, 19s | 270,000 bits, 26s | 90,000 bits |
| m4k-random-12 | SingleNew 600 x 36, contents | 6 | byte-identical | 1148 $equiv, 8s | 203,148 bits, 25s | 208,692 bits |
| m4k-random-13 | SingleNew 4096 x 8 | 8 | byte-identical | 1117 $equiv, 4s | 269,712 bits, 17s | 56,576 bits |
| m4k-random-14 | SingleNew 16 x 4 | 1 | byte-identical | 349 $equiv, 3s | 269,984 bits, 6s | 39,900 bits |
| m4k-random-15 | SingleOld 2048 x 18, read enable, write under it, output register (plain) | 8 | byte-identical | 1317 $equiv, 6s | 268,956 bits, 23s | 77,058 bits |
| m4k-random-16 | SimpleDual 512 x 5, write x2 read x1 words, output register (plain) | 1 | byte-identical | 548 $equiv, 3s | 220,410 bits, 7s | 47,285 bits |

A memory bit written as a constant gets an LE driving the block's data
input: the block doesn't read an unconnected data input as 0, and Quartus
routes its GND there (found on m4k-selftest, which writes a 32-bit pattern
whose top bit was always 0). Quartus replaces that LE by its own GND
elsewhere, so a design that writes a constant bit builds correctly but not
byte-identical; the examples don't. An inverter alone in front of a write
enable is the other such case (below).

The random designs are `crates/chipsmith-cyclone2/tests/support/m4k_random.rs`,
seeds 1-16 (`cargo run --release -p chipsmith-cyclone2 --example
m4k_random -- <dir> 1 16` writes them as projects): one memory each, its
kind, widths, depth, byte enables, read enable, output register, reset,
clocks and contents drawn from the seed. Every existing DE2 example builds
to the same RBF as on `open` but serv, whose register file is now an M4K
(Quartus byte-identical; it was LE registers). de2-io and de2-pll-counter
don't build here and don't on `open` either.

Quartus assembles chipsmith's placement and routing into the same RBF bit
for bit (`bench/quartus-timing.sh`): checked 2026-10-24 on blinky-verilog
and blinky-vhdl (16.65 ns of slack at 50 MHz), on picorv32 (10.60 ns
since cyclone2-re 9cfe9be, see below; 11.08 ns before) and on bidir
(16.64 ns), all built through chipsmith's LUT mapper, and on ram
(16.21 ns), ram_widths (14.80 ns) and ram-leds (14.80 ns). Those were
yosys-synthesized. With chipsmith's own synthesis, `bench/de2-timing.sh`
builds every DE2 example with a clock and has Quartus time the same
placement and routing (2026-10-27, 50 MHz constraint; error is the
estimate's critical path against Quartus'):

| design | estimate | Quartus | error | RBF byte-identical |
|---|---|---|---|---|
| blinky-verilog, blinky-vhdl | 3.37 ns | 3.35 ns | +0.5% | yes |
| bidir | 3.37 ns | 3.36 ns | +0.3% | yes |
| ram | 3.80 ns | 3.79 ns | +0.3% | yes |
| ram_widths | 3.80 ns | 3.78 ns | +0.5% | yes |
| ram-leds | 5.04 ns | 5.12 ns | -1.6% | yes |
| de2-jtag-counter | 3.80 ns | 3.79 ns | +0.3% | no (12 frame bits) |
| uart | 6.05 ns | 6.26 ns | -3.3% | no |
| spi-master | 4.71 ns | 4.46 ns | +5.7% | no |
| i2c-master | 7.82 ns | 8.83 ns | -11.4% | no |
| femtorv32 | 13.74 ns | 14.28 ns | -3.8% | yes |
| serv | 11.86 ns | 13.28 ns | -10.7% | yes |
| sha256 | 26.24 ns | 27.73 ns | -5.4% | no |

Small designs are within 2%; the larger ones run 4 to 11% slower in
Quartus than estimated. Over the 355 connections of the 300 worst
Quartus paths of picorv32, femtorv32, serv and ram-leds that route the
same in both (`bench/timing-calib.py`), Quartus' routing delay is 1.08
times the model's (C4 the most: 312 ps median against the model's 285).
Scaling every routing delay by that brought the four larger designs'
mean error from 6.6% to 5.3%, but Quartus timed three of them slower on
the placements it led to (picorv32 8.57 to 9.07 ns, i2c-master 8.83 to
9.89 ns), so the model stays as it is; the gap is where per-wire delays
(fanout, wire length) would have to come in. The designs that aren't
byte-identical are timed on Quartus' reassembly: uart and i2c-master feed
a pin through an inverter into a register (the known case below);
spi-master's and de2-jtag-counter's differences aren't investigated yet. de2-io's I/O registers need a
newer cyclone2-re `cram quartus-export`.
picorv32 has no M4K; its slack moved because cyclone2-re's synthesis
script gained `memory_libmap`, which renames yosys' cells and nets. Same
logic, same 1,175 LEs, but other net ids and so another placement. The
same chipsmith with the previous script places it at 11.08 ns again.
An inverter from a pin straight into an M4K's write enable, or into a
register, doesn't validate: quartus_map removes the one-input LUT and
folds it into the block (cram doesn't model that bit yet) or re-creates
it elsewhere. The bitstream still works; `ram-leds` registers its key. Since chipsmith places
designs itself, the RBFs no longer match the
[cyclone2-re](https://github.com/cramt/cyclone2-re) examples built with
nextpnr's placement.

The Cyclone II limits come from cyclone2-re's open flow: M4K RAM as
above, no multipliers, PLLs without their external clock
output, clock switchover or reconfiguration, I/O registers without clock
enables or clears, 3.3-V LVCMOS at its default current only.
The timing model covers LE and register delays exactly and routing by
per-element-type averages. M4K clock-to-output (3.33 ns) and setup are
from one quartus_sta path each, and so is the M4K buffer's routing delay.

The Zybo examples:

- `examples/zybo-blinky`: a counter on LD0-LD3. Builds in about 70 s.
- `examples/zybo-jtag-counter`: the Zybo's USB-UART is wired to the
  Zynq's processor (PS MIO 48/49, Digilent's Zybo reference manual,
  section 6), not to the FPGA fabric. So without software on the ARM
  cores, no PL design can print to it. This design answers over the JTAG
  cable instead. A BSCANE2 makes it a 64-bit USER1 register holding
  `"ZYBO"` and a Gray-coded 125 MHz counter.
  `cargo run --release --example xc7_user1` reads it twice, a second apart,
  with chipsmith's own cable code. Builds in about 87 s with chipsmith's
  placement and routing (three nextpnr runs, below). Two builds give the
  same placement and the same FASM. Read back 2026-10-24, chipsmith's
  placement and routing, loaded with `flash --ssh`:

  ```
  USER1 0x5a59424fd795d8f0: tag "ZYBO", count 2598801568
  USER1 0x5a59424ff37745fb: tag "ZYBO", count 2723838290
  125036722 counts in 1.000 s: 125.00 MHz
  ```

  With zybo-blinky loaded instead, the same read returns all zeros.
- `examples/zybo-picorv32`: `examples/picorv32`'s sources with its ports
  on arbitrary Zybo balls, a placement and routing bench, not for
  hardware. Builds in about 110 s.

Zybo limits, all of them inherited from the tools:

- Every pin is LVCMOS33. The manifest has no I/O standards yet, and the
  Zybo's PL banks (34, 35) are both 3.3 V.
- Pins are the package's 100 PL user I/O balls (prjxray-db's
  `package_pins.csv`). PS pins (MIO, DDR) are rejected in the manifest.
- The PS isn't configured from the fabric side beyond one thing: every
  design gets a PS7 whose IRQF2P inputs are tied to 0 (the build's "PS7"
  step, or the design's own PS7 if it leaves IRQF2P open). IRQF2P[19:16]
  are the ARM cores' FIQ and IRQ, active high, and an interconnect input
  nothing drives reads as 1. So a design without a PS7, Vivado's
  PL-only blinky included, holds FIQ on both cores, and the moment the
  PL->PS level shifters come on under Linux (Linux loading it, or a JTAG
  load while U-Boot runs) both cores spin in the FIQ handler. Observed on
  venus, halted over JTAG: both cores in `__fiq_svc`/`ct_nmi_enter`; with
  IRQF2P tied to 0 Linux stays up. Designs that use the PS beyond that
  (AXI, PS clocks, interrupts to Linux) are untested.
- Both Zybo examples declare their 125 MHz clock in `[clocks]`, which
  reaches nextpnr as an XDC `create_clock`. nextpnr's estimate tracks
  Vivado's. zybo-blinky through the Vivado 2026.1 oracle (xc7-re
  `tools/oracle.sh`, same Verilog, 8 ns clock): 27 registers, 1 LUT,
  7 CARRY4, WNS 5.438 ns, so a 2.562 ns critical path (390.3 MHz) through
  the 7-CARRY4 chain. chipsmith's build prints nextpnr's 391.5 MHz, slack
  5.446 ns at 125 MHz, 0.3% apart. zybo-jtag-counter: 317.2 MHz,
  slack 4.847 ns (no Vivado number yet). Its BSCANE2's `tck` has no pin,
  so `[clocks]` can't name it, and nextpnr times it at its default
  12 MHz. nextpnr reports 56 LUT BELs, against Vivado's 1 LUT. That is
  most likely route-through LUTs, not checked.
- The flow is deterministic: two builds give the same FASM. The `.bit`
  header still carries xc7frames2bit's build date and time.
- The Zybo Z7 has its clock on K17. It needs only another pin map; the
  Z7-20 (xc7z020) needs another `Device`.

### Loading a Zybo from its own Linux

`chipsmith flash --ssh HOST` loads the PL from Linux on the Zybo's ARM
cores instead of over the JTAG cable: no cable or cable drivers on the
host, any board on the network. HOST runs NixOS with
[nixos-fpga](https://github.com/A-H-Technology/nixos-fpga)'s
`digilent-zybo` module, whose `/dev/fpga-load` hands a bitstream to the
kernel's fpga-manager (the Zynq's devcfg/PCAP), and the ssh user must be
in its `fpga` group. chipsmith sends the `.bit`'s payload with every
32-bit word byte-swapped, the format the Zynq driver takes, then checks
the manager reports `operating`. The transfer and load of zybo-jtag-counter's
2 MB take about 0.5 s; the rest is the ssh login.

Every Zybo build also writes that format next to the `.bit`
(`build/<name>.bin`). Set nixos-fpga's `hardware.fpga.bitstream` to it and
the board loads it at every boot, which is the Zybo's persistent flashing
(roadmap step 6). Observed 2026-10-24 on venus: zybo-blinky and
zybo-jtag-counter loaded with `flash --ssh`, the JTAG readback following
each (zeros, then `"ZYBO"` at 125.00 MHz), Linux up throughout; blinky
loaded by the boot service on a reboot.

## Flow

Every stage is one `Step` in a `Plan` (`crates/chipsmith/src/flow.rs`). The device in the
manifest picks the plan.

Cyclone II (EP2C35F672C6):

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | stays a translator in front of the flow |
| Verilog front end | chipsmith: preprocessor, parser, elaboration to yosys' `proc` level, with the Cyclone II cell library (`crates/chipsmith-verilog`, `crates/chipsmith-cyclone2/src/synth/cells.v`) | ours |
| synthesis | chipsmith: what cyclone2-re's `cyclone2.ys` does, on chipsmith's netlist IR (`crates/chipsmith-synth`, `crates/chipsmith-cyclone2/src/synth`) | ours |
| LUT mapping | chipsmith: AIG plus priority cuts, in place of `abc -lut 4` (`crates/chipsmith-synth/src/lutmap.rs`) | ours |
| packing | chipsmith: cyclone2-re's nextpnr packer, ported (`crates/chipsmith-cyclone2/src/pack`) | ours |
| placement | chipsmith: simulated annealing over LAB slots, nextpnr's LAB rules (`crates/chipsmith-cyclone2/src/place.rs`) | ours |
| routing | chipsmith: PathFinder over the chip database (`crates/chipsmith-route`, `crates/chipsmith-cyclone2/src/route.rs`) | ours |
| bitstream | chipsmith: cyclone2-re's assembler (`cram pack`), moved here and still checked against Quartus by cyclone2-re's `cram` (`crates/chipsmith-cyclone2/src/bitstream`, ADR-0003) | ours |
| JTAG stream | chipsmith: the SVF `cram svf` wrote, CONF_DONE checked (`crates/chipsmith-cyclone2/src/bitstream/jtag.rs`) | ours |
| cable | chipsmith: USB-Blaster and FT2232 (MPSSE) drivers, SVF player, 7-series JTAG loader (`crates/chipsmith-jtag`) | ours |
| configuration flash | chipsmith: serial flash loader (built by chipsmith) and EPCS16 driver (`crates/chipsmith-cyclone2/src/persist`) | ours |

The plan is `[ghdl,] synthesis, pack, place and route, bitstream`. yosys
and cyclone2-re's techmap are test oracles now (ADR-0004): the devshell has
them, the build doesn't run them.

Zynq-7010 PL (xc7z010clg400-1): shell out, except placement, routing and
the cable.

| stage | today | |
|---|---|---|
| VHDL → Verilog | GHDL `--synth --out=verilog` | as for Cyclone II |
| synthesis | yosys `synth_xilinx -flatten -abc9 -arch xc7` | to replace |
| packing | openXC7's nextpnr (`nextpnr-himbaechel --pack-only`, xilinx uarch), chip database for xc7z010 built from prjxray-db in nix (2.5 min) | to replace next |
| placement | chipsmith: simulated annealing over slices, timing-driven with prjxray-db's delays (`crates/chipsmith-xc7/src/place.rs`, `crates/chipsmith-xc7/src/delay.rs`) | ours |
| site routing | nextpnr, routing chipsmith's placement (`-o preplaced`) | to replace |
| interconnect routing | chipsmith: PathFinder over the prjxray-db graph (`crates/chipsmith-xc7/src/xray.rs`, `crates/chipsmith-xc7/src/route.rs`) | ours |
| FASM | nextpnr's FASM writer, from chipsmith's routes (`-o preplaced`, `-o prerouted`) | to replace |
| FASM → frames | prjxray `fasm2frames` | to replace |
| frames → `.bit` | prjxray `xc7frames2bit` | to replace |
| cable | chipsmith: FT2232 (MPSSE) driver and 7-series JTAG loader (`crates/chipsmith-jtag`), loads in 1.3 s with DONE checked | ours |

openXC7's nextpnr, prjxray, fasm and prjxray-db come from one pinned
[openXC7/toolchain-nix](https://github.com/openXC7/toolchain-nix)
revision (`flake.nix`). That revision is the first whose nextpnr places
BSCANE2. Replacing a tool means replacing one step. The nix package pins
every tool. The devshell pins them too.

Verilog is the input language. Anything else, VHDL today and later Chisel,
Amaranth and the like, gets translated into it. Internally, the stages will
share chipsmith's own netlist IR, not Verilog.

## Roadmap

1. **Shell out** (done): `build` and `flash` over the existing open tools.
2. **Own cable** (done): a USB-Blaster driver and an SVF player in Rust.
   `chipsmith flash` loads the DE2 in 1.4 s, and fails on CONF_DONE low.
3. **Own place and route**: a placer and router over the decoded routing
   graph. cyclone2-re's `just validate` checks each result against Quartus.
   This stage brings the timing model.
   - Router (done). Quartus keeps 100% of its routes and assembles RBFs
     byte-identical to ours for blinky, lfsr, uart_tx and picorv32. Same
     placement, nextpnr's router2 vs ours:

     | design | router | time | routing bits | setup slack @ 50 MHz (Quartus STA) |
     |---|---|---|---|---|
     | picorv32 | nextpnr router2 | 24.2 s | 17,209 | 2.25 ns |
     | picorv32 | chipsmith | 9.1 s | 10,127 | 9.30 ns |
     | lfsr | chipsmith | 0.7 s | 1,127 | 10.93 ns |
     | uart_tx | chipsmith | 0.3 s | 528 | 13.39 ns |

     The routing graph (861,427 nodes, 8,325,848 pips) is flattened from
     cyclone2-re's chip database by a nix derivation. Loading it took 2.4 s;
     now 0.45 s: the name index is built only while parsing (1.05 s), the
     2.3M wire names are one string per tile type, the large arrays are raw
     little-endian words instead of postcard varints, and those decode on
     parallel threads (the cost left is first-touch page faults).
     Routing nets of one PathFinder iteration in parallel batches (each net
     blind to the others in its batch) didn't converge on picorv32 within
     5 minutes at batch sizes 8 and 32, against 6.6 s sequentially: parallel
     routing needs nets that can't compete, e.g. disjoint bounding boxes.
   - Placer (done). nextpnr only packs; chipsmith places and routes. Quartus
     keeps 100% of the routes and assembles byte-identical RBFs for lfsr,
     uart_tx and picorv32:

     | design | placer + router | place time | setup slack @ 50 MHz (Quartus STA) |
     |---|---|---|---|
     | picorv32 | nextpnr (HeAP + SA, router2) | 50.8 s | 2.25 ns |
     | picorv32 | chipsmith, wirelength only | 12.2 s | 9.10 ns |
     | picorv32 | chipsmith, timing-driven (below) | 22.3 s | 11.23 ns |
     | lfsr | chipsmith | 1.5 s | 12.22 ns |
     | uart_tx | chipsmith | 0.7 s | 13.16 ns |

     Placer speed on picorv32, same result each time: 31.8 s with std
     `HashMap`, 19.8 s with FxHash, 12.2 s with an enum of ports and dense
     net ids (no hashing per move). Recomputing high-fanout nets' bounding
     boxes is the largest cost left (26%). Annealing 4x longer (8 moves
     per temperature, 139 s) ends at lower wirelength (3,903 vs 4,261) but
     less slack (8.84 ns): wirelength is not timing, so the next gain is a
     timing model, not more annealing.
   - Timing model (`crates/chipsmith-route/src/timing.rs`) and timing-driven routing. Cell delays
     are Quartus' DMF values (cyclone2-re docs/timing-model.md), routing
     delays quartus_sta's median per element type; register-to-register
     paths only, like Quartus with just a clock constraint. After each
     PathFinder iteration the router runs STA and costs each connection
     `(1 - crit) * congestion + crit * delay`. `bench/quartus-timing.sh`
     has Quartus time a chipsmith routing.

     picorv32, same placement, Quartus' data delay on the critical path:

     | routing | chipsmith estimate | Quartus |
     |---|---|---|
     | congestion only | 9.12 ns (before the carry fixes below) | 10.71 ns |
     | timing-driven, carry chains missing from the model | 7.62 ns | 10.68 ns |
     | timing-driven, carry chains in, carry delay by LE position | 9.45 ns | 9.98 ns |
     | + A* distance from each wire's bounding box | 9.45 ns | 9.82 ns |

     Per connection, Quartus' routing delay is 1.07x the model's (median
     over 564 connections). The critical path is now a 27-LE carry chain
     (2.5 ns), 5 LUTs (1.8 ns) and 5 routed connections (4.7 ns, about
     0.95 ns each), so the next gain is timing-driven placement. Routing
     picorv32 takes 16.7 s, up from 6.6 s: critical nets are rerouted every
     iteration, and the box distance needs a stronger A* weight (1.5) to
     search as narrowly as the old single-tile one.
   - Timing-driven placement. The placer builds the same timing graph,
     estimates each connection's delay from the placement (LE buffer and
     LAB line plus R4/C4 delay per LAB crossed), reruns STA every
     temperature and weights each net's wirelength by
     `1 + 8 * crit^4`. picorv32 critical path in quartus_sta: 9.82 ->
     8.79 ns (11.23 ns of slack at 50 MHz; was 2.25 ns with nextpnr).
     Weight/exponent sweep, one seed each: 8/4 8.79 ns, 32/8 8.81 ns,
     16/4 8.94 ns, 8/2 9.35 ns.
   - Packer (done). `crates/chipsmith-cyclone2/src/pack` is the `pack()` of cyclone2-re's nextpnr
     uarch in Rust: pin buffers named after their port bits, constants
     folded into LUT masks and pins, stuck-at-GND registers and input-less
     LUTs removed, one-input LUTs folded into their loads (inversions into
     masks, `DATA_INVERT`, `ENA_INV`/`SCLR_INV`; pushed back into the driver
     or made an inverted copy of it in front of a register), registers
     paired with their LUT or put on the sync-load path, carry chains in
     Quartus' polarity, a clock control per clock (the placer picks which),
     M4K blocks on their physical pins.
     Some rules depend on visiting order (folding one buffer can make
     another LUT a buffer; the first register to claim a LUT gets it), so
     the netlist iterates the way nextpnr's containers do and imports yosys'
     JSON the way its frontend does; net ids come out in nextpnr's order,
     which is the order the router routes in.
     `bench/pack-equiv.sh` packs a netlist with both and compares
     (`chipsmith pack-diff`: cells by name, type, parameters, every
     connection by net name, net id order):

     | design | cells | nextpnr `--pack-only` vs chipsmith | full-flow RBF vs nextpnr-packed flow | Quartus (`bench/quartus-timing.sh`) |
     |---|---|---|---|---|
     | blinky | 58 | identical, same net order | identical | byte-identical; slack 16.65 ns, data delay 3.39 ns |
     | lfsr | 173 | identical, same net order | identical | byte-identical; slack 13.39 ns, data delay 6.68 ns |
     | uart_tx | 152 | identical, same net order | identical | byte-identical; slack 14.46 ns, data delay 5.56 ns |
     | picorv32 | 1,459 | identical, same net order | identical | byte-identical; slack 11.23 ns, data delay 8.79 ns |
     | examples/blinky-verilog, blinky-vhdl | 58 | identical, same net order | identical | (same design as blinky) |
     | bidir (yosys `abc -lut 4`, and chipsmith's LUT mapper) | 99 | identical, same net order, same ports | not run | byte-identical; slack 16.64 ns, data delay 3.39 ns (examples/bidir) |
     | examples/ram (7 M4Ks) | 241 | identical, same net order | not run | byte-identical; slack 16.21 ns, data delay 3.83 ns (estimate 16.20 ns) |
     | examples/ram_widths (10 M4Ks) | 171 | identical, same net order | not run | byte-identical; slack 14.80 ns, data delay 5.22 ns (estimate 15.24 ns) |
     | examples/ram-leds (2 M4Ks) | 128 | identical, same net order | not run | byte-identical; slack 14.80 ns, data delay 5.28 ns (estimate 15.11 ns) |

     Packing picorv32 takes 0.4 s. A design exercising the rarer rules
     (register inits of 1, active-low enables and clears, constant and
     inverted outputs) packs identically too, and so does bidir (10
     `CY2_IOBUF`s: constant data and enables, inverters folded into
     `DATA_INVERT`/`OE_INVERT`, unread inputs switched off). `pack-diff`
     compares top-level port directions too, since the bitstream decides
     output vs bidir mode by them. Rechecked 2026-10-24 with cyclone2-re
     5eff911 (blinky, lfsr, uart_tx, picorv32 both mappers: identical),
     and for the M4K examples with 9cfe9be (`pack_m4k`: logical ports onto
     the block's pins, buffers in front of its inputs folded, its clock
     through a clock control).
     Parameters nextpnr writes as
     `0` (a flag toggled back) chipsmith leaves out; they read the same.
4. **Own synthesis** (done for the DE2, in the build since 2026-10-25):
   techmap and LUT mapping on the IR. yosys' JSON stays the netlist format
   between stages (ADR-0004).
   - Netlist IR (done, 2026-10-25): `crates/chipsmith-netlist`, in
     yosys' RTLIL cell vocabulary, with typed cells for the families
     synthesis matches on (ADR-0004, ADR-0005). It reads and writes
     yosys' JSON. The tests run 19 designs (the examples, the flash
     loader, eight cyclone2-re specimens, one instance of every typed
     RTLIL cell, every cell in yosys' `simcells.v`) through up to four
     levels (`proc`, `synth -run :fine`, `techmap; opt`, `cyclone2.ys`),
     64 netlists in all. For each one, read-write-read gives the same IR,
     yosys reads our file to the same design as its own, and yosys'
     `equiv_*` passes prove the two equivalent. picorv32 after
     `techmap; opt` (8,417 cells, 5.6 MB) reads in 137 ms and writes in
     73 ms.
   - LUT mapping (done, 2026-10-24). First switched on between two yosys
     runs: yosys ran `cyclone2.ys` up to `abc -lut 4` and wrote
     `gates.json`, chipsmith mapped that, and a second yosys ran the rest
     of the script from `opt -fast` on. Now it runs inside chipsmith's
     synthesis (below). Measured
     2026-10-24, the same design through both flows; slack is from
     `bench/quartus-timing.sh` (byte-identical assembly in every row):

     | design | LUT mapper | mapped LUTs | packed LUTs (LEs) | Quartus slack @ 50 MHz |
     |---|---|---|---|---|
     | picorv32 | yosys `abc -lut 4` | 770 | 992 (1,105) | 11.23 ns |
     | picorv32 | chipsmith | 842 | 1,062 (1,175) | 11.08 ns |
     | blinky-verilog, blinky-vhdl | either | 0 (all carry) | 26 (26) | 16.65 ns, same RBF |

     `bench/lutmap-vs-abc.sh <dir>` maps one `gates.json` (from
     `de2_synth --yosys`) both ways
     (LUTs after `opt -fast`). On picorv32, abc's mapper alone
     (`strash; if -K 4`) gives 847 LUTs and chipsmith's 842, so the
     mapping itself is on par. The 9% gap comes from abc's logic
     optimisation around its mapper: choice nodes (`dch -f`, 810 LUTs)
     and don't-care resynthesis (`mfs2`, `lutpack`, 788). A choice-aware
     mapper is the next gain. More cuts per node (16, 32), more recovery
     passes, or relaxing depth by a level didn't help (844 to 847). An earlier comparison (842 vs 847) had
     counted abc's 75 one-input buffer LUTs, which `opt -fast` removes.
   - Synthesis (done, 2026-10-25, the DE2 build's since then):
     `chipsmith_cyclone2::synth::synthesize` takes the front end's netlist
     (or yosys' `proc; flatten`) to the cells `cyclone2.ys` makes, in place
     of both yosys halves: flatten, tri-states (`chipsmith_synth::tribuf`),
     memories collected and their read registers merged where an M4K takes
     them, undefined values resolved once (ADR-0007), state registers
     compared by the bits that tell their values apart (`fsm`), the `opt`
     loop (constants and merging on gates, `$mux`/`$pmux` folding,
     `opt_muxtree`, `opt_dff`), registers stuck at their power-up value
     found as a fixed point (`opt::seqconst`), `wreduce`, `alumacc`, M4Ks
     (`synth/m4k.rs`, above), the other memories to
     registers, carry chains (`arith_map.v`), lowering to gates, the LE
     register (`dfflegalize` to the six types, `cells_map.v`), the LUT
     mapper on the IR (same LUTs and depth as on yosys' JSON), pin buffers
     (`iopadmap`). Measured with chipsmith's front end, packer, placer and
     router (`cargo run --release --example de2_synth`, `--yosys` for the
     yosys flow as the build ran it before):

     | design | LUTs | registers | LEs | carry chains | M4Ks | Fmax (estimate) |
     |---|---|---|---|---|---|---|
     | picorv32 | 1,058 (yosys flow 1,062) | 431 (434) | 1,171 (1,175) | 8 (8) | 0 | 123.4 MHz (114.4) |
     | ram | 151 (151) | 32 (32) | 151 (151) | 1 (1) | 7 (7) | 263.2 MHz (263.2) |
     | ram_widths | 78 (78) | 32 (32) | 78 (78) | 1 (1) | 10 (10) | 263.2 MHz (210.0) |
     | ram-leds | 40 (40) | 34 (34) | 42 (42) | 2 (2) | 2 (2) | 198.5 MHz (204.3) |
     | bidir | 28 (28) | 27 (27) | 29 (29) | 1 (1) | 0 | 296.5 MHz (296.5) |
     | blinky-verilog, blinky-vhdl | 26 (26) | 26 (26) | 26 (26) | 1 (1) | 0 | 296.5 MHz (296.5) |
     | persist loader | 3 (3) | 0 (0) | 3 (3) | 0 | 0 | no clock |

     The M4K cells of ram, ram_widths and ram-leds (mode, width, contents,
     which pins are tied) are the yosys flow's, cell for cell. picorv32 synthesizes in 1 s (yosys 14 s).
     `crates/chipsmith-cyclone2/tests/synth.rs` proves every example, the
     loader and five specimens sequentially equivalent to yosys' RTL of the
     same Verilog, every stage on three of them, and that a flipped LUT
     bit fails the proof. The check is chipsmith-netlist's `equiv_*`
     script in two steps (ADR-0007): the RTL against the netlist with x
     resolved, then that against the result, with the registers synthesis
     made constant (989 on picorv32) and its state registers' value sets
     first proven invariant by `sat -tempinduct`; memories in M4Ks are
     compared at their ports in the first step and word by word in the
     second (the blocks through a model of the block). All 174 plain-Verilog cyclone2-re specimens pass it and
     the packer.
   - In the build (2026-10-25): one `synthesis` step (front end, the cell
     library `crates/chipsmith-cyclone2/src/synth/cells.v`, synthesis,
     netlist JSON) replaced both yosys runs and the LUT mapping step. Every
     DE2 example and the flash loader build to the same RBF as `de2_synth`,
     byte for byte; for blinky-verilog, blinky-vhdl and the loader that is
     also the RBF the yosys flow built. picorv32 builds in 42 s (41.6 s CPU), against 50 s
     (50.0 s CPU) through yosys. The nix
     package carries no cyclone2-re techmap any more, and yosys only for
     the Zybo.
   - Gaps closed (2026-10-27). What made synthesis stop is now built:
     latches (a LUT feeding its own output back, as Quartus 13.0sp1 does on
     this family, and with Quartus' warning that their power-up value is
     undefined) and registers with an asynchronous load or set and reset
     (Quartus' emulation: a latch, an LE register holding `D ^ latch`, a
     LUT choosing; ADR-0009); `/` and `%`, signed and unsigned, a power of
     two as wiring, constant divisors with only the stages they need. A
     sum of three or more terms is a balanced tree of carry chains: four
     18-bit terms run at 204 MHz (137 MHz as a chain; the yosys flow's
     `$macc` 186 MHz with 123 LUTs against 90). The front end takes
     multi-dimensional arrays and an asynchronous reset tested inside a
     named block, with memories written under such a reset made registers.
     Re-evaluating the exclusive `$pmux` trade (ADR-0007): masking a
     select only by the later selects it can overlap maps picorv32 to the
     same 1,058 LUTs, so the trade stays as it was. Every DE2 example builds
     to the RBF it built before except bidir: an output reading an inout
     (`assign LEDR = GPIO_0`) became a tri-state copy of its driver and
     floated while the pin was released; it now reads the pin's input
     buffer. The checker hadn't caught that (tri-states were opaque cells);
     it now cuts every pin into an outside input and `EN ? A : outside` on
     both sides.

     A corpus of open-source cores, unmodified behind DE2 wrappers
     (`examples/<name>`, upstream and commit in each manifest), each proven
     equivalent to yosys' RTL. LEs and Fmax, chipsmith's synthesis against
     the yosys flow (`de2_synth --yosys`; both through chipsmith's back
     end, Fmax its estimate, which runs up to 11% optimistic on the larger
     ones, see the timing table above) and Quartus 13.0sp1's own synthesis
     and fit (its STA, 50 MHz constraint):

     | design | chipsmith | yosys flow | Quartus |
     |---|---|---|---|
     | uart (Forencich) | 190 LEs, 165.3 MHz | 225 LEs, 154.2 MHz | |
     | spi-master (nandland) | 179 LEs, 212.3 MHz | can't build: an asynchronous reset to 1 that powers up 0 | |
     | i2c-master (Forencich) | 225 LEs, 127.9 MHz | 245 LEs, 118.6 MHz | |
     | femtorv32 quark | 788 LEs, 72.8 MHz | 832 LEs, 87.5 MHz | 473 LEs + 1 M4K, 83.7 MHz |
     | serv | 3,091 LEs, 84.3 MHz | 3,111 LEs, 78.6 MHz | 336 LEs + 1 M4K, 108.6 MHz |
     | sha256 (secworks) | 6,053 LEs, 38.1 MHz | can't build (as spi-master) | 3,303 LEs, 63.7 MHz |
     | picorv32 | 1,171 LEs, 123.4 MHz | 1,175 LEs, 114.4 MHz | |

     Where Quartus is far ahead, it is the register files (serv's and
     femtorv32's go into an M4K; chipsmith's M4K mapping doesn't take their
     shapes yet) and wide multiplexers: sha256's 16-word schedule read maps
     to about 20 LUTs a bit, against Quartus' 11 (the mapper, not the
     netlist: the same read as a `$bmux` tree maps no better). Still
     rejected: arrays declared inside functions (Forencich's `lfsr.v`,
     dropped from the corpus), `$pow` with a non-constant base, the yosys
     flow's own limits above.
5. **Own Verilog frontend** (done for the DE2, in the build since
   2026-10-25): the synthesizable subset that GHDL and HDL generators emit.
   - Elaboration (done, 2026-10-25, the DE2 build's front end since then): `crates/chipsmith-verilog`
     reads Verilog to the netlist yosys has after `read_verilog; hierarchy;
     proc`, and `chipsmith_synth::flatten` flattens it. Every example
     (picorv32, the RAMs, bidir, the Zybo designs, the persist loader),
     GHDL's translation of the VHDL ones and all 250 of cyclone2-re's
     specimens are proven sequentially equivalent to yosys' front end
     (`crates/chipsmith-verilog/tests`). Through `cyclone2.ys` the DE2
     examples give the same cells as with yosys' front end, except
     picorv32: 989 LUTs instead of 993 (ADR-0006). picorv32 elaborates in
     0.1 s, against 1.2 s for yosys.

6. **Persistent flashing**: `chipsmith flash --persist` writes the board's
   configuration flash so the design survives a power cycle. `flash` as it
   is loads configuration RAM over JTAG (fast, gone at power-off).
   - DE2 (done, 2026-10-24). The EP2C35 configures itself from its EPCS16
     at power-up (active serial, RUN/PROG switch in RUN, where it normally
     is; PROG isn't needed). chipsmith writes the EPCS over the JTAG cable
     through a serial flash loader of its own: `crates/chipsmith-cyclone2/src/persist/loader`, a
     JTAG-to-SPI bridge on the FPGA's flash pins, built by chipsmith (the
     nix package prebuilds it). It uses the Cyclone II's JTAG and
     serial-flash hard blocks (`CY2_JTAG`, `CY2_ASMI`, `crates/chipsmith-cyclone2/src/hard.rs`),
     which cyclone2-re's chip database and cram gained for this; Quartus
     assembles chipsmith's loader byte-identical. With USER0 in the IR each
     Shift-DR is one SPI transaction, so the loader is three LUTs and no
     clock. The image is what Quartus writes for an EPCS16 (the RBF with
     the passive-serial option bit cleared, each byte bit-reversed; the
     factory image is stored the same way). After writing, chipsmith reads
     the image back, then sends `PULSE_NCONFIG` (IR 0x001), which makes the
     FPGA reconfigure from the EPCS as at power-up, and checks CONF_DONE
     and the USERCODE. So `flash --persist` ends with the board running the
     design from flash.

     Observed on the DE2: blinky-verilog and ram-leds written (856,076
     bytes, about 50 s, half of it erase and page-program polling),
     verified, and running from the EPCS after `PULSE_NCONFIG` (camera:
     counter, then the bouncing light). A dump of the whole flash, read
     twice and compared (`chipsmith flash-dump`, hidden, 2 MB in 134 s),
     was saved before anything was erased. Writing it back
     (`chipsmith flash-restore`, hidden) brought the factory design up
     again from flash (USERCODE 0x0042b391), so the factory image is
     restorable. After a real power cycle (2026-10-26) the board came up
     from the EPCS by itself: examples/de2-jtag-counter read back `"DE2!"`
     counting at 50.00 MHz over JTAG.
   - Zybo (done, 2026-10-24, through Linux). With NixOS on the board,
     nixos-fpga's `hardware.fpga.bitstream` loads the build's `.bin` at
     every boot ("Loading a Zybo from its own Linux", above), so
     `--persist` with `--ssh` points there instead of writing flash.
     Without Linux, persistence means writing a boot image to QSPI, which
     is still not done:

     The QSPI flash hangs off the Zynq's processor (PS
     MIO 1-6); the PL can't reach it, so a PL bridge like the DE2's can't
     work. openFPGALoader refuses to write Zynq flash for that reason;
     Vivado's `program_flash` runs an FSBL plus a U-Boot helper on the ARM
     core over the debug port (traced for the ZynqMP in openFPGALoader
     issue 654). The PS BootROM loads only a first-stage
     loader from QSPI, never a bitstream, so a PL-only design still needs
     a boot image: a BootROM header, a first-stage loader in OCM, the
     bitstream. It doesn't need Xilinx's FSBL. The honest minimum is three
     pieces chipsmith doesn't have yet:
     1. ARM debug access over JTAG: the ADIv5 DAP on the Zybo's chain
        (IR 4, beside the xc7z010), DPACC/APACC scans, AHB-AP memory reads
        and writes. With that the host can drive the PS's QSPI controller
        (0xE000D000; MIO and LQSPI clock set up in the SLCR) in I/O mode
        and program the 128 Mbit QSPI flash itself, with no code on the ARM.
     2. A first-stage loader of our own, built with an open ARM toolchain
        from nixpkgs: about 40 instructions that start a PCAP DMA from the
        QSPI linear window (0xFC000000 + offset) into the PL and wait for
        DONE. No DDR or PLL setup for a PL-only design.
     3. The BootROM header (UG585 section 6.3: width word, `XLNX`, loader
        offset, length, checksum), written by chipsmith; bootgen isn't
        needed for an unencrypted image.
     The board then also has to boot from QSPI: the boot-mode jumper JP5
     must be on QSPI, which only someone at the board can set (chipsmith
     can't read it until piece 1 exists: it's SLCR BOOT_MODE,
     0xF800025C). A PS soft reset over the DAP (SLCR PSS_RST_CTRL) would
     then rerun the BootROM without a power cycle. On the Zybo JPROGRAM
     only clears the PL; the PL is the PS's slave, so it doesn't reload
     from flash the way a standalone 7-series in master mode does.
     The SD card is the other path (BOOT.bin on FAT32, same boot image
     and jumper).

Next devices:

- **Zybo** (Zynq-7000, 7-series fabric): it builds and loads (above).
  Project X-Ray already documents the fabric, so owning the stages means
  importing prjxray-db, not reverse engineering. xc7-re holds the
  groundwork. Same order as for Cyclone II: router, placer, packer.
  The flow: yosys; nextpnr packs (`--pack-only`); chipsmith places;
  nextpnr routes that placement (`-o preplaced`, nothing left to place)
  for the site routing; chipsmith reroutes the interconnect; nextpnr
  writes the FASM and times it (`--report`).
  - Router (done, interconnect only). The routing graph comes from
    prjxray-db (`tilegrid.json`, `tile_type_*.json`, `tileconn.json`, and
    `segbits`/`ppips` for which pips a bitstream can express), into the
    same `ChipDb` as the Cyclone II graph. A nix derivation
    (`xc7-routing-graph`) prebuilds it: xc7z010 has 13,440 tiles of 90
    types, 3,607,758 tile wires in 1,200,334 nodes and 12,346,252 pips.
    The import takes 7.5 s in the nix build (15 s on a busy run), the
    180 MB cache 2.0 s to load at a load
    average of 18 (up to 8 s on the busier runs). Pips are
    taken as nextpnr's generator takes them: bidirectional ones become one
    pip each way, and pips with no FASM feature are dropped. Pseudo pips
    (route-throughs of a LUT, IOB or OLOGIC) are dropped too, since they
    need a free bel and site configuration the router doesn't model.

    nextpnr's xilinx graph has the sites inside it, and chipsmith's doesn't
    yet. So the flow is: nextpnr routes (chipsmith's placement, below); chipsmith keeps
    everything of nextpnr's routing that isn't an interconnect pip (site
    pips, the LUT pin nextpnr permuted a signal onto, route-throughs) and
    reroutes the rest with the Cyclone II's PathFinder, between the site
    pins nextpnr chose. nextpnr then packs the netlist again with each cell
    pinned to its bel (`-o preplaced`) and every net locked to chipsmith's
    route (`-o prerouted`), and writes the FASM. On both designs below it
    bound all of chipsmith's routes unchanged (0 collided; the routes it
    writes back are pip for pip the ones given), and its router had
    nothing left to route. `chipsmith_xc7::route` also checks every route is a tree
    of graph pips reaching all its sinks, with no node shared.

    Same placement, nextpnr's router2 against chipsmith's (routing is
    congestion-driven only: no 7-series delay model yet). Pips are
    interconnect pips of the prjxray graph; Fmax is nextpnr's own STA of
    each routing:

    | design | router | time | pips | iterations | Fmax (nextpnr STA) |
    |---|---|---|---|---|---|
    | zybo-jtag-counter | nextpnr router2 | 142 s | 1,790 | 6 | 317.2 MHz |
    | zybo-jtag-counter | chipsmith | 4.6 s | 1,764 | 2 | 312.0 MHz |
    | picorv32 (Zybo pins, not for hardware) | nextpnr router2, in the build | 154 s | 16,443 | 8 | 181.3 MHz |
    | picorv32 | nextpnr router2, again on that placement | 37.8 s | 16,356 | 9 | 166.3 MHz |
    | picorv32 | chipsmith | 39.7 s | 16,259 | 4 | 143.5 MHz |

    The machine was shared and loaded throughout (load averages of 10 to
    70 on 8 cores), so times are rough: chipsmith's picorv32 routing took
    39.7 s to 69 s across runs. The second nextpnr picorv32 row and
    chipsmith's ran back to back at a load average near 10. nextpnr
    spends most of the counter's 142 s on the four nets from the BSCANE2
    in the configuration tile (24 to 38 s each). Its two picorv32 routings
    of one placement differ by 9% in Fmax. The A*
    distance weight is device data (`ChipDb::tile_distance`); on
    picorv32, per tile in x/y:

    | weight | time | pips | Fmax |
    |---|---|---|---|
    | 0.125/0.25 | 69.6 s | 16,150 | |
    | 0.25/0.5 (default) | 39.7 s | 16,259 | 143.5 MHz |
    | 0.5/1 | 19.3 s | 16,856 | 127.7 MHz |
    | 1/2 | 10.9 s | 17,885 | |

    Both designs' bitstreams build (fasm2frames, xc7frames2bit), and
    zybo-jtag-counter runs on the Zybo (above). Vivado 2026.1 (xc7-re
    `tools/oracle.sh`, its own placement and routing of the same Verilog,
    8 ns clock) has the counter's clock domain at WNS 5.353 ns, a 2.647 ns
    critical path (377.8 MHz), where nextpnr's STA estimates 317 MHz on
    nextpnr's placement. Vivado can't take chipsmith's routing (that needs
    LOC/BEL constraints and `FIXED_ROUTE` per net), so this compares
    tools, not routings.

    Open: timing-driven routing (prjxray-db's pip and wire delays into
    `timing.rs`), which picorv32's Fmax gap calls for; the site graph,
    so chipsmith can choose site pins and LUT pin permutations itself and
    no longer needs nextpnr's routing to start from; and FASM from
    chipsmith's own writer.
  - Placer (done). chipsmith places nextpnr's packed netlist (the cells
    its packer writes with `--pack-only`: SLICE_LUTX, SLICE_FFX, CARRY4,
    RAMD32, PAD, IOB33 buffers, BUFGCTRL, BSCAN) and hands every cell's
    bel to nextpnr as `-o preplaced` lines. nextpnr binds all of them and
    places nothing itself; `chipsmith_xc7::route` checks that each cell is on the
    bel chipsmith gave it before rerouting. Packing names are
    deterministic, so the packer's own cells (split carries, feed-through
    LUTs, pin buffers) match by name in every later nextpnr run.

    Slices are filled in shapes that are legal by construction, a subset
    of nextpnr's `xc7_logic_tile_valid`: a BLE (a LUT in an `x6LUT` and
    the register in the `xFF` beside it, or either alone) goes anywhere,
    and the only per-slice rule left is one control set (clock, CE, SR
    and their modes) per slice; a carry chain owns whole slices up its
    column, following COUT to the next CIN through the prjxray graph (and
    so across the HCLK row), with its S LUTs in the `x6LUT`s, its DI LUTs
    in the `x5LUT`s and registers fed by a sum output in that bit's
    `xFF`; a RAM32M owns a SLICEM. Chains and RAMs take the tile's X0
    slice, as nextpnr's packer constrains them. Pins stay where the XDC
    put them, the BSCAN for USER<n> goes to BSCAN_X0Y<n-1>, each BUFGCTRL
    to the free one nearest its source. Anything else (block RAM, DSPs,
    SRLs, wide muxes, I/O logic, other RAM shapes) leaves the placement
    empty and nextpnr places the design as before. The 5LUTs and 5FFs
    stay unused outside carry chains, so chipsmith fills more slices than
    nextpnr does.

    The annealer is the Cyclone II one's (VPR schedule, range limit,
    half-perimeter wirelength with x at half weight, since grid x counts
    site columns), with moves that swap BLEs, or move a chain or RAM and
    hand the BLEs in its new slices to the slices it left, slice for
    slice, so control sets never mix. Eight anneals run on eight threads
    from one start with fixed seeds, and the cheapest wins: the result
    doesn't depend on the machine.

    Timing-driven: prjxray-db's timing data loads in a few lines. Cell
    delays come from `timings/slicel.sdf` (slow corner, max: LUT 124 ps,
    clock-to-Q 303 ps, every CARRY4 arc), routing from distance, hop by
    hop, with the mean pip delay per wire class of `tile_type_INT_L.json`
    (hexes up and down, quads across, then doubles and singles, plus the
    IMUX pip). Every temperature the placer runs the STA of `timing.rs`
    and weights each net by `1 + 20 * criticality^4`. prjxray's pip delays
    have no wire RC, and the estimate is rough either way: picorv32's
    critical path comes out at 5.93 ns against 5.89 ns by nextpnr's STA
    of chipsmith's routing, jtag-counter's at 4.07 ns against 2.84 ns.
    Only its proportions matter.

    nextpnr's placement (HeAP and SA, in the old flow) against
    chipsmith's, each routed by chipsmith's router. Fmax is nextpnr's STA
    (`--report`) of that routing; wirelength is chipsmith's measure
    (half-perimeter, x halved) of both placements of the same packed
    netlist. Release builds, 8 cores; another job shared the machine.

    | design | placer | wirelength | place time | Fmax, chipsmith's routing | Fmax, nextpnr's router2 |
    |---|---|---|---|---|---|
    | zybo-blinky | nextpnr | 165.5 | 0.7 s | 391.5 MHz | 391.5 MHz |
    | zybo-blinky | chipsmith | 172 | 0.5 s | 391.5 MHz | 391.5 MHz |
    | zybo-jtag-counter | nextpnr | 569 | 1.0 s | 312.0 MHz | 317.2 MHz |
    | zybo-jtag-counter | chipsmith | 523 | 0.7 s | 352.5 MHz | 352.5 MHz |
    | zybo-picorv32 | nextpnr | 7,562.5 | 10.0 s | 143.5 MHz | 181.3 MHz |
    | zybo-picorv32 | chipsmith | 7,484 | 9.2 s | 169.8 MHz | 172.6 MHz |

    blinky is its 7-CARRY4 chain either way. Whole builds, at a load
    average of 5 to 8: blinky 71 s against 55 s, jtag-counter 87 s
    against 66 s, picorv32 110 s against 101 s. The steps that changed
    account for about 3 s of that (the `--pack-only` run takes 2 s, and
    chipsmith's placing about what nextpnr's took); the rest is the load
    (yosys alone took 38 s instead of 17 s on the counter).

    Picking the weights, nextpnr's Fmax on chipsmith's routing (the
    placement changes with the seeds, and one placement's Fmax moves by
    about 10% with it):

    | placer | picorv32 | jtag-counter |
    |---|---|---|
    | wirelength only | 132.6 MHz | 352.5 MHz |
    | timing weight 8 (the Cyclone II's) | 137.3, 125.2 MHz | 366.2 MHz |
    | 8, criticality^2 | 156.6 MHz | 366.2 MHz |
    | 8, wire hops doubled | 112.1 MHz | 351.3 MHz |
    | 20 (default) | 169.8, 156.7, 146.3 MHz | 352.5, 366.2, 366.2 MHz |
    | 20, wire hops doubled | 160.6 MHz | 366.2 MHz |
    | 40 | 148.3, 130.4 MHz | 366.2, 351.3 MHz |
    | 40, criticality^2 | 152.0 MHz | 351.3 MHz |
    | 80 | 163.4 MHz | 352.5 MHz |

    Wirelength alone gives the shortest picorv32 (5,484.5, 27% under
    nextpnr's) and the slowest; its critical path is the reset counter
    into the high-fanout enable of most registers, which only the timing
    weight pulls together. Open: the 5LUTs and 5FFs, a delay model with
    the wires' RC, and chipsmith's own packer, so nextpnr's `--pack-only`
    run goes.
- **DE25-Nano** (Agilex 5): no public reverse engineering exists. First
  check whether the Secure Device Manager accepts a bitstream we write.

## Manifest

```toml
[project]
name = "blinky"            # build/blinky.rbf
top = "blinky"             # module or entity whose ports are pins
sources = ["src/*.vhd"]    # .v or .vhd/.vhdl, one language per design

[target]
device = "EP2C35F672C6"

[pins]
CLOCK_50 = "PIN_N2"
LEDR = ["PIN_AE23", "PIN_AF23", "PIN_AB21", "PIN_AC22"]  # LEDR[0], LEDR[1], ...
```

The device decides how pins are written: `PIN_N2` on Cyclone II, the
package ball (`L16`) on the Zynq. A pin of the wrong form, or one the
package doesn't have as user I/O, is an error when the manifest loads.
The Zynq builds `build/<name>.bit` instead of `.rbf`.

A `[clocks]` table gives the frequency of a clock entering at a pin:

```toml
[clocks]
CLOCK_50 = "50 MHz"        # a port bit with a pin in [pins]
```

On the Zynq each clock becomes an XDC `create_clock` for nextpnr; the
build prints nextpnr's Fmax and slack per clock and fails if one misses
its constraint. On Cyclone II `[clocks]` gives the clocks entering at
pins; a PLL's outputs take their frequency from it, and a clock made by
logic (a divided clock, the JTAG block's TCK) has none. The build prints,
per global clock, the critical path from its registers to its registers,
the estimated slack against its frequency where it has one, and the
longest path between each pair of clocks (not checked: how two clocks
relate is the design's business). It doesn't fail on the slack: the estimate is not sign-off (yosys-synthesized picorv32: 11.26 ns estimated, 10.60 ns in Quartus). The
placer and router stay timing-driven against the critical path itself,
whatever the clock. Without `[clocks]`, nextpnr times against its default
12 MHz and Cyclone II reports no slack.

A DE2 pin, or a whole bus, can be a table with its I/O settings
(`crates/chipsmith-cyclone2/src/pin.rs`):

```toml
[target]
device = "EP2C35F672C6"
unused_pins = "pull-up"    # "tri-stated" (default), "pull-up" or "bus-hold"

[pins]
LEDG = { pins = ["PIN_AE22", "PIN_AF22"], drive = "4 mA", registers = ["output"] }
GPIO_IO = { pin = "PIN_F24", pull = "up", registers = ["input", "output", "output-enable"] }
HEX0 = { pins = ["PIN_AF10", "PIN_AB12"], standard = "3.3-V LVCMOS" }
KEY = { pins = ["PIN_G26", "PIN_N23"], standard = "2.5 V" }
```

- `standard`: `"3.3-V LVTTL"` (the default), `"3.3-V LVCMOS"`, or `"2.5 V"`
  for inputs. The DE2 powers all eight banks at 3.3 V, and Quartus takes
  a 2.5-V input in a 3.3-V bank but nothing at 1.8 or 1.5 V (error
  169029), so `"1.8 V"` and `"1.5 V"` are errors that name the VCCIO they
  would need, and a 2.5-V port that drives is an error once synthesis
  knows its direction.
- `drive`: 3.3-V LVTTL's current, `"4 mA"` to `"24 mA"` (default) in 4 mA
  steps. 3.3-V LVCMOS has its default current only (12 mA on row IOEs,
  24 on column IOEs): cyclone2-re hasn't decoded its other drive codes.
- `pull`: `"up"` (the weak pull-up) or `"bus-hold"`; one key, so a pin
  can't ask for both. Dedicated clock inputs have neither.
- `registers`: the I/O-cell registers synthesis moves the design's
  registers into, any of `"input"`, `"output"`, `"output-enable"`.
  Quartus' `FAST_INPUT_REGISTER` and friends.
- `unused_pins`: what pins the design doesn't use do. Quartus' "as output
  driving ground" isn't offered: on the DE2 it would short every chip
  output wired to an unused FPGA pin.

I/O registers are opt-in, not automatic. Quartus moves registers into
I/O cells on its own only when that helps the timing it is given for the
pins (`OPTIMIZE_IOC_REGISTER_PLACEMENT_FOR_TIMING`); chipsmith has no pin
timing constraints, so it can't tell. A register in the I/O cell fixes the
pin's clock-to-output or setup time but moves the register to the chip's
edge, which lengthens the path from the logic that feeds it; only the
designer knows which side matters. A register moves when it is a plain
rising-edge register (no enable, no reset): an input register is the one
sampling the pin, moved; an output or OE register is a copy of the one
driving the pin's data or enable, so a register several pins share (a
bus's enable) gets one copy per pin, and the original stays if logic reads
it. Its power-up value goes with it. A request synthesis can't satisfy is
an error that says why. The timing report then times the paths into and
out of those registers; a pin without them stays untimed.

Unknown keys are errors, so a misspelt table can't be silently ignored.

## Development

```
nix develop
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -- -C examples/blinky-verilog build
```

A Cargo workspace, one crate per concern
([ADR-0002](docs/adr/0002-cargo-workspace-by-concern.md)):

| crate | |
|---|---|
| `crates/chipsmith` | the binary: manifest, devices, the flow (`Plan`, `Step`), CLI |
| `crates/chipsmith-synth` | device-independent synthesis: flattening, RTL optimisation, arithmetic, memories, lowering to gates, LUT mapping |
| `crates/chipsmith-route` | what both back ends share: the routing graph (`ChipDb`), the PathFinder router, the timing graph |
| `crates/chipsmith-cyclone2` | Cyclone II: synthesis to its cells, packing, placement, routing (nextpnr JSON in and out), M4K, hard blocks, EPCS and its serial flash loader |
| `crates/chipsmith-xc7` | 7-series: prjxray-db import, placement, delays, routing, Zynq `.bin` |
| `crates/chipsmith-jtag` | cables (USB-Blaster, FT2232), SVF player, 7-series JTAG loader |

Cargo examples sit with their crate and run from the root by name, e.g.
`cargo run --release --example xc7_user1` (`chipsmith-jtag`).

The Quartus toolchain manager that used to be chipsmith lives on the `main`
branch.

## License

MIT
