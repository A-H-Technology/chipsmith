# chipsmith

An open FPGA toolchain: HDL in, programmed chip out, with no vendor software.

## Flow

**Project**:
A directory with a `chipsmith.toml` manifest naming the sources, the device, the pins and the clocks.
_Avoid_: design (when the directory is meant), workspace

**Manifest**:
The `chipsmith.toml` that describes a project.

**Build**:
Turning a project's HDL into a bitstream.
_Avoid_: compile, implement

**Flash**:
Putting a bitstream onto a board, volatile (configuration RAM) or persistent (configuration flash).
_Avoid_: program, download, upload

**Plan**:
Every file a build or flash writes and every step it runs, decided before any of it happens.

**Step**:
One stage of a plan: an in-process stage or an external tool.

## Synthesis

**Elaboration**:
Turning parsed Verilog into a netlist: parameters evaluated, generate blocks expanded, one module per parameter set, procedures lowered to RTL cells.

**Netlist**:
A design as cells connected by nets, at any level from RTL to device cells.

**Net**:
One bit of a module's connectivity: every port bit on the same net is connected.
_Avoid_: wire (when a net is meant)

**Wire**:
A name for some nets, and constants, such as a port or a Verilog `wire`. Several wires can name the same net.

**RTL cell**:
A word-level operation or storage element (adder, multiplexer, register, memory) before mapping.

**Device cell**:
A cell the target device has: a LUT, a register, a carry, a RAM block, a pin buffer.
_Avoid_: primitive (when a device cell is meant)

**LUT mapping**:
Covering a netlist's logic with the device's look-up tables.

## Cyclone II

**LE**:
Logic element: one 4-input LUT and the register it can feed, plus carry logic.
_Avoid_: slice, cell (for the Cyclone II)

**LAB**:
Logic array block: 16 LEs that share clock, enable and clear signals.

**M4K**:
A 4,608-bit RAM block.

**Embedded multiplier**:
One of the 35 blocks that multiply 18 x 18 (or two 9 x 9) with optional operand and product registers; `CY2_MULT` as a cell.
_Avoid_: DSP block (the Cyclone II's has no adder), mac (except as Quartus' atom names)

**Global clock**:
One of the Cyclone II's 16 clock networks, driven by a clock control from a clock pin, a PLL output or logic.
_Avoid_: GCLK (except as Quartus' name)

**Clock control**:
The block in front of a global clock that picks its source and can gate it.
_Avoid_: clkctrl (except as a cell or file name)

**PLL**:
Phase-locked loop: makes new clock frequencies from a clock pin's (four on the EP2C35).

**RBF**:
Raw binary file: a Cyclone II bitstream.

**EPCS**:
The serial configuration flash a Cyclone II loads itself from at power-up.

**Chip database**:
The reverse-engineered description of a device's sites, routing and configuration bits, produced outside chipsmith.
_Avoid_: chipdb (except as a file name), device model

## 7-series and Zynq

**PL**:
The Zynq's programmable logic, the FPGA fabric.
_Avoid_: fabric (when the Zynq's is meant)

**PS**:
The Zynq's processor system: the ARM cores and their peripherals.

**Slice**:
A 7-series logic site: four 6-input LUTs, eight registers, a carry chain.

## Cables

**Cable**:
A JTAG adapter chipsmith drives: the USB-Blaster or an FT2232.
_Avoid_: programmer, adapter
