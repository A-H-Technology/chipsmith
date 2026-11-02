//! The 7-series primitives synthesis ends in, with their Verilog
//! declarations ([`LIBRARY`]) for the front end. Named and parametrized as
//! Xilinx's (UG953), so the netlist is what yosys' `synth_xilinx` writes and
//! openXC7's nextpnr reads.

use std::collections::BTreeMap;

use chipsmith_netlist::{CellKind, Const, Dir, Generic, GenericPort, ParamValue, SigBit, State};

pub const CARRY4: &str = "CARRY4";
pub const RAM32M: &str = "RAM32M";
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

/// Cells that are pure functions of their inputs: unused ones go.
pub const REMOVABLE: &[&str] = &[
    "LUT1", "LUT2", "LUT3", "LUT4", "LUT5", "LUT6", "FDRE", "FDSE", "FDCE", "FDPE", CARRY4, RAM32M,
    BUFG,
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
    ParamValue::Bits(Const((0..width).map(|i| State::from(v >> i & 1 == 1)).collect()))
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
        for ty in LUTS.iter().chain(&FFS).chain(&[CARRY4, RAM32M, IBUF, OBUF, OBUFT, IOBUF, BUFG])
        {
            assert!(is_device_cell(ty), "{ty}");
        }
        assert!(!is_device_cell("LUT"));
    }
}
