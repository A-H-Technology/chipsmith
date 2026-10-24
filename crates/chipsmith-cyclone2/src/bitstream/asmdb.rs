//! `ASMDB_DIE` (ddb_<die>_asmdb.ddb, libdb_asmdb.so): the block-level
//! configuration tables. Each *arch group* is one named feature of one block
//! type (`LE_LUT_INPUT` of a LAB, `IO_PIN_MODE` of an IOE, ...) and lists,
//! per instance inside the block (e.g. per LE), the *muxes* that implement
//! it. A mux is a list of bit positions relative to the block's base flat
//! CRAM address; `ASMDB_BLOCK_INFO` lists every block instance's base.
//! cyclone2-re's `ddb` crate decodes Quartus' file into these types; the
//! shipped database (`db.rs`) stores them as they are.

use std::collections::BTreeMap;

/// Arch group types and block types are `ASMDB_ARCH_GROUP_TYPE` /
/// `ASMDB_ARCH_GROUP_BLOCK_TYPE` enum values. Their names come from the
/// translator table in libdb_asmdb.so; the handful the bitstream needs are
/// listed in `arch`.
pub mod arch {
    pub const LE_LUT_FUNCTION: i32 = 3;
    pub const LE_CLEAR_SELECT: i32 = 6;
    pub const LE_CLOCK_SELECT: i32 = 7;
    pub const LE_USE_SYNC_CLEAR: i32 = 9;
    pub const LE_USE_SYNC_LOAD: i32 = 10;
    pub const LE_USE_CARRY: i32 = 11;
    pub const LE_USE_REG_CASCADE_ARM: i32 = 15;
    pub const LE_USE_REGISTER_FEEDBACK: i32 = 18;
    pub const LE_REG_OUT_YGR: i32 = 19;
    pub const LE_LUT_INPUT: i32 = 21;
    pub const LAB_ENABLE_CE: i32 = 53;
    pub const LAB_ENABLE_SYNC_SIGNALS: i32 = 59;
    pub const LAB_PIA_TO_INPUT: i32 = 60;
    pub const LAB_CE_FAST_MUX: i32 = 61;
    pub const LAB_CLEAR_FAST_MUX: i32 = 62;
    pub const LAB_CLOCK_FAST_MUX: i32 = 63;
    pub const LAB_SYNC_CLR_FAST_MUX: i32 = 67;
    pub const LAB_INVERT: i32 = 69;
    pub const LAB_CLR_INVERT_PAIR: i32 = 70;
    pub const LAB_SYNC_LOAD_FAST_MUX_MAG: i32 = 74;
    pub const CLKBUF_CLK_INPUT_SELECT_CYCLONEII: i32 = 2318;
    pub const CLKBUF_ENABLE_AND_INVERT: i32 = 2320;
    pub const CLKBUF_ENABLE_MODE: i32 = 2339;
    pub const CLKBUF_INTERFACE_COREIN: i32 = 2333;
    /// Block type of a LAB (`m_group_block_type`).
    pub const BLOCK_LAB: i32 = 18;
    /// Block type of an M4K RAM block, and its groups.
    pub const BLOCK_M4K: i32 = 39;
    pub const RAM_MS_BYTE_ENABLE_INPUT: i32 = 570;
    pub const RAM_MS_READ_ADDRESS_INPUT: i32 = 572;
    pub const RAM_MS_WRITE_ADDRESS_INPUT: i32 = 573;
    pub const RAM_MS_ADDRESS_STALL_INPUT: i32 = 574;
    pub const RAM_MS_ADDRESS_STALL_INPUT_B: i32 = 575;
    pub const RAM_MS_DATA_INPUT: i32 = 576;
    pub const RAM_CE_FAST_MUX: i32 = 578;
    pub const RAM_CLEAR_FAST_MUX: i32 = 581;
    pub const RAM_CLOCK_FAST_MUX: i32 = 582;
    pub const RAM_RE_FAST_MUX: i32 = 583;
    pub const RAM_WE_FAST_MUX: i32 = 584;
    pub const RAM_CE_INVERT: i32 = 586;
    pub const RAM_CLEAR_INVERT: i32 = 587;
    pub const RAM_CLOCK_INVERT: i32 = 588;
    pub const RAM_READ_ENABLE_INVERT: i32 = 589;
    pub const RAM_WRITE_ENABLE_INVERT: i32 = 590;
    pub const RAM_DATA_WRITE_MODE: i32 = 591;
    pub const RAM_DATA_WRITE_MODE_B: i32 = 592;
    pub const RAM_REG_DATA_OUT: i32 = 593;
    pub const RAM_REG_DATA_OUT_B: i32 = 594;
    pub const RAM_SELECT_CLK_DATA_OUT: i32 = 599;
    pub const RAM_SELECT_CLK_DATA_OUT_B: i32 = 600;
    pub const RAM_SELECT_CLK_READ_ENABLE_READ_ADDRESS_B: i32 = 603;
    pub const RAM_SELECT_CLK_BYTE_ENABLE_DATA_B: i32 = 604;
    pub const RAM_ENABLE_CE_INPUT: i32 = 606;
    pub const RAM_ENABLE_CE_INPUT_B: i32 = 607;
    pub const RAM_ENABLE_CE_OUTPUT: i32 = 608;
    pub const RAM_ENABLE_CE_OUTPUT_B: i32 = 609;
    pub const RAM_SELECT_CLR_BYTE_ENABLE_B: i32 = 616;
    pub const RAM_SELECT_CLR_DATA_B: i32 = 617;
    pub const RAM_SELECT_CLR_DATA_OUT: i32 = 618;
    pub const RAM_SELECT_CLR_DATA_OUT_B: i32 = 619;
    pub const RAM_SELECT_CLR_READ_ADDRESS_B: i32 = 620;
    pub const RAM_SELECT_CLR_READ_ENABLE_B: i32 = 621;
    pub const RAM_USAGE_MODE: i32 = 624;
    pub const RAM_USE_CLR_DATA_OUT: i32 = 634;
    pub const RAM_USE_CLR_DATA_OUT_B: i32 = 635;
    pub const RAM_USE_EXTENDED_WRITE_PULSE_MAGELLAN: i32 = 642;
    pub const RAM_ENABLE: i32 = 643;
    pub const RAM_CONTENT_YGR_MEAB: i32 = 645;
    pub const RAM_USE_TRUE_DUAL_PORT_MODE: i32 = 648;
    pub const RAM_USE_PACKED_MODE: i32 = 649;
    pub const RAM_PIA_TO_INPUT_YGR: i32 = 650;
    pub const RAM_FLOW_THROUGH_MODE: i32 = 651;
    pub const RAM_FLOW_THROUGH_MODE_B: i32 = 652;
}

/// Index into `Asmdb::muxes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct MuxId(pub u32);

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum MuxKind {
    Plain,
    Labeled { name: Option<String> },
    MappedSimple { map: Option<MuxId> },
    MappedEncoded { map: Option<MuxId> },
    StlMappedEncoded { index_map: Option<u32> },
    Decision { lower: bool, upper: bool },
    EncodedBank { mux_size: i32, bank_size: i32 },
    Ram { polarity: Option<MuxId> },
    Simple,
    Encoded,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Mux {
    /// Bit positions relative to the block base flat address (block base
    /// + bit = flat CRAM address), or for map/label muxes plain integers.
    pub bits: Vec<i32>,
    pub kind: MuxKind,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MuxData {
    pub arch_value: bool,
    pub aux: Option<MuxId>,
    pub next_selection: Option<MuxId>,
}

/// One `ARCH_GROUP_STLMAP` setting: arch value, aux index map, next
/// selection mux, mask index map.
pub type StlmapData = (bool, Option<u32>, Option<MuxId>, Option<u32>);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ArchBody {
    /// `ARCH_GROUP_STD` (`block_info` none) and `ARCH_GROUP_CALC`.
    Muxed {
        paths: Vec<Vec<Option<MuxId>>>,
        data: Vec<MuxData>,
        block_info: Option<u32>,
        group_index_map: Option<MuxId>,
        offset_index_map: Option<MuxId>,
    },
    Dumb {
        arch_value: bool,
        bits: Vec<i32>,
    },
    Stlmap {
        paths: Vec<Vec<Option<MuxId>>>,
        data: Vec<StlmapData>,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ArchGroup {
    pub group_index: i32,
    pub arch_type: i32,
    pub block_type: i32,
    pub body: ArchBody,
}

impl ArchGroup {
    /// The mux paths of a muxed group: for LE-level groups one path per LE
    /// (16), each a list of muxes (levels of a two-level mux).
    pub fn paths(&self) -> &[Vec<Option<MuxId>>] {
        match &self.body {
            ArchBody::Muxed { paths, .. } | ArchBody::Stlmap { paths, .. } => paths,
            ArchBody::Dumb { .. } => &[],
        }
    }
    pub fn data(&self) -> &[MuxData] {
        match &self.body {
            ArchBody::Muxed { data, .. } => data,
            _ => &[],
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BlockInfo {
    pub block_type: i32,
    pub num_ram_cols: i32,
    /// Block instance base addresses, packed `row << 16 | column` in flat
    /// CRAM address space (flat = row * num_ram_cols + column).
    pub block_lists: BTreeMap<i32, Vec<u32>>,
    /// `(rows << 16) | columns` of one block instance.
    pub simple_block_lists: BTreeMap<i32, u32>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Asmdb {
    pub num_bits: i32,
    pub block_info_map: Vec<(i32, u32)>,
    pub arch_groups: Vec<ArchGroup>,
    pub muxes: Vec<Mux>,
    pub block_infos: Vec<BlockInfo>,
    pub index_maps: Vec<Vec<(i32, i32)>>,
    pub default_bits: Vec<i32>,
    pub default_mux_settings: Vec<(Option<u32>, i32)>,
    pub default_bit_calcs: Vec<(bool, i32, Option<u32>)>,
}

impl Asmdb {
    pub fn mux(&self, id: MuxId) -> &Mux {
        &self.muxes[id.0 as usize]
    }
    pub fn block_info(&self, block_type: i32) -> Option<&BlockInfo> {
        self.block_info_map
            .iter()
            .find(|(t, _)| *t == block_type)
            .and_then(|(_, i)| self.block_infos.get(*i as usize))
    }
    /// Groups of one type for one block type, by `m_group_index`.
    pub fn groups(&self, block_type: i32, arch_type: i32) -> Vec<&ArchGroup> {
        let mut v: Vec<_> = self
            .arch_groups
            .iter()
            .filter(|g| g.block_type == block_type && g.arch_type == arch_type)
            .collect();
        v.sort_by_key(|g| g.group_index);
        v
    }
}
