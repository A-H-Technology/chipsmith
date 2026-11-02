//! Whether one site pin reaches another in the 7-series routing graph, and
//! the last nodes on the way: `xc7_reach <site> <pin> <site> <pin>`. A
//! debugging aid for the site routing (`crate::phys`).

use std::collections::VecDeque;

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    anyhow::ensure!(a.len() == 5, "usage: xc7_reach <site> <pin> <site> <pin>");
    let db = chipsmith_xc7::xray::load(chipsmith_xc7::part::XC7Z010CLG400_1)?;
    let node = |site: &str, pin: &str| -> anyhow::Result<u32> {
        let s = &db.sites[site];
        let w = db.tile_type(s.tile).bels[&s.relative].pins[pin];
        Ok(db.node(s.tile, w))
    };
    let (src, dst) = (node(&a[1], &a[2])?, node(&a[3], &a[4])?);
    let mut from = vec![u32::MAX; db.nodes()];
    from[src as usize] = src;
    let mut q = VecDeque::from([src]);
    while let Some(n) = q.pop_front() {
        if n == dst {
            break;
        }
        for e in db.downhill(n) {
            if from[e.dst as usize] == u32::MAX {
                from[e.dst as usize] = n;
                q.push_back(e.dst);
            }
        }
    }
    if from[dst as usize] == u32::MAX {
        println!("unreachable");
    } else {
        let mut path = vec![dst];
        while *path.last().unwrap() != src {
            path.push(from[*path.last().unwrap() as usize]);
        }
        println!("{} hops", path.len() - 1);
        let classes: Vec<&str> = path
            .iter()
            .rev()
            .map(|&n| db.wire_types[db.node_type[n as usize] as usize].as_str())
            .collect();
        println!("{}", classes.join(" "));
    }
    Ok(())
}
