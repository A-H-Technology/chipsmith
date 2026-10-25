//! The Cyclone II's JTAG and serial-flash hard blocks, one of each per
//! device. The cell library (`synth/cells.v`) has them as `CY2_JTAG` (the TAP's
//! signals during a user instruction, and the TDO it shifts out) and
//! `CY2_ASMI` (DCLK, nCSO and ASDO of the configuration flash driven from
//! the core, DATA0 read back). Together they make a serial flash loader:
//! `persist.rs` loads one to program the EPCS over the JTAG cable.
//!
//! Their bels sit at fixed places in cyclone2-re's chip database (its
//! `hard.rs`): the packer passes them through, the placer pins them there,
//! the router reaches their pins like any other bel's.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Block {
    Jtag,
    Asmi,
}

impl Block {
    pub const ALL: [Block; 2] = [Block::Jtag, Block::Asmi];

    pub fn cell_type(self) -> &'static str {
        match self {
            Block::Jtag => "CY2_JTAG",
            Block::Asmi => "CY2_ASMI",
        }
    }

    pub fn from_cell_type(ty: &str) -> Option<Block> {
        Block::ALL.into_iter().find(|b| b.cell_type() == ty)
    }

    /// The chip database's tile (logic tile X = 2 * Quartus X + 1) and bel.
    pub fn bel(self) -> ((u32, u32), &'static str) {
        match self {
            Block::Jtag => ((13, 22), "JTAG"),
            Block::Asmi => ((13, 20), "ASMI"),
        }
    }

    pub fn pins(self) -> &'static [Pin] {
        use Pin::*;
        match self {
            Block::Jtag => &[
                Tdouser,
                Tmsutap,
                Tckutap,
                Tdiutap,
                Shiftuser,
                Clkdruser,
                Updateuser,
                Runidleuser,
                Usr1user,
            ],
            Block::Asmi => &[Dclkin, Scein, Sdoin, Data0out],
        }
    }
}

/// A pin of one of the blocks (the cell's port and the bel's pin share the
/// name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pin {
    Tdouser,
    Tmsutap,
    Tckutap,
    Tdiutap,
    Shiftuser,
    Clkdruser,
    Updateuser,
    Runidleuser,
    Usr1user,
    Dclkin,
    Scein,
    Sdoin,
    Data0out,
}

impl Pin {
    pub fn name(self) -> &'static str {
        use Pin::*;
        match self {
            Tdouser => "TDOUSER",
            Tmsutap => "TMSUTAP",
            Tckutap => "TCKUTAP",
            Tdiutap => "TDIUTAP",
            Shiftuser => "SHIFTUSER",
            Clkdruser => "CLKDRUSER",
            Updateuser => "UPDATEUSER",
            Runidleuser => "RUNIDLEUSER",
            Usr1user => "USR1USER",
            Dclkin => "DCLKIN",
            Scein => "SCEIN",
            Sdoin => "SDOIN",
            Data0out => "DATA0OUT",
        }
    }

    pub fn parse(name: &str) -> Option<Pin> {
        Block::ALL
            .iter()
            .flat_map(|b| b.pins())
            .copied()
            .find(|p| p.name() == name)
    }

    pub fn is_output(self) -> bool {
        !matches!(self, Pin::Tdouser | Pin::Dclkin | Pin::Scein | Pin::Sdoin)
    }
}

impl fmt::Display for Pin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}
