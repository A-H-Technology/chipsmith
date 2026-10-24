//! The Cyclone II cells synthesis ends in, as cyclone2-re's
//! `synth/cells_sim.v` declares them and the packer reads them.

use std::collections::BTreeMap;

use chipsmith_netlist::{CellKind, Const, Dir, Generic, GenericPort, ParamValue, SigBit, State};

pub const LCELL_COMB: &str = "LCELL_COMB";
pub const LCELL_FF: &str = "LCELL_FF";
pub const IBUF: &str = "CY2_IBUF";
pub const OBUF: &str = "CY2_OBUF";
pub const IOBUF: &str = "CY2_IOBUF";
pub const M4K: &str = "CY2_M4K";

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
        let mask = Const((0..16).map(|i| State::from(self.mask >> i & 1 == 1)).collect());
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
    pub ena: SigBit,
    pub aclr: SigBit,
    pub sclr: SigBit,
    pub q: SigBit,
}

impl Reg {
    pub fn cell(self) -> CellKind {
        generic(
            LCELL_FF,
            BTreeMap::new(),
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

pub fn ibuf(pad: SigBit, combout: SigBit) -> CellKind {
    generic(
        IBUF,
        BTreeMap::new(),
        vec![
            port("PAD", Dir::In, vec![pad]),
            port("COMBOUT", Dir::Out, vec![combout]),
        ],
    )
}

pub fn obuf(dataout: SigBit, pad: SigBit) -> CellKind {
    generic(
        OBUF,
        BTreeMap::new(),
        vec![
            port("DATAOUT", Dir::In, vec![dataout]),
            port("PAD", Dir::Out, vec![pad]),
        ],
    )
}

/// A tri-state output (`combout` `None`) or a bidir.
pub fn iobuf(dataout: SigBit, oe: SigBit, combout: Option<SigBit>, pad: SigBit) -> CellKind {
    let mut ports = vec![
        port("DATAOUT", Dir::In, vec![dataout]),
        port("OE", Dir::In, vec![oe]),
    ];
    if let Some(c) = combout {
        ports.push(port("COMBOUT", Dir::Out, vec![c]));
    }
    ports.push(port("PAD", Dir::InOut, vec![pad]));
    generic(IOBUF, BTreeMap::new(), ports)
}
