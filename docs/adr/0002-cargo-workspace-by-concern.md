# A Cargo workspace split by concern, devices in their own crates

chipsmith is a workspace rather than one crate, so that a device's back end, the synthesis front end and the cable drivers can't reach into each other except through a public API, and so the reverse-engineering repos can depend on just the part they need (cyclone2-re's `cram` on the Cyclone II bitstream code, see ADR-0003).

```
crates/
  chipsmith-netlist   the netlist IR, shared by front end and synthesis (ADR-0004)
  chipsmith-verilog   Verilog parser and elaborator -> netlist
  chipsmith-synth     device-independent synthesis: RTL passes, LUT mapping
  chipsmith-route     shared by the device back ends: routing graph,
                      PathFinder router, timing graph
  chipsmith-cyclone2  Cyclone II: synthesis mapping, packing, placement,
                      routing, timing, bitstream, JTAG stream, EPCS image
  chipsmith-xc7       7-series: placement, routing, delays, Zynq .bin
  chipsmith-jtag      cables (USB-Blaster, FT2232), SVF player, 7-series loader
  chipsmith           the binary: manifest, flow (Plan/Step), CLI
```

Dependencies point down: `chipsmith` uses everything; device crates use `chipsmith-netlist`, `chipsmith-synth`, `chipsmith-route` and `chipsmith-jtag`, never each other; nothing depends on `chipsmith`. Device-independent algorithms move into a shared crate only once a second device actually shares them; until then they stay in the device crate that has them.

`chipsmith-route` is the first such crate. The 7-series router reuses the Cyclone II's PathFinder, its `ChipDb` graph (prjxray-db is imported into the same structure) and its timing graph, so those three were shared before the split, not Cyclone II code. Each device keeps its own side: reading its placed design into the router's `Design` and writing the routes back.

## Considered options

- One crate with modules: what chipsmith was. Nothing stopped `flow.rs` from calling into any device's internals, and cyclone2-re could only take the whole toolchain.
- A crate per stage (pack, place, route): the stages share each device's chip database and cell model far more than they share anything across devices, so the cut that keeps coupling inside a crate is the device.
