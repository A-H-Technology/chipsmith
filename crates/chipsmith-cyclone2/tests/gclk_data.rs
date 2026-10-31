//! A PLL clock used as data, through the whole flow on the shipped chip
//! database: synthesis, pack, place, route, bitstream. Quartus routes such
//! a clock on its global into a LAB line or an I/O cell's data through
//! the row clock buffers (cyclone2-re README, "Global clocks as data");
//! these check that chipsmith builds it the same way and that a PLL's
//! counter 2 still takes its dedicated clock output to its own pin. Needs
//! `CHIPSMITH_CYCLONE2_DB`: run in the devshell.

mod support;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chipsmith_cyclone2::bitstream::{self, device::Device, nextpnr};
use chipsmith_cyclone2::pin::UnusedPins;
use chipsmith_cyclone2::place::{self, PinPlan};
use chipsmith_cyclone2::{pack, route, synth};

const BLK_CLK_BUF: i16 = 560;
const LOCAL_INTERCONNECT: i16 = 276;

/// Builds `body` (inside `module top(...)` with `ports`) with `pins`
/// (port, PIN_x), returning the routed design.
fn build(name: &str, ports: &str, body: &str, pins: &[(&str, &str)]) -> (Device, nextpnr::Routed) {
    let dir = support::scratch(Path::new(env!("CARGO_TARGET_TMPDIR")), name);
    let src = dir.join("top.v");
    std::fs::write(&src, format!("module top({ports});\n{body}\nendmodule\n")).unwrap();
    let design = support::frontend("top", &[PathBuf::from(&src)]);
    let out =
        synth::synthesize(design, &BTreeMap::new()).unwrap_or_else(|e| panic!("{name}: {e:#}"));
    let netlist = dir.join("netlist.json");
    support::write(&out.design, &netlist);
    let packed = dir.join("packed.json");
    pack::pack_file(&netlist, &packed).unwrap_or_else(|e| panic!("{name}: pack: {e:#}"));
    let graph = route::load_graph().unwrap();
    let pins: BTreeMap<String, PinPlan> = pins
        .iter()
        .map(|&(port, pin)| {
            let plan = PinPlan {
                pin: pin.to_string(),
                settings: Default::default(),
            };
            (port.to_string(), plan)
        })
        .collect();
    let placed = dir.join("placed.json");
    place::place_file(&graph, &packed, &pins, UnusedPins::default(), &placed)
        .unwrap_or_else(|e| panic!("{name}: place: {e:#}"));
    let routed = dir.join("routed.json");
    route::route_file(&graph, &placed, &routed).unwrap_or_else(|e| panic!("{name}: route: {e:#}"));
    let db = bitstream::load_db().unwrap();
    bitstream::pack_file(&db, &routed, &dir.join("top.rbf"))
        .unwrap_or_else(|e| panic!("{name}: bitstream: {e:#}"));
    let r = nextpnr::read(&db.dev, &routed).unwrap();
    (db.dev, r)
}

/// The net into `port`'s pin, and the type of the cell driving it.
fn pin_net<'r>(r: &'r nextpnr::Routed, port: &str) -> (u64, &'r str) {
    let pin = r
        .cells
        .values()
        .find(|c| c.attrs.get("PORT").map(String::as_str) == Some(port))
        .unwrap_or_else(|| panic!("no pin {port}"));
    let n = pin.conns["DATAOUT"];
    let driver = r
        .cells
        .values()
        .find(|c| {
            c.conns
                .iter()
                .any(|(p, &m)| m == n && matches!(p.as_str(), "OUTCLK" | "COMBOUT" | "REGOUT"))
        })
        .unwrap_or_else(|| panic!("{port}: undriven"));
    (n, driver.ty.as_str())
}

/// Whether net `n` leaves the clock network for a LAB line: a row clock
/// buffer into LOCAL_INTERCONNECT.
fn global_to_line(dev: &Device, r: &nextpnr::Routed, n: u64) -> bool {
    r.nets[&n].pips.iter().any(|p| match (&p.src, &p.dst) {
        (nextpnr::Wire::Node(s), nextpnr::Wire::Node(d)) => {
            dev.element_enum(*s) == BLK_CLK_BUF && dev.element_enum(*d) == LOCAL_INTERCONNECT
        }
        _ => false,
    })
}

const PLL: &str = r#"
  wire c0, c2;
  CY2_PLL #(
    .INCLK0_INPUT_FREQUENCY(20000),
    .CLK0_MULTIPLY_BY(1), .CLK0_DIVIDE_BY(2),
    .CLK2_MULTIPLY_BY(1), .CLK2_DIVIDE_BY(2)
  ) pll (.INCLK(CLOCK_50), .CLK0(c0), .CLK2(c2));
  reg [3:0] n = 0;
  always @(posedge c0) n <= n + 1'b1;
  assign LEDR = n[3];
"#;

/// Terasic's `assign VGA_CLK = pll_clk`, the clock into a LUT, and counter
/// 2 alone on an ordinary pin (AUD_XCK): the packer's guess of its
/// dedicated output is wrong, so it becomes that clock's global.
#[test]
fn a_pll_clock_drives_pins_and_luts_on_its_global() {
    let body =
        format!("{PLL}  assign VGA_CLK = c0;\n  assign LEDG = c0 & SW;\n  assign AUD_XCK = c2;");
    let (dev, r) = build(
        "gclk-data",
        "input CLOCK_50, input SW, output LEDR, output LEDG, output VGA_CLK, output AUD_XCK",
        &body,
        &[
            ("CLOCK_50", "PIN_N2"),
            ("SW", "PIN_N25"),
            ("LEDR", "PIN_AE23"),
            ("LEDG", "PIN_AE22"),
            ("VGA_CLK", "PIN_B8"),
            ("AUD_XCK", "PIN_A5"),
        ],
    );
    let (vga, ty) = pin_net(&r, "VGA_CLK");
    assert_eq!(ty, "CLKCTRL");
    assert!(
        global_to_line(&dev, &r, vga),
        "VGA_CLK off the global through a row clock buffer"
    );
    let (aud, ty) = pin_net(&r, "AUD_XCK");
    assert_eq!(
        ty, "CLKCTRL",
        "counter 2 on an ordinary pin is a global, not PLLOUT"
    );
    assert!(global_to_line(&dev, &r, aud));
    assert!(r.cells.values().all(|c| c.ty != "PLLOUT"));
    // The LUT reads the same global the pin does.
    let lut_reads = r.cells.values().any(|c| {
        c.ty == "LCELL_COMB"
            && c.conns
                .iter()
                .any(|(p, &m)| p.starts_with("DATA") && m == vga)
    });
    assert!(lut_reads, "c0 into a LUT on its global");
}

/// Counter 2 clocking a register and on an ordinary pin: the pin joins
/// the global the register already has.
#[test]
fn counter_2_on_an_ordinary_pin_joins_its_global() {
    let body = format!("{PLL}  reg [3:0] m = 0;\n  always @(posedge c2) m <= m + 1'b1;\n  assign LEDG = m[3];\n  assign AUD_XCK = c2;");
    let (_, r) = build(
        "gclk-data-shared",
        "input CLOCK_50, output LEDR, output LEDG, output AUD_XCK",
        &body,
        &[
            ("CLOCK_50", "PIN_N2"),
            ("LEDR", "PIN_AE23"),
            ("LEDG", "PIN_AE22"),
            ("AUD_XCK", "PIN_A5"),
        ],
    );
    let (aud, ty) = pin_net(&r, "AUD_XCK");
    assert_eq!(ty, "CLKCTRL");
    let clocks = r.cells.values().filter(|c| c.ty == "CLKCTRL").count();
    assert_eq!(clocks, 2, "a global each for c0 and c2, none for the pin");
    let ff_on_it = r
        .cells
        .values()
        .any(|c| c.ty == "LCELL_FF" && c.conns.get("CLK") == Some(&aud));
    assert!(ff_on_it, "one global for c2's register and its pin");
}

/// Counter 2 on its PLL's own clock output pin (PLL_1: PIN_AA7, DRAM_CLK)
/// still goes through the dedicated output.
#[test]
fn counter_2_on_its_own_pin_keeps_the_dedicated_output() {
    let body = format!("{PLL}  assign DRAM_CLK = c2;");
    let (_, r) = build(
        "gclk-data-pllout",
        "input CLOCK_50, output LEDR, output DRAM_CLK",
        &body,
        &[
            ("CLOCK_50", "PIN_N2"),
            ("LEDR", "PIN_AE23"),
            ("DRAM_CLK", "PIN_AA7"),
        ],
    );
    assert_eq!(pin_net(&r, "DRAM_CLK").1, "PLLOUT");
}
