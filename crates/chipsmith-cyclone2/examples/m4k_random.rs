//! Writes the held-out random memories (`tests/support/m4k_random.rs`) as
//! DE2 projects, for building and for Quartus to assemble:
//!
//!     cargo run --release -p chipsmith-cyclone2 --example m4k_random -- <dir> <first seed> <last seed>

#[path = "../tests/support/m4k_random.rs"]
#[allow(dead_code)]
mod m4k_random;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, dir, first, last] = &args[..] else {
        eprintln!("usage: m4k_random <dir> <first seed> <last seed>");
        std::process::exit(2);
    };
    let ram = include_str!("../../../examples/ram/chipsmith.toml");
    let pins = &ram[ram.find("[pins]").unwrap()..ram.find("# The DE2").unwrap()];
    let pins = pins.replace(
        "CLOCK_50 = \"PIN_N2\"\n",
        "CLOCK_50 = \"PIN_N2\"\nCLOCK_27 = \"PIN_D13\"\n",
    );
    for seed in first.parse::<u64>().unwrap()..=last.parse::<u64>().unwrap() {
        let cfg = m4k_random::Config::new(seed);
        let p = std::path::Path::new(dir).join(cfg.name());
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(p.join("src/top.v"), cfg.verilog()).unwrap();
        std::fs::write(p.join("chipsmith.toml"), cfg.manifest(&pins)).unwrap();
        println!("{}: {cfg:?}", p.display());
    }
}
