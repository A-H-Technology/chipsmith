//! Bench: place a packed design, `place <graph> <packed.json> <pins.qsf> <placed.json>`.
use std::collections::BTreeMap;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let db = chipsmith_route::chipdb::ChipDb::read_cache(a[1].as_ref()).unwrap();
    let pins: BTreeMap<String, chipsmith_cyclone2::place::PinPlan> = std::fs::read_to_string(&a[3])
        .unwrap()
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            (f.next()? == "set_location_assignment").then_some(())?;
            let pin = f.next()?.to_string();
            f.next()?;
            let plan = chipsmith_cyclone2::place::PinPlan {
                pin,
                settings: Default::default(),
            };
            Some((f.next()?.to_string(), plan))
        })
        .collect();
    let t = std::time::Instant::now();
    let s = chipsmith_cyclone2::place::place_file(
        &db,
        a[2].as_ref(),
        &pins,
        Default::default(),
        a[4].as_ref(),
    )
    .unwrap();
    eprintln!("{s:?} in {:.1} s", t.elapsed().as_secs_f64());
}
