//! The 7-series primitives synthesis ends in, with their Verilog
//! declarations ([`LIBRARY`]) for the front end. Named and parametrized as
//! Xilinx's (UG953), so the netlist is what yosys' `synth_xilinx` writes and
//! openXC7's nextpnr reads.

use std::collections::BTreeMap;

use anyhow::{ensure, Result};
use chipsmith_netlist::{CellKind, Const, Dir, Generic, GenericPort, ParamValue, SigBit, State};

use crate::bram::{self, Size};

pub const CARRY4: &str = "CARRY4";
pub const RAM32M: &str = "RAM32M";
pub const RAMB18E1: &str = "RAMB18E1";
pub const RAMB36E1: &str = "RAMB36E1";
pub const DSP48E1: &str = "DSP48E1";
pub const IBUF: &str = "IBUF";
pub const OBUF: &str = "OBUF";
pub const OBUFT: &str = "OBUFT";
pub const IOBUF: &str = "IOBUF";
pub const BUFG: &str = "BUFG";
pub const LUTS: [&str; 6] = ["LUT1", "LUT2", "LUT3", "LUT4", "LUT5", "LUT6"];
pub const FFS: [&str; 4] = ["FDRE", "FDSE", "FDCE", "FDPE"];

/// Every primitive as a Verilog blackbox module, for the front end to read
/// as a library before a design that instantiates one.
pub const LIBRARY: &str = include_str!("cells.v");
/// The name [`LIBRARY`] is read under, for error positions.
pub const LIBRARY_NAME: &str = "chipsmith-xc7/cells.v";

/// Behavioural models of the hard blocks (block RAM), for co-simulating a
/// netlist with iverilog; checked against Vivado's unisim models
/// (`tests/models.rs`).
pub const MODELS: &str = include_str!("../sim.v");
/// The cells [`MODELS`] models.
pub const MODELLED: [&str; 6] = [
    RAMB18E1,
    RAMB36E1,
    "XC7_PLL",
    "XC7_MMCM",
    "PLLE2_ADV",
    "MMCME2_ADV",
];

/// yosys' `xilinx/cells_sim.v` without its models of [`MODELLED`]
/// (timing only, no behaviour), so they stay blackboxes for [`MODELS`].
pub fn yosys_sim_without_models(sim: &str) -> String {
    let mut out = sim.to_string();
    for ty in MODELLED {
        let start = format!("module {ty} (");
        while let Some(i) = out.find(&start) {
            let end = out[i..]
                .find("endmodule")
                .map_or(out.len(), |j| i + j + "endmodule".len());
            out.replace_range(i..end, "");
        }
    }
    out
}

/// Cells that are pure functions of their inputs: unused ones go.
pub const REMOVABLE: &[&str] = &[
    "LUT1", "LUT2", "LUT3", "LUT4", "LUT5", "LUT6", "FDRE", "FDSE", "FDCE", "FDPE", CARRY4, RAM32M,
    RAMB18E1, RAMB36E1, DSP48E1, BUFG,
];

/// Whether `ty` is one of [`LIBRARY`]'s cells.
pub fn is_device_cell(ty: &str) -> bool {
    LIBRARY.contains(&format!("module {ty} "))
}

fn port(name: &str, dir: Dir, sig: Vec<SigBit>) -> GenericPort {
    GenericPort {
        name: name.into(),
        dir: Some(dir),
        sig,
    }
}

fn bits(v: u64, width: usize) -> ParamValue {
    ParamValue::Bits(Const(
        (0..width).map(|i| State::from(v >> i & 1 == 1)).collect(),
    ))
}

fn generic(ty: &str, params: BTreeMap<String, ParamValue>, ports: Vec<GenericPort>) -> CellKind {
    CellKind::Other(Generic {
        ty: ty.into(),
        params,
        ports,
    })
}

/// `LUT<n>`: `O = INIT[{I<n-1>, ..., I0}]`, `n` = `inputs.len()`, 1 to 6.
pub fn lut(inputs: &[SigBit], init: u64, o: SigBit) -> CellKind {
    let n = inputs.len();
    assert!((1..=6).contains(&n), "a LUT has 1 to 6 inputs, not {n}");
    let mut ports: Vec<GenericPort> = inputs
        .iter()
        .enumerate()
        .map(|(i, &b)| port(&format!("I{i}"), Dir::In, vec![b]))
        .collect();
    ports.push(port("O", Dir::Out, vec![o]));
    generic(
        LUTS[n - 1],
        BTreeMap::from([("INIT".to_string(), bits(init, 1 << n))]),
        ports,
    )
}

/// What a register's SR input does, which picks the primitive: R, S, CLR
/// or PRE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reset {
    /// FDRE with R tied 0.
    None,
    /// FDRE: Q goes 0 on a clock edge with R high, enabled or not.
    SyncClear(SigBit),
    /// FDSE: ... 1.
    SyncSet(SigBit),
    /// FDCE: Q is 0 while CLR is high.
    AsyncClear(SigBit),
    /// FDPE: ... 1 while PRE is high.
    AsyncPreset(SigBit),
}

/// One slice register.
pub struct Fd {
    pub d: SigBit,
    pub clk: SigBit,
    /// Clocked on the falling edge (the slice inverts its clock).
    pub clk_inv: bool,
    pub ce: SigBit,
    pub reset: Reset,
    /// The power-up value.
    pub init: bool,
    pub q: SigBit,
}

impl Fd {
    pub fn cell(self) -> CellKind {
        let (ty, sr) = match self.reset {
            Reset::None => ("FDRE", ("R", SigBit::ZERO)),
            Reset::SyncClear(r) => ("FDRE", ("R", r)),
            Reset::SyncSet(s) => ("FDSE", ("S", s)),
            Reset::AsyncClear(c) => ("FDCE", ("CLR", c)),
            Reset::AsyncPreset(p) => ("FDPE", ("PRE", p)),
        };
        generic(
            ty,
            BTreeMap::from([
                ("INIT".to_string(), bits(self.init.into(), 1)),
                ("IS_C_INVERTED".to_string(), bits(self.clk_inv.into(), 1)),
            ]),
            vec![
                port("C", Dir::In, vec![self.clk]),
                port("CE", Dir::In, vec![self.ce]),
                port(sr.0, Dir::In, vec![sr.1]),
                port("D", Dir::In, vec![self.d]),
                port("Q", Dir::Out, vec![self.q]),
            ],
        )
    }
}

/// One CARRY4: per bit `CO = S ? carry : DI`, `O = S ^ carry`, the carry
/// into bit 0 being `CI`, or `CYINIT` at the head of a chain (`CI` tied 0).
pub struct Carry4 {
    pub ci: SigBit,
    pub cyinit: SigBit,
    pub di: [SigBit; 4],
    pub s: [SigBit; 4],
    pub o: [SigBit; 4],
    pub co: [SigBit; 4],
}

impl Carry4 {
    pub fn cell(self) -> CellKind {
        generic(
            CARRY4,
            BTreeMap::new(),
            vec![
                port("CI", Dir::In, vec![self.ci]),
                port("CYINIT", Dir::In, vec![self.cyinit]),
                port("DI", Dir::In, self.di.to_vec()),
                port("S", Dir::In, self.s.to_vec()),
                port("O", Dir::Out, self.o.to_vec()),
                port("CO", Dir::Out, self.co.to_vec()),
            ],
        )
    }
}

/// One RAM32M: four 32 x 2 memories (A to D) written together at `waddr`,
/// each read at its own address (D at `waddr`).
pub struct Ram32m {
    pub raddr: [Vec<SigBit>; 3],
    pub waddr: Vec<SigBit>,
    pub di: [[SigBit; 2]; 4],
    pub dout: [[SigBit; 2]; 4],
    pub wclk: SigBit,
    pub wclk_inv: bool,
    pub we: SigBit,
    /// Per memory, bit `2 * word + i` holds bit `i` of `word`.
    pub init: [u64; 4],
}

impl Ram32m {
    pub fn cell(self) -> CellKind {
        let mut params = BTreeMap::from([(
            "IS_WCLK_INVERTED".to_string(),
            bits(self.wclk_inv.into(), 1),
        )]);
        let mut ports = Vec::new();
        for (i, l) in ['A', 'B', 'C', 'D'].into_iter().enumerate() {
            params.insert(format!("INIT_{l}"), bits(self.init[i], 64));
            let addr = if i < 3 { &self.raddr[i] } else { &self.waddr };
            ports.push(port(&format!("ADDR{l}"), Dir::In, addr.clone()));
            ports.push(port(&format!("DI{l}"), Dir::In, self.di[i].to_vec()));
            ports.push(port(&format!("DO{l}"), Dir::Out, self.dout[i].to_vec()));
        }
        ports.push(port("WCLK", Dir::In, vec![self.wclk]));
        ports.push(port("WE", Dir::In, vec![self.we]));
        generic(RAM32M, params, ports)
    }
}

/// A block RAM at its pins (UG953's names), per port A and B: clock,
/// enable (of the port's reads and writes), byte write enables (WEA,
/// WEBWE), address, write data, read data, output register enable and the
/// latch's and register's resets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ramb {
    pub config: bram::Config,
    pub clk: [SigBit; 2],
    pub en: [SigBit; 2],
    pub regce: [SigBit; 2],
    pub rstram: [SigBit; 2],
    pub rstreg: [SigBit; 2],
    /// [`Size::addr_pins`] bits each.
    pub addr: [Vec<SigBit>; 2],
    /// WEA: 2 (4) bits, WEBWE: 4 (8).
    pub we: [Vec<SigBit>; 2],
    /// [`Size::data_pins`] bits each, and an eighth of that in parity.
    pub di: [Vec<SigBit>; 2],
    pub dip: [Vec<SigBit>; 2],
    pub dout: [Vec<SigBit>; 2],
    pub doutp: [Vec<SigBit>; 2],
}

/// Port names per side: (A, B).
const RAMB_PORTS: [(&str, &str); 11] = [
    ("CLKARDCLK", "CLKBWRCLK"),
    ("ENARDEN", "ENBWREN"),
    ("REGCEAREGCE", "REGCEB"),
    ("RSTRAMARSTRAM", "RSTRAMB"),
    ("RSTREGARSTREG", "RSTREGB"),
    ("ADDRARDADDR", "ADDRBWRADDR"),
    ("WEA", "WEBWE"),
    ("DIADI", "DIBDI"),
    ("DIPADIP", "DIPBDIP"),
    ("DOADO", "DOBDO"),
    ("DOPADOP", "DOPBDOP"),
];

impl Ramb {
    /// Every pin tied off (enables 0, write enables 0, addresses 0), read
    /// data on fresh nets the caller fills in.
    pub fn new(config: bram::Config, dout: [Vec<SigBit>; 2], doutp: [Vec<SigBit>; 2]) -> Ramb {
        let s = config.size;
        let zeros = |n: usize| vec![SigBit::ZERO; n];
        Ramb {
            config,
            clk: [SigBit::ZERO; 2],
            en: [SigBit::ZERO; 2],
            regce: [SigBit::ZERO; 2],
            rstram: [SigBit::ZERO; 2],
            rstreg: [SigBit::ZERO; 2],
            addr: [zeros(s.addr_pins()), zeros(s.addr_pins())],
            we: [zeros(s.data_pins() / 8), zeros(s.data_pins() / 4)],
            di: [zeros(s.data_pins()), zeros(s.data_pins())],
            dip: [zeros(s.data_pins() / 8), zeros(s.data_pins() / 8)],
            dout,
            doutp,
        }
    }

    pub fn cell(self) -> CellKind {
        let s = self.config.size;
        let mut ports = Vec::new();
        for (side, (a, b)) in RAMB_PORTS.into_iter().enumerate() {
            for (k, name) in [a, b].into_iter().enumerate() {
                let (dir, sig) = match side {
                    0 => (Dir::In, vec![self.clk[k]]),
                    1 => (Dir::In, vec![self.en[k]]),
                    2 => (Dir::In, vec![self.regce[k]]),
                    3 => (Dir::In, vec![self.rstram[k]]),
                    4 => (Dir::In, vec![self.rstreg[k]]),
                    5 => (Dir::In, self.addr[k].clone()),
                    6 => (Dir::In, self.we[k].clone()),
                    7 => (Dir::In, self.di[k].clone()),
                    8 => (Dir::In, self.dip[k].clone()),
                    9 => (Dir::Out, self.dout[k].clone()),
                    _ => (Dir::Out, self.doutp[k].clone()),
                };
                ports.push(port(name, dir, sig));
            }
        }
        if s == Size::Full {
            for p in ["CASCADEINA", "CASCADEINB", "INJECTDBITERR", "INJECTSBITERR"] {
                ports.push(port(p, Dir::In, vec![SigBit::ZERO]));
            }
        }
        generic(s.primitive(), self.config.params(), ports)
    }

    /// A RAMB18E1 or RAMB36E1 cell's pins and configuration. Pins left
    /// out read as UG953's defaults: 0, but REGCE 1 (`unconnected`).
    pub fn from_generic(g: &Generic) -> Result<Ramb> {
        let config = bram::Config::from_params(&g.ty, &g.params)?;
        let s = config.size;
        let sig = |name: &str, width: usize, default: SigBit| -> Result<Vec<SigBit>> {
            let mut v = g.port(name).map(|p| p.sig.clone()).unwrap_or_default();
            ensure!(v.len() <= width, "{}: {name} has {} bits", g.ty, v.len());
            v.resize(width, default);
            Ok(v)
        };
        let one = |name: &str, default: SigBit| -> Result<SigBit> { Ok(sig(name, 1, default)?[0]) };
        let pair = |i: usize, default: SigBit| -> Result<[SigBit; 2]> {
            let (a, b) = RAMB_PORTS[i];
            Ok([one(a, default)?, one(b, default)?])
        };
        let vec2 = |i: usize, wa: usize, wb: usize| -> Result<[Vec<SigBit>; 2]> {
            let (a, b) = RAMB_PORTS[i];
            Ok([sig(a, wa, SigBit::ZERO)?, sig(b, wb, SigBit::ZERO)?])
        };
        let (d, p) = (s.data_pins(), s.data_pins() / 8);
        let out = |name: &str, width: usize| -> Result<Vec<SigBit>> {
            let v = g.port(name).map(|p| p.sig.clone()).unwrap_or_default();
            ensure!(v.len() <= width, "{}: {name} has {} bits", g.ty, v.len());
            Ok(v)
        };
        let ramb = Ramb {
            clk: pair(0, SigBit::ZERO)?,
            en: pair(1, SigBit::ZERO)?,
            regce: pair(2, SigBit::ONE)?,
            rstram: pair(3, SigBit::ZERO)?,
            rstreg: pair(4, SigBit::ZERO)?,
            addr: vec2(5, s.addr_pins(), s.addr_pins())?,
            we: vec2(6, d / 8, d / 4)?,
            di: vec2(7, d, d)?,
            dip: vec2(8, p, p)?,
            dout: [out("DOADO", d)?, out("DOBDO", d)?],
            doutp: [out("DOPADOP", p)?, out("DOPBDOP", p)?],
            config,
        };
        Ok(ramb)
    }
}

pub fn ibuf(pad: SigBit, o: SigBit) -> CellKind {
    generic(
        IBUF,
        BTreeMap::new(),
        vec![port("I", Dir::In, vec![pad]), port("O", Dir::Out, vec![o])],
    )
}

pub fn obuf(i: SigBit, pad: SigBit) -> CellKind {
    generic(
        OBUF,
        BTreeMap::new(),
        vec![port("I", Dir::In, vec![i]), port("O", Dir::Out, vec![pad])],
    )
}

/// `t` high releases the pin.
pub fn obuft(i: SigBit, t: SigBit, pad: SigBit) -> CellKind {
    generic(
        OBUFT,
        BTreeMap::new(),
        vec![
            port("I", Dir::In, vec![i]),
            port("T", Dir::In, vec![t]),
            port("O", Dir::Out, vec![pad]),
        ],
    )
}

/// `t` high releases the pin; `o` reads it.
pub fn iobuf(i: SigBit, t: SigBit, o: SigBit, pad: SigBit) -> CellKind {
    generic(
        IOBUF,
        BTreeMap::new(),
        vec![
            port("I", Dir::In, vec![i]),
            port("T", Dir::In, vec![t]),
            port("O", Dir::Out, vec![o]),
            port("IO", Dir::InOut, vec![pad]),
        ],
    )
}

pub fn bufg(i: SigBit, o: SigBit) -> CellKind {
    generic(
        BUFG,
        BTreeMap::new(),
        vec![port("I", Dir::In, vec![i]), port("O", Dir::Out, vec![o])],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_declares_every_cell_synthesis_makes() {
        for ty in LUTS.iter().chain(&FFS).chain(&[
            CARRY4, RAM32M, RAMB18E1, RAMB36E1, IBUF, OBUF, OBUFT, IOBUF, BUFG,
        ]) {
            assert!(is_device_cell(ty), "{ty}");
        }
        assert!(!is_device_cell("LUT"));
    }
}
