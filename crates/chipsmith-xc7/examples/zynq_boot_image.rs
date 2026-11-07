//! A QSPI boot image (BootROM header, chipsmith's loader, bitstream) from a
//! Zynq `.bin`, for flash offset 0.
//! `cargo run --release -p chipsmith-xc7 --example zynq_boot_image -- IN.bin OUT.img`
fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = args.as_slice() else {
        anyhow::bail!("usage: zynq_boot_image IN.bin OUT.img");
    };
    let image = chipsmith_xc7::zynq::qspi_boot_image(&std::fs::read(input)?)?;
    std::fs::write(output, &image)?;
    println!("{output}: {} bytes", image.len());
    Ok(())
}
