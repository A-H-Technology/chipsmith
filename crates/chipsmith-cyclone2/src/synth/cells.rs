//! The Cyclone II cells synthesis ends in and the packer reads, with their
//! Verilog declarations ([`LIBRARY`]) for the front end.

use std::collections::BTreeMap;

use chipsmith_netlist::{CellKind, Const, Ctrl, Dir, Generic, GenericPort, ParamValue, Polarity, SigBit, State};

use crate::pin::{control_param, register_param, IoRegMode, PowerUp};

pub const LCELL_COMB: &str = "LCELL_COMB";
pub const LCELL_FF: &str = "LCELL_FF";
pub const IBUF: &str = "CY2_IBUF";
pub const OBUF: &str = "CY2_OBUF";
pub const IOBUF: &str = "CY2_IOBUF";
pub const M4K: &str = "CY2_M4K";
pub const MULT: &str = "CY2_MULT";

/// Every device cell as a Verilog blackbox module, for the front end to
/// read as a library (`Frontend::read_library_source`) before a design that
/// instantiates one.
pub const LIBRARY: &str = include_str!("cells.v");
/// The name [`LIBRARY`] is read under, for error positions.
pub const LIBRARY_NAME: &str = "chipsmith-cyclone2/cells.v";

/// Device cells that are pure functions of their inputs: unused ones go.
pub const REMOVABLE: &[&str] = &[LCELL_COMB, LCELL_FF];

fn port(name: &str, dir: Dir, sig: Vec<SigBit>) -> GenericPort {
    GenericPort {
        name: name.into(),
        dir: Some(dir),
        sig,
    }
}

fn generic(ty: &str, params: BTreeMap<String, ParamValue>, ports: Vec<GenericPort>) -> CellKind {
    CellKind::Other(Generic {
        ty: ty.into(),
        params,
        ports,
    })
}

/// One LE's LUT: `COMBOUT = LUT_MASK[{D, C, B, A}]` with C the carry in on
/// an arithmetic LE (`DATAC` tied 1) and `DATAC` otherwise (`CIN` tied 1),
/// `COUT` the same table with D = 0 and C = `CIN`. An output left `None` is
/// not connected.
pub struct Comb {
    pub mask: u16,
    /// DATAA, DATAB, DATAC, DATAD.
    pub data: [SigBit; 4],
    pub cin: SigBit,
    pub combout: Option<SigBit>,
    pub cout: Option<SigBit>,
}

impl Comb {
    pub fn cell(self) -> CellKind {
        let mut ports = Vec::new();
        for (name, b) in ["DATAA", "DATAB", "DATAC", "DATAD"].iter().zip(self.data) {
            ports.push(port(name, Dir::In, vec![b]));
        }
        ports.push(port("CIN", Dir::In, vec![self.cin]));
        if let Some(y) = self.combout {
            ports.push(port("COMBOUT", Dir::Out, vec![y]));
        }
        if let Some(c) = self.cout {
            ports.push(port("COUT", Dir::Out, vec![c]));
        }
        let mask = Const(
            (0..16)
                .map(|i| State::from(self.mask >> i & 1 == 1))
                .collect(),
        );
        generic(
            LCELL_COMB,
            BTreeMap::from([("LUT_MASK".to_string(), ParamValue::Bits(mask))]),
            ports,
        )
    }
}

/// An LE's register: rising edge, clock enable, asynchronous clear and a
/// synchronous clear that only acts on enabled edges. Powers up 0.
pub struct Reg {
    pub d: SigBit,
    pub clk: SigBit,
    /// Clocked on the falling edge: the LAB clock's inversion bit, not an
    /// inverter (which would make a clock from logic).
    pub clk_inv: bool,
    pub ena: SigBit,
    pub aclr: SigBit,
    pub sclr: SigBit,
    pub q: SigBit,
}

impl Reg {
    pub fn cell(self) -> CellKind {
        let mut params = BTreeMap::new();
        if self.clk_inv {
            params.insert("CLK_INV".to_string(), ParamValue::int(1));
        }
        generic(
            LCELL_FF,
            params,
            vec![
                port("DATAIN", Dir::In, vec![self.d]),
                port("CLK", Dir::In, vec![self.clk]),
                port("ENA", Dir::In, vec![self.ena]),
                port("ACLR", Dir::In, vec![self.aclr]),
                port("SCLR", Dir::In, vec![self.sclr]),
                port("REGOUT", Dir::Out, vec![self.q]),
            ],
        )
    }
}

/// The registers of a pin buffer's I/O cell. The output and OE registers
/// share one clock (`IO_OUTREG_CLK_FAST_MUX`) and one clock-enable line, so
/// those are given once; all three share the asynchronous reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IoRegs {
    /// Input register: it samples the pad into its output.
    pub input: Option<InReg>,
    /// The output-side clock, and which of the two registers it clocks.
    pub output: Option<OutRegs>,
    /// The cell's asynchronous reset, for the registers whose mode says
    /// they take it.
    pub areset: Option<Ctrl>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InReg {
    pub clk: Ctrl,
    pub q: SigBit,
    pub enable: Option<Ctrl>,
    pub power_up: PowerUp,
    pub areset: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutRegs {
    pub clk: Ctrl,
    /// The clock-enable line, for the registers whose mode says they
    /// take it.
    pub enable: Option<Ctrl>,
    /// The output register: DATAOUT is what it samples.
    pub data: Option<IoRegMode>,
    /// The OE register: OE is what it samples.
    pub oe: Option<IoRegMode>,
}

impl IoRegs {
    fn add(self, ports: &mut Vec<GenericPort>, params: &mut BTreeMap<String, ParamValue>) {
        let one = || ParamValue::int(1);
        let str = |v: &str| ParamValue::Str(v.into());
        // Ports in the order they had before the controls (INCLK, REGOUT,
        // OUTCLK), the controls after.
        let mut control = |ports: &mut Vec<GenericPort>, pin: &str, invert: &str, c: Ctrl| {
            ports.push(port(pin, Dir::In, vec![c.sig]));
            if c.polarity == Polarity::Neg {
                params.insert(invert.to_string(), one());
            }
        };
        if let Some(r) = self.input {
            control(ports, "INCLK", control_param::INCLK_INVERT, r.clk);
            ports.push(port("REGOUT", Dir::Out, vec![r.q]));
        }
        if let Some(o) = self.output {
            control(ports, "OUTCLK", control_param::OUTCLK_INVERT, o.clk);
        }
        if let Some(e) = self.input.and_then(|r| r.enable) {
            control(ports, "INCLKENA", control_param::INCLKENA_INVERT, e);
        }
        if let Some(e) = self.output.and_then(|o| o.enable) {
            control(ports, "OUTCLKENA", control_param::OUTCLKENA_INVERT, e);
        }
        if let Some(a) = self.areset {
            control(ports, "ARESET", control_param::ARESET_INVERT, a);
        }
        if let Some(r) = self.input {
            let mode = IoRegMode {
                power_up: r.power_up,
                enable: r.enable.is_some(),
                areset: r.areset,
            };
            params.extend(mode.params(register_param::INPUT, str, one));
        }
        if let Some(o) = self.output {
            for (name, mode) in [(register_param::OUTPUT, o.data), (register_param::OE, o.oe)] {
                if let Some(m) = mode {
                    params.extend(m.params(name, str, one));
                }
            }
        }
    }
}

/// An input pin: COMBOUT reads the pad, REGOUT (with an input register)
/// what the register sampled.
pub fn ibuf(pad: SigBit, combout: SigBit, regs: IoRegs) -> CellKind {
    let mut ports = vec![
        port("PAD", Dir::In, vec![pad]),
        port("COMBOUT", Dir::Out, vec![combout]),
    ];
    let mut params = BTreeMap::new();
    regs.add(&mut ports, &mut params);
    generic(IBUF, params, ports)
}

pub fn obuf(dataout: SigBit, pad: SigBit, regs: IoRegs) -> CellKind {
    let mut ports = vec![
        port("DATAOUT", Dir::In, vec![dataout]),
        port("PAD", Dir::Out, vec![pad]),
    ];
    let mut params = BTreeMap::new();
    regs.add(&mut ports, &mut params);
    generic(OBUF, params, ports)
}

/// A tri-state output (`combout` `None`) or a bidir.
pub fn iobuf(
    dataout: SigBit,
    oe: SigBit,
    combout: Option<SigBit>,
    pad: SigBit,
    regs: IoRegs,
) -> CellKind {
    let mut ports = vec![
        port("DATAOUT", Dir::In, vec![dataout]),
        port("OE", Dir::In, vec![oe]),
    ];
    if let Some(c) = combout {
        ports.push(port("COMBOUT", Dir::Out, vec![c]));
    }
    ports.push(port("PAD", Dir::InOut, vec![pad]));
    let mut params = BTreeMap::new();
    regs.add(&mut ports, &mut params);
    generic(IOBUF, params, ports)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hard::Block;

    /// The library declares the cells this module builds and the hard
    /// blocks, no more, under the names the packer reads.
    #[test]
    fn the_library_declares_every_device_cell() {
        let mut expected = vec![LCELL_COMB, LCELL_FF, IBUF, OBUF, IOBUF, M4K, MULT];
        expected.extend(Block::ALL.map(Block::cell_type));
        let instances: String = expected
            .iter()
            .enumerate()
            .map(|(i, ty)| format!("  {ty} c{i} ();\n"))
            .collect();
        let mut fe = chipsmith_verilog::Frontend::new();
        fe.read_library_source(LIBRARY_NAME, LIBRARY).unwrap();
        fe.read_source("top.v", &format!("module top;\n{instances}endmodule\n"))
            .unwrap();
        let design = fe.elaborate("top").unwrap();
        let mut declared: Vec<&str> = design
            .modules
            .iter()
            .filter(|m| m.is_blackbox())
            .map(|m| m.name.as_str())
            .collect();
        declared.sort_unstable();
        expected.sort_unstable();
        assert_eq!(declared, expected);
        assert_eq!(LIBRARY.matches("\nmodule ").count(), expected.len());
    }
}
