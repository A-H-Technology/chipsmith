//! The die's routing graph joined with its routing configuration bits: the
//! device side of "which CRAM bits does this routed net set". Quartus keeps
//! them in `ddb_magellan4_routing.ddb` and `ddb_magellan4_asm.ddb`;
//! cyclone2-re's `ddb` crate decodes those, and the shipped database
//! (`db.rs`) stores what it decoded.

use anyhow::{ensure, Result};
use std::collections::HashMap;

/// Bit 31 of a global id marks a routing element; the low 31 bits index
/// `Routing::elements` (libddb_dygr.so `DYGR_DIE_INFO_BODY::get_node_index_of_id`).
pub const ROUTER_ID_BIT: u32 = 0x8000_0000;

/// One routing element (a wire or a port): a tile and a shared template.
#[derive(Debug, Clone, Copy)]
pub struct Element {
    pub x: i16,
    pub y: i16,
    /// Global id that template fanout offsets are relative to.
    pub fanout_edge_base: u32,
    /// Index into `Routing::templates`.
    pub template: u32,
}

/// What identical elements share: type, z/index, wire length, direction,
/// and the fanout relative to the element's `fanout_edge_base`.
#[derive(Debug, Clone, Default)]
pub struct Template {
    /// `DEV_ELEMENT_ENUM` value (wire/port kind), named by `element_name`.
    pub element_enum: i16,
    pub num_edges: i16,
    pub z: i16,
    pub index: i16,
    /// Edges at or past this index are electrically redundant duplicates.
    pub first_purely_redundant_edge: i16,
    pub length: i8,
    pub metal_layer: i8,
    pub direction: i8,
    pub fanout_offsets: Vec<i32>,
    pub fanout_to_positions: Vec<i16>,
}

pub struct Routing {
    pub elements: Vec<Element>,
    pub templates: Vec<Template>,
}

impl Routing {
    /// Global ids of node `i`'s fanouts (edges past `first_purely_redundant_edge` included).
    pub fn fanout(&self, i: usize) -> impl Iterator<Item = u32> + '_ {
        let e = &self.elements[i];
        let t = &self.templates[e.template as usize];
        t.fanout_offsets
            .iter()
            .map(move |&o| e.fanout_edge_base.wrapping_add(o as u32))
    }
}

/// One configuration bit of a routing connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AsmBit {
    /// Flat CRAM address (`DYGR_MAX_CRAM_FLAT_ADDRESS` caps it at 2^31-1).
    pub flat_address: u32,
    pub use_encoded_setting: u8,
    pub is_encoded_bit_high: u8,
    pub is_strangely_encoded: u8,
    pub is_cff_bit: u8,
}

/// A routing element's configuration bits, split into groups: group k
/// holds the bits that turn on edge k of the element (libddb_dygr.so
/// `get_destination_bit_group`).
#[derive(Debug, Clone, Default)]
pub struct AsmNode {
    /// 1-based index into `bits` where each group starts; a group ends where
    /// the next begins (`DYGR_ROUTE_ASM_NODE::get_num_bits_in_group`).
    pub start_bit_index: Vec<u16>,
    pub bits: Vec<AsmBit>,
}

impl AsmNode {
    /// Bits of group `g`. A start index of 0 marks a group with no bits
    /// (`get_single_bit` asserts it is nonzero before indexing).
    pub fn group(&self, g: usize) -> &[AsmBit] {
        let start = self.start_bit_index[g] as usize;
        if start == 0 {
            return &[];
        }
        let end = self.start_bit_index[g + 1..]
            .iter()
            .find(|&&s| s != 0)
            .map_or(self.bits.len(), |&s| s as usize - 1);
        &self.bits[start - 1..end]
    }
}

/// Index into the die's routing element list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Node(pub u32);

pub struct Device {
    pub routing: Routing,
    pub asm: Vec<AsmNode>,
    by_loc: HashMap<(i16, i16, i16, i16), Vec<Node>>,
}

impl Device {
    /// The graph and its bits from wherever they were stored (Quartus'
    /// files, or the shipped database).
    pub fn new(routing: Routing, asm: Vec<AsmNode>) -> Result<Self> {
        ensure!(
            asm.len() == routing.elements.len(),
            "ASM node count != routing element count"
        );
        let mut by_loc: HashMap<_, Vec<Node>> = HashMap::new();
        for (i, e) in routing.elements.iter().enumerate() {
            let t = &routing.templates[e.template as usize];
            by_loc
                .entry((e.x, e.y, t.z, t.index))
                .or_default()
                .push(Node(i as u32));
        }
        Ok(Device {
            routing,
            asm,
            by_loc,
        })
    }

    pub fn element_enum(&self, n: Node) -> i16 {
        self.template(n).element_enum
    }

    /// The element's tile.
    pub fn xy(&self, n: Node) -> (i16, i16) {
        let e = &self.routing.elements[n.0 as usize];
        (e.x, e.y)
    }

    /// The element's index among its type in its tile (`I` in an rcf).
    pub fn index(&self, n: Node) -> i16 {
        self.template(n).index
    }

    fn template(&self, n: Node) -> &Template {
        &self.routing.templates[self.routing.elements[n.0 as usize].template as usize]
    }

    /// All routing elements at tile (x, y) with this z and index; several
    /// element types share each coordinate.
    pub fn at(&self, x: i16, y: i16, z: i16, index: i16) -> &[Node] {
        self.by_loc
            .get(&(x, y, z, index))
            .map_or(&[], Vec::as_slice)
    }

    /// Position of `dst` in `src`'s fanout list, which is also the ASM bit
    /// group that enables the edge (`get_destination_bit_group`).
    pub fn edge(&self, src: Node, dst: Node) -> Option<usize> {
        let gid = dst.0 | ROUTER_ID_BIT;
        self.routing.fanout(src.0 as usize).position(|g| g == gid)
    }

    /// CRAM bits that turn on edge `k` of `src`.
    pub fn edge_bits(&self, src: Node, k: usize) -> &[AsmBit] {
        self.asm[src.0 as usize].group(k)
    }
}

/// The chip database's (and the rcf's) name of a routing element type.
pub fn element_name(e: i16) -> String {
    match e {
        43 => "LOCAL_LINE",
        45 => "LE_BUFFER",
        264 => "R4",
        268 => "R24",
        269 => "C4",
        275 => "C16",
        276 => "LOCAL_INTERCONNECT",
        289 => "CLK_BUFFER",
        292 => "GLOBAL_CLK_H",
        293 => "GLOBAL_CLK_V",
        294 => "LAB_CLK",
        298 => "TB_IO_CLK",
        301 => "IO_DATAIN",
        513 => "CLKBUF_IN",
        515 => "CLKBUF_OUT",
        532 => "BLOCK_INPUT_MUX",
        512 => "CLKBUF_ENABLE",
        520 => "CLKPIN_TO_PLLCLKIN_MUX",
        329 => "PLL_CLKIN",
        69 => "PLL_BUFFER",
        347 => "PLL_LOCKED_PIN",
        560 => "BLK_CLK_BUF",
        // The rcf's names for the JTAG and serial-flash blocks' outputs.
        361 => "JTAG_TMSUTAP_PIN",
        362 => "JTAG_TCKUTAP_PIN",
        363 => "JTAG_TDIUTAP_PIN",
        364 => "JTAG_SHIFTUSER_PIN",
        365 => "JTAG_CLKDRUSER_PIN",
        366 => "JTAG_UPDATEUSER_PIN",
        367 => "JTAG_RUNIDLEUSER_PIN",
        368 => "JTAG_USR1USER_PIN",
        474 => "ASMI_DATAOUT",
        _ => return format!("E{e}"),
    }
    .to_string()
}
