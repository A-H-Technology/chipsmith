//! I/O registers (README (z10)): the input, output and output-enable
//! registers of an HVIO or IO96 pin, their controls' block pins and the
//! routing tiles' programmable inverters in front of them.
//!
//! [obs, Quartus' ct and iq images]: a pin's register controls are pins of
//! its I/O block next to the pin's own data pins. On an HVIO pin, with O the
//! block pin its OE path enters (`pin_nodes.tsv` role `oe`): O − 4 the input
//! register's synchronous clear, O − 3 the asynchronous clear (or preset)
//! all three registers share, O − 2 the output and OE registers' clock
//! enable, O − 1 the input register's clock enable, O + 1 the output data.
//! On an IO96 pin, with U the block pin its output data enters (role
//! `out`): U + 2 the asynchronous clear, U + 4 the input register's clock
//! enable, U + 5 the output and OE registers' clock enable. All of them are
//! active low (`nce`, `naclr`, `nsclr`); Quartus drives an active-high
//! signal through the RPI inverter of the I/O routing tile it passes
//! (`db/sm4revb/io/rpi.tsv`). The clock is the block's register clock pin.
//! A registered input leaves on the pin's `in1` node.

use super::det::HashMap;

use anyhow::{bail, Context, Result};

use super::graph::{Graph, NO, PASS};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Role {
    InSclr,
    Aclr,
    OutCe,
    InCe,
    Oe,
    Out,
    /// The block's register clock.
    Clk,
    /// The input register's output, on the pin's `in1` node (a driver).
    Q,
}

/// One HVIO or IO96 pin's register block.
#[derive(Clone, Debug)]
pub struct IoPin {
    pub ball: String,
    /// The I/O block instance (`hvio_3_1`, `pio_0_0`).
    pub uid: String,
    /// The pin's register attributes' path prefix in that instance.
    pub path: String,
    pub io96: bool,
    /// The block's node prefix (`HVIOWR_RE:X121Y30Z500I`).
    prefix: String,
    /// O (HVIO) or U (IO96).
    base: u32,
    /// The OE data block pin (IO96: from pin_nodes, not U-relative).
    oe: u32,
}

impl IoPin {
    pub fn block_pin(&self, r: Role) -> Option<String> {
        let o = self.base as i64;
        let i = if self.io96 {
            match r {
                Role::Aclr => o + 2,
                Role::InCe => o + 4,
                Role::OutCe => o + 5,
                Role::Out => o,
                Role::Oe => self.oe as i64,
                // Not seen on IO96 [unk].
                Role::InSclr | Role::Clk | Role::Q => return None,
            }
        } else {
            match r {
                Role::InSclr => o - 4,
                Role::Aclr => o - 3,
                Role::OutCe => o - 2,
                Role::InCe => o - 1,
                Role::Oe => o,
                Role::Out => o + 1,
                Role::Clk | Role::Q => return None,
            }
        };
        Some(format!("{}{i}", self.prefix))
    }
}

/// The block pin a fabric node's hops with no choice lead to (PASS, or a
/// bitless hop into a node with no other fanin): the I/O block pin an RPI
/// boundary node feeds.
pub fn forward_block(g: &Graph, single: &[bool], mut n: u32) -> Option<u32> {
    for _ in 0..8 {
        let t = g.type_name(n);
        if t == "HVIOWR_RE" || t == "IO96_RE" {
            return Some(n);
        }
        let next: Vec<u32> = g
            .fanout(n)
            .filter(|&e| g.class[e] == PASS || (g.class[e] == NO && single[g.dst[e] as usize]))
            .map(|e| g.dst[e])
            .collect();
        let [m] = next.as_slice() else { return None };
        n = *m;
    }
    None
}

/// Every ball's register block, from db/sm4revb/io/pins.tsv (pad names)
/// and pin_nodes.tsv (the oe and out boundary nodes).
pub fn pins(g: &Graph, db: &str) -> Result<HashMap<String, IoPin>> {
    let single = g.single_fanin();
    let mut nodes: HashMap<(String, String), u32> = HashMap::default();
    for l in std::fs::read_to_string(format!("{db}/io/pin_nodes.tsv"))?.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() == 5 && !l.starts_with('#') && f[0] != "ball" {
            if let Some(n) = g.node(f[3]) {
                nodes.insert((f[0].to_owned(), f[2].to_owned()), n);
            }
        }
    }
    let mut out = HashMap::default();
    for l in std::fs::read_to_string(format!("{db}/io/pins.tsv"))?.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 7 || l.starts_with('#') || f[0] == "ball" {
            continue;
        }
        let (ball, uid, pad) = (f[0], f[1], f[2]);
        let io96 = uid.starts_with("pio");
        let path = if io96 {
            // P<n>X..: lane n / 12, pin n % 12.
            let n: u32 = pad
                .strip_prefix('P')
                .and_then(|r| r.split('X').next())
                .context("IO96 pad")?
                .parse()?;
            format!("x0/xiophyadaptwr_{}/x_gpio_{}", n / 12, n % 12)
        } else {
            let p: Vec<&str> = pad.split('_').collect();
            let [_, b, gg, k, ..] = p.as_slice() else {
                bail!("HVIO pad {pad}")
            };
            format!(
                "xx20_u{}/xx4_u{gg}/xx1_u{k}/xiogpio_reg",
                b.trim_start_matches('B')
            )
        };
        let blk = |role: &str| {
            nodes
                .get(&(ball.to_owned(), role.to_owned()))
                .and_then(|&n| forward_block(g, &single, n))
        };
        let (Some(oe), Some(outb)) = (blk("oe"), blk("out")) else {
            continue;
        };
        let name = g.name(oe);
        let prefix = name[..name.rfind('I').context("node name")? + 1].to_owned();
        if g.name(outb)[..prefix.len()] != prefix {
            // The output data on another block than the OE (HVIO B5, IO96
            // DN25): IO96 counts from the output data, HVIO from the OE.
            if !io96 {
                continue;
            }
        }
        let (base, prefix) = if io96 {
            let n = g.name(outb);
            (
                g.i[outb as usize],
                n[..n.rfind('I').unwrap() + 1].to_owned(),
            )
        } else {
            (g.i[oe as usize], prefix)
        };
        out.insert(
            ball.to_owned(),
            IoPin {
                ball: ball.to_owned(),
                uid: uid.to_owned(),
                path,
                io96,
                prefix,
                base,
                oe: g.i[oe as usize],
            },
        );
    }
    Ok(out)
}

/// The RPI nodes of `db/sm4revb/io/rpi.tsv`, keyed by (tile, attribute path).
pub fn rpis(g: &Graph, db: &str) -> Result<HashMap<(String, String), u32>> {
    let mut m = HashMap::default();
    for l in std::fs::read_to_string(format!("{db}/io/rpi.tsv"))
        .context("reading io/rpi.tsv")?
        .lines()
    {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() == 3 && !l.starts_with('#') && f[0] != "uid" {
            if let Some(n) = g.node(f[2]) {
                m.insert((f[0].to_owned(), f[1].to_owned()), n);
            }
        }
    }
    Ok(m)
}

/// What a5pnr puts in one pin's I/O cell (pins.txt fourth column `reg`):
/// the flops by netlist index and the controls they share.
#[derive(Clone, Debug)]
pub struct IoRegs {
    pub port: usize,
    pub input: Option<usize>,
    pub output: Option<usize>,
    pub oe: Option<usize>,
    /// The `$_TBUF_` an OE register drives.
    pub tbuf: Option<usize>,
    pub clk: u32,
    /// The input register's enable and the output and OE registers' one
    /// (a constant-1 LUT where the flop has none: Quartus drives every
    /// enable, iq).
    pub in_ce: Option<u32>,
    pub out_ce: Option<u32>,
    pub aclr: Option<super::netlist::Aclr>,
    pub sclr: Option<u32>,
}

impl IoRegs {
    pub fn flops(&self) -> impl Iterator<Item = usize> + '_ {
        [self.input, self.output, self.oe].into_iter().flatten()
    }
}

/// Moves each `reg` port's flops into its I/O cell: an input's one flop
/// on the pin, an output's flop driving it (and through a `$_TBUF_`, its
/// OE flop). Each must have nothing else on its pin side; the output and
/// OE flops share the enable and the clear, as the block does, and only
/// the input register has a synchronous clear (README (z10)).
pub fn choose(nl: &mut super::netlist::Netlist, ports: &[usize]) -> Result<Vec<IoRegs>> {
    use super::netlist::{Cell, Sig};
    let mut loads: HashMap<u32, usize> = HashMap::default();
    let mut drv: HashMap<u32, usize> = HashMap::default();
    for (i, c) in nl.cells.iter().enumerate() {
        let ins: Vec<Sig> = match c {
            Cell::Lut { ins, out, .. } => {
                drv.insert(*out, i);
                ins.iter().map(|&n| Sig::Net(n)).collect()
            }
            Cell::Ff(f) => {
                drv.insert(f.q, i);
                [f.d, Sig::Net(f.clk)]
                    .into_iter()
                    .chain(
                        [f.ce, f.sclr, f.aclr.map(|a| a.net)]
                            .into_iter()
                            .flatten()
                            .map(Sig::Net),
                    )
                    .collect()
            }
            Cell::Tbuf { a, e, y } => {
                drv.insert(*y, i);
                vec![*a, *e]
            }
            Cell::Arith { c, d, ci, .. } => [*ci]
                .into_iter()
                .chain([c, d].into_iter().flatten().map(|&n| Sig::Net(n)))
                .collect(),
            Cell::M20k(m) => m
                .waddr
                .iter()
                .chain(&m.wdata)
                .chain(&m.raddr)
                .chain(&m.be)
                .chain([&m.we, &m.re])
                .copied()
                .chain([Sig::Net(m.clk)])
                .collect(),
            Cell::Mlab(m) => m
                .waddr
                .iter()
                .chain(&m.raddr)
                .chain([&m.wdata, &m.we])
                .copied()
                .chain([Sig::Net(m.clk)])
                .collect(),
            Cell::Pll { refclk, out, .. } => {
                drv.insert(*out, i);
                vec![Sig::Net(*refclk)]
            }
            Cell::Mul { a, b, p, .. } => {
                for n in p.iter().flatten() {
                    drv.insert(*n, i);
                }
                a.iter().chain(b).copied().collect()
            }
        };
        for s in ins {
            if let Sig::Net(n) = s {
                *loads.entry(n).or_default() += 1;
            }
        }
    }
    for p in nl.ports.iter().filter(|p| !p.input) {
        if let Sig::Net(n) = p.net {
            *loads.entry(n).or_default() += 1;
        }
    }
    let ff = |i: usize| -> Option<super::netlist::Ff> {
        if let Cell::Ff(f) = &nl.cells[i] {
            Some(f.clone())
        } else {
            None
        }
    };
    let mut out = Vec::new();
    let mut need_const = false;
    for &pi in ports {
        let p = &nl.ports[pi];
        let Sig::Net(n) = p.net else {
            bail!("{}: a constant port has no I/O register", p.name)
        };
        let mut r = IoRegs {
            port: pi,
            input: None,
            output: None,
            oe: None,
            tbuf: None,
            clk: 0,
            in_ce: None,
            out_ce: None,
            aclr: None,
            sclr: None,
        };
        if p.input {
            let on: Vec<usize> = nl
                .cells
                .iter()
                .enumerate()
                .filter(|(_, c)| matches!(c, Cell::Ff(f) if f.d == Sig::Net(n)))
                .map(|(i, _)| i)
                .collect();
            let [i] = on.as_slice() else {
                bail!(
                    "{}: an input register needs exactly one flop on the pin, not {}",
                    p.name,
                    on.len()
                )
            };
            if loads[&n] != 1 {
                bail!("{}: the pin feeds more than its register", p.name);
            }
            let f = ff(*i).unwrap();
            r.input = Some(*i);
            r.clk = f.clk;
            r.in_ce = f.ce;
            r.aclr = f.aclr;
            r.sclr = f.sclr;
            need_const |= f.ce.is_none();
        } else {
            let lone = |q: Sig| -> Option<usize> {
                let Sig::Net(q) = q else { return None };
                drv.get(&q)
                    .copied()
                    .filter(|&d| matches!(nl.cells[d], Cell::Ff(_)) && loads.get(&q) == Some(&1))
            };
            let (data, oe) = match drv.get(&n).map(|&d| &nl.cells[d]) {
                Some(Cell::Tbuf { a, e, .. }) => {
                    r.tbuf = drv.get(&n).copied();
                    (
                        lone(*a),
                        Some(lone(*e).with_context(|| {
                            format!("{}: the OE isn't a flop of its own", p.name)
                        })?),
                    )
                }
                _ => (lone(Sig::Net(n)), None),
            };
            let data = data.with_context(|| {
                format!("{}: the output isn't driven by a flop of its own", p.name)
            })?;
            let (fd, fe) = (ff(data).unwrap(), oe.and_then(ff));
            if let Some(fe) = &fe {
                if fe.clk != fd.clk || fe.ce != fd.ce || fe.aclr != fd.aclr {
                    bail!(
                        "{}: the output and OE registers share the clock, enable and clear",
                        p.name
                    );
                }
            }
            if fd.sclr.is_some() || fe.as_ref().is_some_and(|f| f.sclr.is_some()) {
                bail!(
                    "{}: an output register has no synchronous clear (fold it into the data)",
                    p.name
                );
            }
            r.output = Some(data);
            r.oe = oe;
            r.clk = fd.clk;
            r.out_ce = fd.ce;
            r.aclr = fd.aclr;
            need_const |= fd.ce.is_none();
        }
        out.push(r);
    }
    if need_const {
        let n = nl.nets.len() as u32;
        nl.nets.push("io_const1".into());
        nl.cells.push(Cell::Lut {
            ins: vec![],
            mask: 1,
            out: n,
        });
        for r in out.iter_mut() {
            if r.input.is_some() {
                r.in_ce.get_or_insert(n);
            } else {
                r.out_ce.get_or_insert(n);
            }
        }
    }
    Ok(out)
}

/// The RPI each I/O control's block pin is fed from: where routing ends
/// (the hops after it have no bits).
pub fn rpi_of_block(g: &Graph, rpis: &HashMap<(String, String), u32>) -> HashMap<u32, u32> {
    let single = g.single_fanin();
    rpis.values()
        .filter_map(|&r| forward_block(g, &single, r).map(|b| (b, r)))
        .collect()
}

/// The IO96 lane pin a node's hops with no choice lead to: the
/// `IOFBRADAPT_IO12LANE_RE` node's (X, I). Its X names the lane (X72 lane
/// 4, X73 lane 5, from the pins' output data paths).
pub fn io12lane_of(g: &Graph, mut n: u32) -> Option<(u32, u32)> {
    for _ in 0..12 {
        if g.type_name(n) == "IOFBRADAPT_IO12LANE_RE" {
            return Some((g.x[n as usize] as u32, g.i[n as usize]));
        }
        let next: Vec<u32> = g.fanout(n).map(|e| g.dst[e]).collect();
        let [m] = next.as_slice() else { return None };
        n = *m;
    }
    None
}

/// An IO96 lane's two register clock pins: I901 clocks the input
/// registers, I902 the output and OE registers.
const LANE_IN_CLK: u32 = 901;
const LANE_OUT_CLK: u32 = 902;

/// Where a pin's I/O registers take their clock: the routing node the
/// block's register clock pin is entered from. HVIO [obs: Quartus' iq uses
/// hvio_3_1's at HIOL row 49, hvio_0_1's at row 9; inf: each block's top row
/// serves it all]. IO96: each lane has two, the VIO `GLOBAL_LOCAL_SEL` I3
/// whose RPI reaches the lane's I901 (input registers) or I902 (output and
/// OE registers): lane 4 X98 and X99, lane 5 X106 and X107 [obs, graph;
/// board 2026-11-06: lane 4's outputs on X106 (lane 5's input clock, which
/// phase 10 took for lane 4's) held their reset value, on X99 they run;
/// Quartus' iq drives X99 for lane 4's output and OE registers, X106 and
/// X107 for lane 5's input and output ones]. Other lanes unknown.
pub fn clock_node(g: &Graph, p: &IoPin, input: bool) -> Result<String> {
    let lane = |l4: &str, l5: &str| -> Result<String> {
        let e = if p.path.starts_with("x0/xiophyadaptwr_4/") {
            l4
        } else if p.path.starts_with("x0/xiophyadaptwr_5/") {
            l5
        } else {
            bail!(
                "{}: I/O registers in {} {} aren't modelled (no clock entry seen)",
                p.ball,
                p.uid,
                p.path
            )
        };
        let n = format!("GLOBAL_LOCAL_SEL:X{e}Y5Z0I3");
        // The clock must reach the lane the pin's own data enters.
        let want = (
            p.block_pin(Role::Out)
                .and_then(|b| g.node(&b))
                .and_then(|b| io12lane_of(g, b))
                .map(|(x, _)| x),
            if input { LANE_IN_CLK } else { LANE_OUT_CLK },
        );
        let got = g.node(&n).and_then(|c| io12lane_of(g, c));
        match (want, got) {
            ((Some(x), i), Some(c)) if c == (x, i) => Ok(n),
            _ => bail!(
                "{}: clock entry {n} reaches lane pin {got:?}, the pin's lane wants {want:?}",
                p.ball
            ),
        }
    };
    match (p.uid.as_str(), p.io96) {
        ("hvio_3_1", _) => Ok("RPI:X121Y49Z0I29".into()),
        ("hvio_0_1", _) => Ok("RPI:X121Y9Z0I29".into()),
        ("pio_0_0", true) if input => lane("98", "106"),
        ("pio_0_0", true) => lane("99", "107"),
        _ => bail!(
            "{}: I/O registers in {} {} aren't modelled (no clock entry seen)",
            p.ball,
            p.uid,
            p.path
        ),
    }
}
