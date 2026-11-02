//! FASM to frames and a `.bit` with chipsmith's writer
//! (`chipsmith_xc7::frames`), for comparing with prjxray's tools:
//! `xc7_frames <fasm> <frm out> <bit out>`.

fn main() -> anyhow::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    anyhow::ensure!(a.len() == 4, "usage: xc7_frames <fasm> <frm out> <bit out>");
    let part = chipsmith_xc7::part::XC7Z010CLG400_1;
    let family = std::path::PathBuf::from(std::env::var(chipsmith_xc7::xray::PRJXRAY_DB_VAR)?).join(part.family);
    let fasm = std::fs::read_to_string(&a[1])?;
    let frames = chipsmith_xc7::frames::Assembler::new(&family, part)?.frames(&fasm)?;
    std::fs::write(&a[2], chipsmith_xc7::frames::frm(&frames))?;
    std::fs::write(&a[3], chipsmith_xc7::frames::bit(&family, part.part, "design", &frames)?)?;
    Ok(())
}
