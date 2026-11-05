//! `XC7_PLL` and `XC7_MMCM` (`cells.v`) as the PLLE2_ADV or MMCME2_ADV
//! that makes what they ask for (`crate::cmt`), its feedback through a
//! BUFG for ZHOLD (the clocking wizard's way) or inside the tile for
//! INTERNAL. The block keeps the cell's name, so clocks and reports name
//! the design's own instance.

use anyhow::{ensure, Context, Result};
use chipsmith_netlist::{CellKind, Connectivity, Dir, Generic, GenericPort, Module, SigBit, State};
use chipsmith_synth::rewrite::Gates;

use super::cells;
use crate::cmt::{Compensation, Prim, Request};

/// Lowers every request; returns how many blocks it made.
pub fn lower(m: &mut Module) -> Result<usize> {
    let conn = Connectivity::new(m);
    let mut jobs = Vec::new();
    for (i, c) in m.cells.iter().enumerate() {
        let CellKind::Other(g) = &c.kind else { continue };
        let Some(prim) = Prim::from_request_cell(&g.ty) else { continue };
        let bit = |port: &str| g.port(port).and_then(|p| p.sig.first().copied());
        let used: Vec<bool> = (0..prim.outputs())
            .map(|k| match bit(&format!("CLK{k}")) {
                Some(SigBit::Net(n)) => !conn.users(n).is_empty(),
                _ => false,
            })
            .collect();
        let request = Request::from_params(prim, &g.params, &used).with_context(|| format!("cell {}", c.name))?;
        let solved = request.solve().with_context(|| format!("cell {}", c.name))?;
        let inclk = bit("INCLK").with_context(|| format!("cell {}: INCLK unconnected", c.name))?;
        ensure!(
            matches!(inclk, SigBit::Net(_)),
            "cell {}: INCLK is a constant",
            c.name
        );
        let outputs: Vec<Option<SigBit>> = (0..prim.outputs())
            .map(|k| if used[k] { bit(&format!("CLK{k}")) } else { None })
            .collect();
        jobs.push((
            i,
            prim,
            solved.config.params(Some(request.inclk_period_ps)),
            request.compensation,
            inclk,
            bit("ARESET").unwrap_or(SigBit::Const(State::Zero)),
            bit("LOCKED"),
            outputs,
        ));
    }
    let n = jobs.len();
    for (i, prim, params, compensation, inclk, reset, locked, outputs) in jobs {
        let fb = SigBit::Net(m.new_net());
        let fb_in = match compensation {
            Compensation::Internal => fb,
            Compensation::Zhold => {
                let g = SigBit::Net(m.new_net());
                Gates::new(m, "cmt").add(cells::bufg(fb, g));
                g
            }
        };
        let port = |name: &str, dir: Dir, b: SigBit| GenericPort {
            name: name.into(),
            dir: Some(dir),
            sig: vec![b],
        };
        let mut ports = vec![
            port("CLKIN1", Dir::In, inclk),
            port("CLKFBIN", Dir::In, fb_in),
            port("RST", Dir::In, reset),
            port("CLKFBOUT", Dir::Out, fb),
        ];
        for (k, o) in outputs.iter().enumerate() {
            if let Some(o) = o {
                ports.push(port(&format!("CLKOUT{k}"), Dir::Out, *o));
            }
        }
        if let Some(l) = locked {
            ports.push(port("LOCKED", Dir::Out, l));
        }
        m.cells[i].kind = CellKind::Other(Generic {
            ty: prim.cell().into(),
            params,
            ports,
        });
    }
    Ok(n)
}
