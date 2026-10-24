//! Who drives and who reads each net.

use crate::cell::PortSig;
use crate::module::{CellId, Module, WireId};
use crate::sig::Net;

/// One bit of a cell port or module port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endpoint {
    Cell {
        cell: CellId,
        /// Which of the cell's ports, in [`CellKind::for_each_port`]
        /// order; [`Connectivity::port`] names it.
        ///
        /// [`CellKind::for_each_port`]: crate::CellKind::for_each_port
        port: u32,
        bit: u32,
    },
    /// A module port: an input drives its nets, an output reads them.
    Port { wire: WireId, bit: u32 },
}

/// A snapshot of a module's connectivity: for each net, the endpoints
/// driving it and the ones reading it. Built in one pass over the module
/// (linear time), and stale as soon as the module changes; passes rebuild
/// it rather than keep it in sync.
///
/// A port of unknown direction (a cell of a type yosys knew nothing about)
/// counts as both, as does an inout.
pub struct Connectivity {
    driver_start: Vec<u32>,
    drivers: Vec<Endpoint>,
    user_start: Vec<u32>,
    users: Vec<Endpoint>,
}

impl Connectivity {
    pub fn new(m: &Module) -> Connectivity {
        // Compressed rows: count per net, prefix sums, then fill.
        let n = m.net_count();
        let mut driver_start = vec![0u32; n + 1];
        let mut user_start = vec![0u32; n + 1];
        each_endpoint(m, |net, _, drives, reads| {
            if drives {
                driver_start[net.index() + 1] += 1;
            }
            if reads {
                user_start[net.index() + 1] += 1;
            }
        });
        for i in 0..n {
            driver_start[i + 1] += driver_start[i];
            user_start[i + 1] += user_start[i];
        }
        let placeholder = Endpoint::Port { wire: 0, bit: 0 };
        let mut drivers = vec![placeholder; driver_start[n] as usize];
        let mut users = vec![placeholder; user_start[n] as usize];
        let mut next_driver = driver_start.clone();
        let mut next_user = user_start.clone();
        each_endpoint(m, |net, e, drives, reads| {
            let i = net.index();
            if drives {
                drivers[next_driver[i] as usize] = e;
                next_driver[i] += 1;
            }
            if reads {
                users[next_user[i] as usize] = e;
                next_user[i] += 1;
            }
        });
        Connectivity {
            driver_start,
            drivers,
            user_start,
            users,
        }
    }

    /// The endpoints driving `net`: one, normally; none if it's undriven,
    /// several for a tri-state bus or a conflict.
    pub fn drivers(&self, net: Net) -> &[Endpoint] {
        let i = net.index();
        &self.drivers[self.driver_start[i] as usize..self.driver_start[i + 1] as usize]
    }

    /// The single driver, if there's exactly one.
    pub fn driver(&self, net: Net) -> Option<Endpoint> {
        match self.drivers(net) {
            [e] => Some(*e),
            _ => None,
        }
    }

    pub fn users(&self, net: Net) -> &[Endpoint] {
        let i = net.index();
        &self.users[self.user_start[i] as usize..self.user_start[i + 1] as usize]
    }

    /// The cell port an endpoint is on, and the bit's index within the
    /// whole port (a piece of a `$mem_v2` port starts past the earlier
    /// pieces).
    pub fn port<'m>(m: &'m Module, e: Endpoint) -> Option<(PortSig<'m>, usize)> {
        let Endpoint::Cell { cell, port, bit } = e else {
            return None;
        };
        let mut found = None;
        let mut i = 0;
        let mut offset = 0;
        let mut prev: Option<&str> = None;
        m.cells[cell].kind.for_each_port(|p| {
            if prev != Some(p.name) {
                offset = 0;
            }
            if i == port {
                found = Some((p, offset + bit as usize));
            }
            offset += p.bits.len();
            prev = Some(p.name);
            i += 1;
        });
        found
    }
}

fn each_endpoint(m: &Module, mut f: impl FnMut(Net, Endpoint, bool, bool)) {
    for (wire, w, dir) in m.ports() {
        for (bit, b) in w.bits.iter().enumerate() {
            if let Some(net) = b.net() {
                let e = Endpoint::Port {
                    wire,
                    bit: bit as u32,
                };
                // Seen from inside the module: an input port drives.
                f(net, e, dir.reads(), dir.drives());
            }
        }
    }
    for (cell, c) in m.cells.iter().enumerate() {
        let mut port = 0;
        c.kind.for_each_port(|p| {
            let (drives, reads) = p.dir.map_or((true, true), |d| (d.drives(), d.reads()));
            for (bit, b) in p.bits.iter().enumerate() {
                if let Some(net) = b.net() {
                    let e = Endpoint::Cell {
                        cell,
                        port,
                        bit: bit as u32,
                    };
                    f(net, e, drives, reads);
                }
            }
            port += 1;
        });
    }
}
