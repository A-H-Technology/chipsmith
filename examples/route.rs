fn main() {
    let a: Vec<String> = std::env::args().collect();
    let t = std::time::Instant::now();
    let db = chipsmith::route::chipdb::ChipDb::read_cache(a[1].as_ref()).unwrap();
    eprintln!("graph {:.1} s", t.elapsed().as_secs_f64());
    let t = std::time::Instant::now();
    let s = chipsmith::route::route_file(&db, a[2].as_ref(), a[3].as_ref()).unwrap();
    eprintln!("{s:?} in {:.2} s", t.elapsed().as_secs_f64());
}
