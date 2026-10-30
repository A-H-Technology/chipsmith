//! Bench: place and route a DE2 project from its packed netlist, with the
//! manifest's pins and clocks, so the placer and router can be timed and
//! profiled without synthesis in front of them.
//!
//! `de2_pnr [--place-only | --route-only] <project dir> <scratch dir>`:
//! the scratch dir must hold `packed.json` (from `de2_synth`), or
//! `placed.json` for `--route-only`; it gets `placed.json`,
//! `routed.json` and the .rbf. Prints one line of numbers: placement time,
//! wirelength, the placer's critical path estimate, then routing time,
//! iterations, pips and the routed critical path.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Result};
use chipsmith::manifest::{Manifest, Target};
use chipsmith_cyclone2::{bitstream, place, route};

fn main() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |args: &mut Vec<String>, name: &str| {
        let on = args.first().is_some_and(|a| a == name);
        if on {
            args.remove(0);
        }
        on
    };
    let place_only = flag(&mut args, "--place-only");
    let route_only = flag(&mut args, "--route-only");
    let [project, scratch] = &args[..] else {
        bail!("usage: de2_pnr [--place-only | --route-only] <project dir> <scratch dir>");
    };
    let project = std::fs::canonicalize(project)?;
    let dir = std::fs::canonicalize(PathBuf::from(scratch))?;
    let manifest = Manifest::load(&project)?;
    let Target::Ep2c35f672c6 {
        pins,
        clocks,
        unused_pins,
    } = &manifest.target
    else {
        bail!("not a DE2 project");
    };
    let graph = route::load_graph()?;
    let pins: BTreeMap<String, place::PinPlan> = pins
        .iter()
        .map(|a| {
            let plan = place::PinPlan {
                pin: a.pin.as_str().to_string(),
                settings: a.io.settings,
            };
            (a.port.to_string(), plan)
        })
        .collect();
    let packed = dir.join("packed.json");
    let placed = dir.join("placed.json");
    if !route_only {
        let t = Instant::now();
        let pl = place::place_file(&graph, &packed, &pins, *unused_pins, &placed)?;
        let place_s = t.elapsed().as_secs_f64();
        print!(
            "{} place {:.1} s, {} LEs, wirelength {} -> {}, {} moves, critical {:.2} ns (estimate)",
            manifest.name,
            place_s,
            pl.les,
            pl.initial_cost,
            pl.cost,
            pl.moves,
            pl.critical / 1000.0
        );
    }
    if place_only {
        println!();
        return Ok(());
    }
    let routed = dir.join("routed.json");
    let t = Instant::now();
    let cpu = cpu_seconds();
    let r = route::route_file(&graph, &placed, &routed)?;
    let route_s = t.elapsed().as_secs_f64();
    let route_cpu = cpu_seconds() - cpu;
    let period = clocks.first().map(|c| c.frequency.period_ns());
    println!(
        "; route {:.1} s ({:.1} s CPU), {} iterations, {} pips, critical {:.2} ns, Fmax {:.1} MHz{}",
        route_s,
        route_cpu,
        r.iterations,
        r.pips,
        r.critical / 1000.0,
        1e6 / f64::from(r.critical),
        period.map_or(String::new(), |p| format!(
            ", slack {:.2} ns",
            p - f64::from(r.critical) / 1000.0
        ))
    );
    let db = bitstream::load_db()?;
    bitstream::pack_file(&db, &routed, &dir.join(format!("{}.rbf", manifest.name)))?;
    Ok(())
}

/// User plus system time of this process so far, from /proc (Linux): on a
/// loaded machine a steadier measure of work than the wall clock.
fn cpu_seconds() -> f64 {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    // Fields after the parenthesised command name; utime and stime are
    // the 14th and 15th of the line.
    let rest = stat.rsplit_once(')').map_or("", |r| r.1);
    let f: Vec<f64> = rest
        .split_whitespace()
        .skip(11)
        .take(2)
        .filter_map(|v| v.parse().ok())
        .collect();
    // Clock ticks; 100 per second on every Linux this runs on.
    f.iter().sum::<f64>() / 100.0
}
