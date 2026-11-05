//! Bench: load a 7-series `.bit` over a Digilent FT2232 with chipsmith's own
//! JTAG stack. `xc7_load <file.bit>`
use chipsmith_jtag::{ftdi::Ftdi, xc7};

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: xc7_load <file.bit>");
    let file = std::fs::read(&path)?;
    let mut cable = Ftdi::open()?;
    let chain = xc7::scan(&mut cable)?;
    for (i, t) in chain.iter().enumerate() {
        eprintln!("TAP {i}: {:#010x}, IR {} bits", t.idcode, t.ir_len);
    }
    let target = chain
        .iter()
        .position(|t| t.ir_len == 6)
        .expect("no 7-series TAP on the chain");
    let started = std::time::Instant::now();
    xc7::load(&mut cable, &chain, target, xc7::bit_payload(&file)?)?;
    eprintln!(
        "loaded, DONE high, in {:.2} s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}
