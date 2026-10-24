use std::path::Path;
#[test]
fn parse_all() {
    let files = std::env::var("PARSE_FILES").unwrap_or_default();
    let t = std::time::Instant::now();
    let mut n = 0;
    for f in files.split_whitespace() {
        let mut fe = chipsmith_verilog::Frontend::new();
        if let Err(e) = fe.read_file(Path::new(f)) {
            println!("ERR {e}");
        }
        n += 1;
    }
    println!("{n} files in {:?}", t.elapsed());
}
