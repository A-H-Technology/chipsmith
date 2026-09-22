# chipsmith

chipsmith turns a declarative description of an FPGA design into a programmed
chip. It owns everything between a text file and a blinking LED: acquiring the
vendor toolchain, generating the vendor's project files, running the design's
testbenches, driving the compile, and reporting whether the result actually
meets timing.

## Language

### The design

**Manifest**:
The `chipsmith.toml` that declares a Project completely. The single source of
truth; nothing about a design is configured anywhere else.
_Avoid_: config, settings, project file (that last one means something else here)

**Project**:
One FPGA design — its sources, its Target, its Pin Assignments, its Clock
Constraints. One Manifest describes exactly one Project.
_Avoid_: app, package, crate

**Target**:
The chip a Project is compiled for: a device family and a specific part number.
_Avoid_: platform, board, chip

**Top-level entity**:
The HDL entity that forms the Project's outermost boundary — the one whose ports
are wired to physical pins.
_Avoid_: main, root module, entrypoint

### Acquiring the toolchain

**Toolchain**:
A vendor's installed FPGA compiler and its tools. chipsmith downloads, installs
and invokes Toolchains; it never ships one.
_Avoid_: SDK, compiler, tooling

**Backend**:
The name a Manifest uses to select a Toolchain implementation, e.g.
`quartus-prime`. One Backend name resolves to exactly one Toolchain.
_Avoid_: driver, provider, plugin

**Product**:
A line of Quartus releases that share an install layout, an installer
invocation, and a Spawn Strategy — currently Quartus Prime and Quartus II. Two
Backends over one Product line differ only in their Product description.
_Avoid_: flavour, variant, edition

**Version**:
A key identifying one release of a Product, e.g. `23.1` or `13.0sp1`. Each
Version names its download, its install subdirectory, and its Device Support.
_Avoid_: release, tag

**Install Root**:
The directory a Product installs its Versions under, e.g. `~/intelFPGA_lite`.
Each Version occupies a subdirectory of its Product's Install Root.
_Avoid_: prefix, home, install path

**Device Support**:
A per-family package of device data that a Toolchain needs before it can compile
for that family. Installed alongside a Version, separately from it.
_Avoid_: device pack, family support, PDK

### Compiling

**Build Plan**:
A complete, inert description of a build: every file to write, every Build Step
to run, and where each artifact will land. Producing one performs no work.
_Avoid_: build config, pipeline, recipe

**Build Step**:
One invocation of one Toolchain tool, with a human-readable label. Steps run in
order; the design is not built until all of them have.
_Avoid_: stage, task, job

**Build Directory**:
The directory a Build Plan materialises into. Disposable: deleting it costs only
compile time, never source.
_Avoid_: output dir, target dir, workspace

**Bitstream**:
The programming file a successful build produces (`.sof` for Quartus). The thing
that actually gets loaded onto the chip.
_Avoid_: binary, image, firmware, sof (as a noun in prose)

**Build Outcome**:
What a finished build yields: the Bitstream, plus the Timing Summary if one was
produced. Distinct from success — a build can succeed and still miss timing.
_Avoid_: build result, report

### Simulating

**Testbench**:
A top-level HDL entity with no ports that instantiates the design, drives it,
and asserts on what it sees. Declared in `[sim]`; never part of the synthesised
design.
_Avoid_: test, unit test, bench, stimulus file

**Simulator**:
A tool that elaborates a Testbench and runs it. chipsmith supports GHDL, which
it looks up on `PATH`, and the one bundled with the Project's Quartus. Unlike a
Toolchain, chipsmith never installs a Simulator on its own account.
_Avoid_: sim, simulation tool, HDL runtime

**Work Library**:
The directory a Simulator analyses sources into. Disposable in the same way as
the Build Directory, and every Simulator runs with it as the working directory
so nothing lands in the Project.
_Avoid_: work dir, library, sim output

**Test Verdict**:
What one Testbench did: passed, or failed with whatever the Simulator said. A
Testbench that will not elaborate has a Verdict; sources that will not compile
do not, because no Testbench got that far.
_Avoid_: test result, status, outcome (that last one means something else here)

**Test Report**:
Every Test Verdict from one run, plus the name of the Simulator that produced
them. A run passes only if every Verdict did.
_Avoid_: test summary, results

### Constraints

**Pin Assignment**:
The binding of a named signal in the design to a physical pin on the package.
A signal maps to one pin, or to an ordered list of pins if it is a bus.
_Avoid_: pinout, mapping, IO assignment

**I/O Standard**:
The electrical signalling standard applied to a Pin Assignment, e.g.
`3.3-V LVTTL`. Declared once for the Target and overridable per signal.
_Avoid_: voltage, IO level, drive standard

**Clock Constraint**:
A declared frequency for a clock port, which becomes the period the timing
analyser holds the design to. Without one, timing analysis reports nothing
meaningful.
_Avoid_: clock spec, timing constraint (too broad), frequency

**Settings File / Project File / Constraints File**:
The three files a Build Plan writes for Quartus: `.qsf` carries assignments and
source list, `.qpf` names the revision, `.sdc` carries Clock Constraints. Always
generated, never hand-edited, never checked in.
_Avoid_: QSF/QPF/SDC as bare acronyms in prose

### Timing

**Timing Corner**:
One analysed combination of process, voltage and temperature, with the Slack it
produced. A design is analysed at several Corners.
_Avoid_: PVT, scenario, case

**Slack**:
Nanoseconds of margin at a Timing Corner. Negative Slack means the design does
not run reliably at the constrained frequency.
_Avoid_: margin, headroom, delta

**Timing Summary**:
The set of Timing Corners from one analysis. Its Worst Corner — the lowest Slack
— decides whether the design meets timing.
_Avoid_: timing report, STA result

**Meets timing**:
Every Timing Corner has non-negative Slack. A design that does not meet timing
still produces a usable Bitstream; it just isn't trustworthy at speed.
_Avoid_: passes, is clean, is green

### Running on the host

**Nix Compat**:
The resolved set of nix store paths and tools needed to run a vendor's
FHS-assuming binaries on NixOS. Acquired once, used for every spawn.
_Avoid_: nix shim, FHS wrapper, nix env

**Spawn Strategy**:
How a Product's binaries must be launched on NixOS: patch the ELF interpreter
in place, or sandbox with bubblewrap so a 32-bit loader appears at `/lib`.
_Avoid_: launch mode, exec method, compat mode

**Cable**:
A JTAG programmer connecting the host to the chip, e.g. USB-Blaster. Named when
more than one is attached.
_Avoid_: programmer, adapter, dongle

**Flash**:
Loading a Bitstream onto the chip over a Cable. Volatile — it does not survive a
power cycle.
_Avoid_: program, burn, deploy, upload
