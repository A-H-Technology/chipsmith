//! Place and route for the Agilex 5 A5EB013B (agilex5-re's `a5pnr`,
//! README (w), (x)-(z)): reads a netlist in yosys' JSON (`synth`'s), packs
//! it into ALMs, places them by simulated annealing, routes every net with
//! PathFinder over the routing graph dumped from Quartus (plus `a5db
//! usable`'s usable.bin) and writes the design file `rebuild::build`
//! assembles. pins.txt: `<port> <ball> <3.3|1.8|1.1>` lines, or `<port>
//! node <graph node>` for an output that ends inside the fabric. delays.tsv:
//! agilex5-re's `tools/timing/fit.py` model (node costs, timing).

mod det;
mod dsp;
mod emit;
mod graph;
mod ioreg;
mod netlist;
pub mod pack;
mod place;
mod pll;
mod route;
mod sta;

use std::fmt::Write as _;

use anyhow::{bail, Context, Result};

use det::HashMap;
use netlist::Sig;
use pack::End;

pub struct Options {
    /// The placement seed; a placement whose routing doesn't converge is
    /// placed again with the next one.
    pub seed: u64,
    /// Annealing moves per temperature, times N^(4/3).
    pub effort: f64,
    /// Timing-driven placement and routing; else wire length only.
    pub timing_driven: bool,
    pub pack: pack::PackMode,
    /// Let the router move a LUT input to another pin of its LUT.
    pub swap_pins: bool,
}

impl Default for Options {
    fn default() -> Options {
        Options {
            seed: 1,
            effort: 1.0,
            timing_driven: true,
            pack: pack::PackMode::Pairs,
            swap_pins: true,
        }
    }
}

/// A flop's names for the ffmap (every visible name of its output, `!`
/// before each when the image holds the inverse).
fn ff_names(nl: &netlist::Netlist, i: usize, inv: bool) -> String {
    let netlist::Cell::Ff(f) = &nl.cells[i] else {
        unreachable!("an I/O register is a flop")
    };
    let names = nl
        .aliases
        .get(&f.q)
        .cloned()
        .unwrap_or_else(|| vec![nl.nets[f.q as usize].clone()]);
    names
        .iter()
        .map(|n| format!("{}{n}", if inv { "!" } else { "" }))
        .collect::<Vec<_>>()
        .join("|")
}

/// Place and route `top` of the netlist `json` and write the design file
/// `out` (and its flop map, `<out>.ffmap`).
#[allow(clippy::too_many_arguments)]
pub fn run(
    db: &str,
    gdir: &str,
    delays: &str,
    json: &str,
    top: &str,
    pins: &str,
    out: &str,
    opts: &Options,
) -> Result<()> {
    let mut seed = opts.seed;
    let t0 = std::time::Instant::now();
    let mut nl = netlist::read(json, top)?;
    // Pins: port → (ball, std), and the ports whose flops go into the pin's
    // I/O cell (a fourth column `reg`, README (z10)).
    let mut pin_of: HashMap<String, (String, String)> = HashMap::default();
    let mut reg_ports: Vec<String> = Vec::new();
    // `<port> node <graph node>`: an output that ends on a fabric node instead
    // of a pin, e.g. an HPS input's RPII (README, "HPS-first").
    let mut sink_of: HashMap<String, String> = HashMap::default();
    for l in std::fs::read_to_string(pins)
        .context("reading pins")?
        .lines()
    {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() == 3 && f[1] == "node" && !f[0].starts_with('#') {
            sink_of.insert(f[0].to_owned(), f[2].to_owned());
        } else if f.len() >= 3 && !f[0].starts_with('#') {
            pin_of.insert(f[0].to_owned(), (f[1].to_owned(), f[2].to_owned()));
            if f.get(3) == Some(&"reg") {
                reg_ports.push(f[0].to_owned());
            }
        }
    }
    let reg_idx: Vec<usize> = reg_ports
        .iter()
        .map(|n| {
            nl.ports
                .iter()
                .position(|p| &p.name == n)
                .with_context(|| format!("pins.txt: no port {n}"))
        })
        .collect::<Result<_>>()?;
    let io = ioreg::choose(&mut nl, &reg_idx)?;
    netlist::tie_pll_resets(&mut nl);
    let nl = nl;
    let mut packed = pack::pack(&nl, opts.pack, &io)?;
    let [n6, n55, n5, n_arith, n_ff, n_ram] = sta::alm_kinds(&packed);
    let n_ffs = nl
        .cells
        .iter()
        .filter(|c| matches!(c, netlist::Cell::Ff(_)))
        .count();
    let ff_alms = packed
        .alms
        .iter()
        .filter(|a| {
            a.slots
                .iter()
                .any(|s| matches!(s, Some(pack::Slot::Ff { .. })))
        })
        .count();
    eprintln!(
        "pack: {} cells into {} ALMs ({n6} 6-LUT, {n55} two 5-LUTs, {n5} one 5-LUT, {n_arith} arithmetic in {} chains, {n_ff} flops only, {n_ram} MLAB RAM in {} MLABs; {ff_alms} with flops), {n_ffs} flops, {} M20Ks, {} clocks",
        nl.cells.len(),
        packed.alms.len(),
        packed.chains.len(),
        packed.mlabs.len(),
        packed.m20ks.len(),
        packed.clocks.len()
    );

    // The balls' boundary nodes (README (t)).
    let mut nodes_of: HashMap<(String, String), String> = HashMap::default();
    let mut io96: HashMap<String, bool> = HashMap::default();
    for l in std::fs::read_to_string(format!("{db}/io/pin_nodes.tsv"))?.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() == 5 && !l.starts_with('#') && f[0] != "ball" {
            nodes_of.insert((f[0].to_owned(), f[2].to_owned()), f[3].to_owned());
            io96.insert(f[0].to_owned(), f[1].starts_with("pio"));
        }
    }
    let g = graph::Graph::load(std::path::Path::new(gdir))?;
    eprintln!(
        "graph: {} nodes, {} edges, {:.0} s",
        g.len(),
        g.dst.len(),
        t0.elapsed().as_secs_f64()
    );
    let mut port_node: HashMap<usize, u32> = HashMap::default();
    let mut pin_lines = Vec::new();
    let mut pll_ball: HashMap<usize, String> = HashMap::default();
    for (pi, p) in nl.ports.iter().enumerate() {
        if let Some(node) = sink_of.get(&p.name) {
            anyhow::ensure!(!p.input, "port {}: only outputs can end on a node", p.name);
            port_node.insert(pi, g.node(node).with_context(|| format!("no node {node}"))?);
            continue;
        }
        let Some((ball, std)) = pin_of.get(&p.name) else {
            if matches!(p.net, Sig::Net(n) if packed.nets.contains_key(&n)) {
                bail!("port {} has no pin in {pins}", p.name);
            }
            continue;
        };
        let pll_ref = p.input && nl.cells.iter().any(|c| matches!((c, p.net), (netlist::Cell::Pll { refclk, .. }, Sig::Net(n)) if *refclk == n));
        if pll_ref {
            // A PLL's reference stays in its I/O bank: a clock pin, no route.
            pll_ball.insert(pi, ball.clone());
            pin_lines.push(format!("pin\t{ball}=clock_in:{std}"));
            continue;
        }
        let is_clock = p.input && matches!(p.net, Sig::Net(n) if packed.clocks.contains(&n));
        let registered = io.iter().any(|r| r.port == pi);
        let (role, usage) = if is_clock {
            ("clock", "clock_in")
        } else if p.input && registered {
            // A registered input leaves on in1; on IO96 its pin use is
            // in_reg (README (z10)).
            (
                "in1",
                if io96.get(ball) == Some(&true) {
                    "in_reg"
                } else {
                    "in"
                },
            )
        } else if p.input {
            (
                if io96.get(ball) == Some(&true) {
                    "in0"
                } else {
                    "in1"
                },
                "in",
            )
        } else {
            ("out", "out")
        };
        let node = nodes_of
            .get(&(ball.clone(), role.to_owned()))
            .with_context(|| format!("{ball} has no {role} node"))?;
        port_node.insert(pi, g.node(node).with_context(|| format!("no node {node}"))?);
        pin_lines.push(format!("pin\t{ball}={usage}:{std}"));
    }
    // PLL k: its reference ball, output node and iopll.py's lines. Output 0
    // of V16's PLL enters the DCM from HVIOWR_RE I16 (pl) [obs]; the other
    // reference pins' PLLs aren't wired up yet.
    let mut pll_node: Vec<u32> = Vec::new();
    let mut pll_rst_node: Vec<u32> = Vec::new();
    let mut pll_text = String::new();
    for &ci in &packed.plls {
        let netlist::Cell::Pll {
            refclk,
            ref_khz,
            out_khz,
            ..
        } = nl.cells[ci]
        else {
            unreachable!()
        };
        let pi = nl
            .ports
            .iter()
            .position(|p| p.net == Sig::Net(refclk))
            .context("a PLL's reference must be an input port")?;
        let ball = pll_ball
            .get(&pi)
            .with_context(|| format!("PLL reference {} has no pin", nl.ports[pi].name))?;
        let node = match ball.as_str() {
            "V16" => "HVIOWR_RE:X121Y30Z500I16",
            b => bail!("a PLL on reference pin {b}: only V16's is modelled"),
        };
        pll_node.push(g.node(node).with_context(|| format!("no node {node}"))?);
        // Its reset enters the block at HVIOWR_RE I2715 from this RPI (pl).
        let rst = "RPI:X121Y46Z0I38";
        pll_rst_node.push(g.node(rst).with_context(|| format!("no node {rst}"))?);
        pll_text.push_str(&pll::lines(db, ball, ref_khz, &[out_khz])?);
    }
    // I/O cells (README (z10)): each register's data and control nodes, the
    // RPIs to invert (every active-high control, and the OE register's
    // data: it holds the inverse) and the block settings.
    let io_pins = ioreg::pins(&g, db)?;
    let rpis = ioreg::rpis(&g, db)?;
    let rpi_blk = ioreg::rpi_of_block(&g, &rpis);
    let rpi_name: HashMap<u32, (String, String)> =
        rpis.iter().map(|(k, &v)| (v, k.clone())).collect();
    let mut io_node: HashMap<(usize, ioreg::Role), u32> = HashMap::default();
    let mut io_attrs: Vec<(String, String, String)> = Vec::new();
    let mut io_ffmap = String::new();
    {
        use ioreg::Role;
        let mut flyby: std::collections::BTreeSet<(String, String)> = Default::default();
        for r in &io {
            let pname = &nl.ports[r.port].name;
            let ball = &pin_of[pname].0;
            let p = io_pins.get(ball).with_context(|| {
                format!("{pname}: ball {ball} has no I/O register block in the model")
            })?;
            let ctl = |role: Role| -> Result<u32> {
                let b = p
                    .block_pin(role)
                    .and_then(|n| g.node(&n))
                    .with_context(|| format!("{ball}: no {role:?} pin"))?;
                rpi_blk
                    .get(&b)
                    .copied()
                    .with_context(|| format!("{ball}: no RPI feeds {}", g.name(b)))
            };
            if p.io96 && r.input.is_some() && (r.output.is_some() || r.oe.is_some()) {
                anyhow::bail!("{pname}: an IO96 pin's input and output registers take different lane clocks, one port can't have both");
            }
            let clk = ioreg::clock_node(&g, p, r.input.is_some())?;
            io_node.insert(
                (r.port, Role::Clk),
                g.node(&clk).with_context(|| format!("no node {clk}"))?,
            );
            let mut inv: Vec<u32> = Vec::new();
            let mut set = |sub: &str, v: &str| {
                io_attrs.push((p.uid.clone(), format!("{}/{sub}", p.path), v.to_owned()))
            };
            if let Some(i) = r.input {
                io_node.insert(
                    (r.port, Role::Q),
                    *port_node.get(&r.port).context("registered input node")?,
                );
                io_node.insert((r.port, Role::InCe), ctl(Role::InCe)?);
                inv.push(ctl(Role::InCe)?);
                set("xio_gpio_ireg/xinv_fr_in_clk/a_rb_sel", "IN_INV");
                set(
                    "xio_gpio_ireg/xio_gpio_in_clk_rst_gen/a_rb_fr_in_clk_ena",
                    "FR_IN_CLK_ENA",
                );
                set(
                    "xio_gpio_ireg/xio_gpio_in_clk_rst_gen/a_rb_ncein_ireg_ena",
                    "NCEIN_IREG_ENA",
                );
                set("xio_gpio_ireg_4to1_mux1/a_rb_mux_sel", "SEL1");
                if r.aclr.is_some() {
                    set(
                        "xio_gpio_ireg/xio_gpio_in_clk_rst_gen/a_rb_naclr_ireg_ena",
                        "NACLR_IREG_ENA",
                    );
                }
                if r.sclr.is_some() {
                    io_node.insert((r.port, Role::InSclr), ctl(Role::InSclr)?);
                    inv.push(ctl(Role::InSclr)?);
                    set(
                        "xio_gpio_ireg/xio_gpio_in_clk_rst_gen/a_rb_nsclr_ireg_ena",
                        "NSCLR_IREG_ENA",
                    );
                }
                // An IO96 lane with a registered input sets its twelve flyby
                // muxes to GPIO [obs, iq lane 5].
                if p.io96 {
                    let lane = p
                        .path
                        .trim_start_matches("x0/xiophyadaptwr_")
                        .split('/')
                        .next()
                        .unwrap_or("")
                        .to_owned();
                    flyby.insert((p.uid.clone(), lane));
                }
                writeln!(
                    io_ffmap,
                    "{}\tio_{}_i",
                    ff_names(&nl, i, false),
                    pname.replace(['[', ']'], "_")
                )?;
            }
            if let Some(o) = r.output {
                io_node.insert((r.port, Role::Out), ctl(Role::Out)?);
                io_node.insert((r.port, Role::OutCe), ctl(Role::OutCe)?);
                inv.push(ctl(Role::OutCe)?);
                set(
                    "xio_gpio_oreg/xio_gpio_out_clk_rst_gen/a_rb_fr_out_clk_oreg_ena",
                    "FR_OUT_CLK_OREG_ENA",
                );
                set(
                    "xio_gpio_oreg/xio_gpio_out_clk_rst_gen/a_rb_nceout_oreg_ena",
                    "NCEOUT_OREG_ENA",
                );
                set("xio_gpio_oreg_4to1_mux/a_rb_mux_sel", "SEL1");
                if r.aclr.is_some() {
                    set(
                        "xio_gpio_oreg/xio_gpio_out_clk_rst_gen/a_rb_naclr_oreg_ena",
                        "NACLR_OREG_ENA",
                    );
                }
                writeln!(
                    io_ffmap,
                    "{}\tio_{}_o",
                    ff_names(&nl, o, false),
                    pname.replace(['[', ']'], "_")
                )?;
            }
            if let Some(e) = r.oe {
                io_node.insert((r.port, Role::Oe), ctl(Role::Oe)?);
                inv.push(ctl(Role::Oe)?);
                set(
                    "xio_gpio_ereg/xio_gpio_oe_clk_rst_gen/a_rb_fr_out_clk_ereg_ena",
                    "FR_OUT_CLK_EREG_ENA",
                );
                set(
                    "xio_gpio_ereg/xio_gpio_oe_clk_rst_gen/a_rb_nceout_ereg_ena",
                    "NCEOUT_EREG_ENA",
                );
                set("xio_gpio_ereg_4to1_mux/a_rb_mux_sel", "SEL1");
                if r.aclr.is_some() {
                    // The OE register holds the inverse: its clear is a preset.
                    set(
                        "xio_gpio_ereg/xio_gpio_oe_clk_rst_gen/a_rb_naclr_ereg_ena",
                        "NACLR_EREG_ENA",
                    );
                    set(
                        "xio_gpio_ereg/xio_gpio_oe_clk_rst_gen/a_rb_naclr_ereg_sel",
                        "EREG_NPRE_SEL",
                    );
                }
                writeln!(
                    io_ffmap,
                    "{}\tio_{}_e",
                    ff_names(&nl, e, true),
                    pname.replace(['[', ']'], "_")
                )?;
            }
            if let Some(a) = r.aclr {
                io_node.insert((r.port, Role::Aclr), ctl(Role::Aclr)?);
                if !a.active_low {
                    inv.push(ctl(Role::Aclr)?);
                }
            }
            for n in inv {
                let (u, path) = rpi_name
                    .get(&n)
                    .with_context(|| format!("{} has no inverter", g.name(n)))?;
                io_attrs.push((u.clone(), format!("{path}/rpi_sel"), "INV".into()));
            }
        }
        for (uid, lane) in flyby {
            for k in 0..12 {
                io_attrs.push((
                    uid.clone(),
                    format!("x0/xiofbradapt/xio12_{lane}/xp2c/xflyby/x{k}/sel_f"),
                    "GPIO".into(),
                ));
            }
        }
        io_attrs.sort();
        io_attrs.dedup();
    }
    let port_xy: HashMap<usize, (u32, u32)> = port_node
        .iter()
        .map(|(&p, &n)| (p, (g.x[n as usize] as u32, g.y[n as usize] as u32)))
        .collect();
    let labs = place::labs(db)?;
    let sites = place::m20k_sites(db)?;
    let dsites = place::dsp_sites(db)?;
    let mut delay: HashMap<String, f32> = HashMap::default();
    for l in std::fs::read_to_string(delays)
        .context("reading delays")?
        .lines()
        .skip(1)
    {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() >= 3 {
            delay.insert(f[0].to_owned(), f[2].parse()?);
        }
    }
    let base = route::base_costs(&g, &delay);
    let sta = sta::Sta::new(&nl, &packed, &delay, &io);

    // Placement: wire length first; then, timing-driven, again with the
    // connections' criticalities from that placement (README (z)). A
    // placement whose routing doesn't converge is placed again with the next
    // seed: with every LUT input fixed to a pin at packing, two sinks in a
    // LAB can need the same local line (the CPU, specimens/p8/cpu/r4).
    let mut attempts = 0;
    let (pl, place_s, lines, rnets, _sink_conns, routed, route_s) = loop {
        let t1 = std::time::Instant::now();
        let mut pl = place::place(
            &packed,
            &nl,
            &labs,
            &sites,
            &dsites,
            &port_xy,
            seed,
            None,
            opts.effort,
        )?;
        let mut dtab = None;
        if opts.timing_driven {
            let t = route::delay_table(&g, &base, &labs)?;
            let est = |pl: &place::Placement| -> Vec<f32> {
                sta.conns
                    .iter()
                    .map(|(n, e)| t.conn(&packed, pl, &labs, &port_xy, packed.nets[n].0, *e))
                    .collect()
            };
            let r0 = sta.analyse(&est(&pl));
            eprintln!(
                "timing: wire-length placement, {:.3} ns estimated ({})",
                r0.worst, r0.at
            );
            // Bench knobs (tools/bench/pnrbench.sh): the timing term's weight
            // and the criticality exponent.
            let knob = |k: &str, d: f64| {
                std::env::var(k)
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(d)
            };
            let (lambda, exp) = (knob("A5PNR_TD_LAMBDA", 0.5), knob("A5PNR_TD_EXP", 8.0));
            let conns: Vec<(End, End, f64)> = sta
                .conns
                .iter()
                .enumerate()
                .map(|(i, (n, e))| (packed.nets[n].0, *e, (r0.crit[i] as f64).powf(exp)))
                .collect();
            let delay_fn = |dx: i64, dy: i64| t.lab(dx, dy) as f64;
            let tc = place::TimingCost {
                conns,
                delay: &delay_fn,
                lambda,
            };
            pl = place::place(
                &packed,
                &nl,
                &labs,
                &sites,
                &dsites,
                &port_xy,
                seed,
                Some(&tc),
                opts.effort,
            )?;
            let r1 = sta.analyse(&est(&pl));
            eprintln!(
                "timing: timing-driven placement, {:.3} ns estimated ({})",
                r1.worst, r1.at
            );
            dtab = Some(t);
        }
        let place_s = t1.elapsed().as_secs_f64();
        let lines = emit::lab_lines(&packed, &nl, &pl);
        let lab_re = |l: usize, pin: u32| -> Result<u32> {
            let name = format!("LAB_RE:X{}Y{}Z0I{pin}", labs[l].x, labs[l].y);
            g.node(&name).with_context(|| format!("no node {name}"))
        };
        let node = |e: &End| -> Result<u32> {
            match *e {
                End::Out { alm, slot } => {
                    lab_re(pl.at[alm].0, 89 + 4 * pl.at[alm].1 as u32 + slot as u32)
                }
                End::In { alm, pin } => lab_re(pl.at[alm].0, 8 * pl.at[alm].1 as u32 + pin as u32),
                End::Ctl { alm, ctl } => {
                    let l = pl.at[alm].0;
                    lab_re(l, emit::ctl_pin(&lines[&l], ctl))
                }
                End::Port(p) => port_node
                    .get(&p)
                    .copied()
                    .with_context(|| format!("port {} has no node", nl.ports[p].name)),
                End::M20k { m, pin } => {
                    let (x, y) = pl.m20k_at[m];
                    let name = format!("MEDIUM_EAB_RE:X{x}Y{y}Z0I{pin}");
                    g.node(&name).with_context(|| format!("no node {name}"))
                }
                End::Pll(k) => Ok(pll_node[k]),
                End::PllRst(k) => Ok(pll_rst_node[k]),
                End::Io { port, role } => io_node
                    .get(&(port, role))
                    .copied()
                    .with_context(|| format!("{}: no I/O node for {role:?}", nl.ports[port].name)),
                End::Dsp { d, pin } => {
                    let (x, y) = pl.dsp_at[d];
                    let name = format!("MP_MAC_RE:X{x}Y{y}Z0I{pin}");
                    g.node(&name).with_context(|| format!("no node {name}"))
                }
            }
        };
        // Nets as source and sink nodes; per sink the connections it serves
        // and the pins it may swap to.
        let mut rnets = Vec::new();
        let mut sink_conns: Vec<HashMap<u32, Vec<usize>>> = Vec::new();
        for (net, (d, sinks)) in &packed.nets {
            let mut by_node: HashMap<u32, Vec<usize>> = HashMap::default();
            let mut alt_of: HashMap<u32, Vec<u32>> = HashMap::default();
            for e in sinks {
                let n = node(e)?;
                let ci = sta.conn_of.get(&(*net, *e)).copied().unwrap_or(usize::MAX);
                by_node.entry(n).or_default().push(ci);
                if let End::In { alm, pin } = *e {
                    if opts.swap_pins {
                        let alts = pack::swap_group(&packed.alms[alm], pin)[1..]
                            .iter()
                            .map(|&q| node(&End::In { alm, pin: q }))
                            .collect::<Result<Vec<_>>>()?;
                        alt_of.insert(n, alts);
                    }
                }
            }
            let mut ss: Vec<u32> = by_node.keys().copied().collect();
            ss.sort();
            let alts = ss
                .iter()
                .map(|n| alt_of.get(n).cloned().unwrap_or_default())
                .collect();
            rnets.push(route::Net {
                name: nl.nets[*net as usize].clone(),
                source: node(d)?,
                sinks: ss,
                alts,
                clock: packed.clocks.contains(net),
            });
            sink_conns.push(by_node);
        }
        let order: Vec<usize> = {
            let mut o: Vec<usize> = (0..rnets.len()).collect();
            o.sort_by_key(|&i| (!rnets[i].clock, std::cmp::Reverse(rnets[i].sinks.len())));
            o
        };
        let rnets: Vec<route::Net> = order
            .iter()
            .map(|&i| std::mem::take(&mut rnets[i]))
            .collect();
        let sink_conns: Vec<HashMap<u32, Vec<usize>>> =
            order.iter().map(|&i| sink_conns[i].clone()).collect();
        let t2 = std::time::Instant::now();
        // Per sink criticality for the router, from the placement's estimate.
        let mut crit: Vec<Vec<f32>> = rnets.iter().map(|n| vec![0.0; n.sinks.len()]).collect();
        if let Some(t) = &dtab {
            let r = sta.analyse(
                &sta.conns
                    .iter()
                    .map(|(n, e)| t.conn(&packed, &pl, &labs, &port_xy, packed.nets[n].0, *e))
                    .collect::<Vec<_>>(),
            );
            for (ni, n) in rnets.iter().enumerate() {
                for (si, s) in n.sinks.iter().enumerate() {
                    crit[ni][si] = sink_conns[ni][s]
                        .iter()
                        .filter(|&&c| c != usize::MAX)
                        .map(|&c| r.crit[c])
                        .fold(0.0, f32::max);
                }
            }
        }
        let routed = match route::route(&g, &rnets, &base, &crit, 80) {
            Ok(r) => r,
            Err(e) if attempts < 3 && e.to_string().contains("did not converge") => {
                attempts += 1;
                seed += 1;
                eprintln!("route: {e}; placing again with seed {seed}");
                continue;
            }
            Err(e) => return Err(e),
        };
        let hops: usize = routed.trees.iter().map(|t| t.len()).sum();
        let route_s = t2.elapsed().as_secs_f64();
        eprintln!(
            "route: {} nets, {hops} hops, {} iterations, {route_s:.1} s",
            rnets.len(),
            routed.iterations
        );
        break (pl, place_s, lines, rnets, sink_conns, routed, route_s);
    };

    // The pins the router swapped LUT inputs to (README (z8)): per ALM, old
    // pin → new pin, then the sinks and masks follow.
    let mut moves: HashMap<usize, Vec<(u8, u8)>> = HashMap::default();
    let pin_of_node = |n: u32| -> Option<(usize, u8)> {
        let (x, y, i) = (
            g.x[n as usize] as u32,
            g.y[n as usize] as u32,
            g.i[n as usize],
        );
        let l = labs.iter().position(|l| l.x == x && l.y == y)?;
        let a = pl
            .at
            .iter()
            .position(|&(la, k)| la == l && k as u32 == i / 8)?;
        Some((a, (i % 8) as u8))
    };
    let mut swapped = 0;
    for (ni, net) in rnets.iter().enumerate() {
        for (si, &s) in net.sinks.iter().enumerate() {
            let r = routed.reached[ni][si];
            if r != s {
                let ((a, old), (_, new)) = (
                    pin_of_node(s).context("swapped sink")?,
                    pin_of_node(r).context("swapped pin")?,
                );
                moves.entry(a).or_default().push((old, new));
                swapped += 1;
            }
        }
    }
    for (&a, m) in &moves {
        pack::apply_swaps(&mut packed.alms[a], m);
    }
    for (_, sinks) in packed.nets.values_mut() {
        for e in sinks.iter_mut() {
            if let End::In { alm, pin } = e {
                if let Some(&(_, new)) = moves.get(alm).and_then(|m| m.iter().find(|m| m.0 == *pin))
                {
                    *pin = new;
                }
            }
        }
    }
    if swapped > 0 {
        eprintln!(
            "route: {swapped} LUT inputs swapped to other pins in {} ALMs",
            moves.len()
        );
    }
    // LAB-local feedback: a connection inside one LAB can run LAB_RE → LL →
    // LEIM → LAB_RE, as Quartus' critical paths do (README (z4), (z8)).
    let (mut local, mut direct) = (0, 0);
    for (ni, net) in rnets.iter().enumerate() {
        let src = net.source as usize;
        if g.type_name(net.source) != "LAB_RE" {
            continue;
        }
        let parent: HashMap<u32, u32> = routed.trees[ni].iter().map(|&(a, b)| (b, a)).collect();
        for &r in &routed.reached[ni] {
            if g.type_name(r) != "LAB_RE"
                || (g.x[r as usize], g.y[r as usize]) != (g.x[src], g.y[src])
            {
                continue;
            }
            local += 1;
            let mut hops = 0;
            let mut m = r;
            while let Some(&p) = parent.get(&m) {
                hops += 1;
                m = p;
            }
            direct += (hops == 3) as usize;
        }
    }
    eprintln!(
        "route: {local} connections inside one LAB, {direct} of them over its local lines alone"
    );
    // A connection's sink node, as the loop's `node` (outputs and
    // un-routed ends: None).
    let end_node = |e: &End| -> Option<u32> {
        let lab_re =
            |l: usize, pin: u32| g.node(&format!("LAB_RE:X{}Y{}Z0I{pin}", labs[l].x, labs[l].y));
        match *e {
            End::In { alm, pin } => lab_re(pl.at[alm].0, 8 * pl.at[alm].1 as u32 + pin as u32),
            End::Ctl { alm, ctl } => {
                lab_re(pl.at[alm].0, emit::ctl_pin(&lines[&pl.at[alm].0], ctl))
            }
            End::Port(p) => port_node.get(&p).copied(),
            End::M20k { m, pin } => {
                let (x, y) = pl.m20k_at[m];
                g.node(&format!("MEDIUM_EAB_RE:X{x}Y{y}Z0I{pin}"))
            }
            End::Io { port, role } => io_node.get(&(port, role)).copied(),
            End::Dsp { d, pin } => {
                let (x, y) = pl.dsp_at[d];
                g.node(&format!("MP_MAC_RE:X{x}Y{y}Z0I{pin}"))
            }
            End::PllRst(k) => Some(pll_rst_node[k]),
            End::Out { .. } | End::Pll(_) => None,
        }
    };
    // Timing of the routed design (README (u), (z)): each routed hop costs
    // its node type's mean, cells their arcs' means; on the swapped pins.
    let sta = sta::Sta::new(&nl, &packed, &delay, &io);
    let net_index: HashMap<u32, usize> = packed
        .nets
        .keys()
        .map(|k| {
            (
                *k,
                rnets
                    .iter()
                    .position(|r| r.name == nl.nets[*k as usize])
                    .unwrap_or(usize::MAX),
            )
        })
        .collect();
    let mut conn_delay = vec![0f32; sta.conns.len()];
    let parents: Vec<HashMap<u32, u32>> = routed
        .trees
        .iter()
        .map(|tree| tree.iter().map(|&(a, b)| (b, a)).collect())
        .collect();
    for (c, (net, e)) in sta.conns.iter().enumerate() {
        let ni = net_index[net];
        if ni == usize::MAX {
            continue;
        }
        let Some(mut m) = end_node(e) else { continue };
        let mut d = 0.0f32;
        while let Some(&p) = parents[ni].get(&m) {
            d += base[g.ty[m as usize] as usize];
            m = p;
        }
        conn_delay[c] = d;
    }
    let rep = sta.analyse(&conn_delay);
    if rep.worst > 0.0 {
        eprintln!(
            "timing: longest path {:.3} ns ({}), {:.0} MHz by the delay model",
            rep.worst,
            rep.at,
            1000.0 / rep.worst
        );
    }
    if rep.core_worst > 0.0 {
        eprintln!(
            "timing: register to register {:.3} ns, {:.0} MHz by the delay model",
            rep.core_worst,
            1000.0 / rep.core_worst
        );
    }

    // The design file (README (q), docs/toolchain.md).
    let mut text = format!("# a5pnr design: {top} from {json}\n");
    for l in &pin_lines {
        writeln!(text, "{l}")?;
    }
    // Only edges with bits: pass-through and hardwired hops (class PASS)
    // write nothing, and a decode of the image can't see them. An inverted
    // clear's last routing-tile hop is the inverter's setting instead.
    let inverted: det::HashSet<u32> = lines
        .iter()
        .filter(|(_, ll)| matches!(ll.aclr, Some((_, true))))
        .filter_map(|(&l, _)| {
            g.node(&format!(
                "RPI:X{}Y{}Z0I{}",
                labs[l].x,
                labs[l].y,
                emit::ACLR_RPI
            ))
        })
        .collect();
    let mut edges: Vec<(String, String)> = routed
        .trees
        .iter()
        .flatten()
        .filter(|&&(a, b)| {
            g.fanout(a)
                .any(|e| g.dst[e] == b && g.class[e] != graph::PASS)
        })
        .filter(|&&(_, b)| !inverted.contains(&b))
        .map(|&(a, b)| (g.name(a), g.name(b)))
        .collect();
    edges.sort();
    edges.dedup();
    for (a, b) in &edges {
        writeln!(text, "edge\t{a}\t{b}")?;
    }
    let (attrs, mut directs) = emit::blocks(&nl, &packed, &labs, &pl, &lines);
    // DSP blocks (README (z11)): their mode's settings as Quartus writes
    // them (db/sm4revb/dsp/modes.tsv), and the output register in the ffmap.
    let mut attrs = attrs;
    let dsp_modes = dsp::modes(db)?;
    let mut dsp_ffmap = String::new();
    for (d, u) in packed.dsps.iter().enumerate() {
        let netlist::Cell::Mul { mode, .. } = &nl.cells[u.cell] else {
            unreachable!()
        };
        let (x, y) = pl.dsp_at[d];
        let uid = format!("cw_dssm_r{y}_c{x}");
        for (kind, a, v) in dsp_modes
            .get(mode)
            .with_context(|| format!("dsp/modes.tsv has no {}", mode.name()))?
        {
            let row = (uid.clone(), a.clone(), v.clone());
            if kind == "direct" {
                directs.push(row)
            } else {
                attrs.push(row)
            }
        }
        for (k, f) in u.regs.iter().enumerate() {
            if let Some(f) = f {
                writeln!(dsp_ffmap, "{}\td{x}_{y}_p[{k}]", ff_names(&nl, *f, false))?;
            }
        }
    }
    for (u, a, v) in attrs.iter().chain(&io_attrs) {
        writeln!(text, "attr\t{u}\t{a}\t{v}")?;
    }
    for (u, a, v) in &directs {
        writeln!(text, "direct\t{u}\t{a}\t{v}")?;
    }
    text.push_str(&pll_text);
    std::fs::write(out, text)?;
    std::fs::write(
        format!("{out}.ffmap"),
        emit::ffmap(&nl, &packed, &labs, &pl)? + &io_ffmap + &dsp_ffmap,
    )?;
    let labs_used: std::collections::BTreeSet<usize> = pl.at.iter().map(|a| a.0).collect();
    eprintln!(
        "a5pnr: {} ALMs in {} LABs, {} M20Ks, {} DSPs, {} nets, {} edges, {} routing iterations, model Fmax {:.0} MHz; place {place_s:.1} s, route {route_s:.1} s, {:.1} s total",
        packed.alms.len(),
        labs_used.len(),
        packed.m20ks.len(),
        packed.dsps.len(),
        rnets.len(),
        edges.len(),
        routed.iterations,
        if rep.worst > 0.0 { 1000.0 / rep.worst } else { 0.0 },
        t0.elapsed().as_secs_f64()
    );
    Ok(())
}
