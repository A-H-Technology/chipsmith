//! The committed loader binary is what its source assembles to (the
//! devshell has arm-none-eabi binutils; the build doesn't need them).
use std::process::Command;

#[test]
fn committed_loader_matches_its_source() {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src/zynq_loader.S");
    let dir = std::env::temp_dir().join(format!("zynq-loader-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (obj, bin) = (dir.join("l.o"), dir.join("l.bin"));
    let ok = |c: &mut Command| {
        assert!(c
            .status()
            .expect("arm-none-eabi binutils (devshell)")
            .success())
    };
    ok(Command::new("arm-none-eabi-as")
        .arg("-o")
        .arg(&obj)
        .arg(src));
    ok(Command::new("arm-none-eabi-objcopy")
        .args(["-O", "binary"])
        .arg(&obj)
        .arg(&bin));
    let built = std::fs::read(&bin).unwrap();
    let committed = include_bytes!("../src/zynq_loader.bin");
    std::fs::remove_dir_all(&dir).ok();
    assert!(
        built == committed,
        "src/zynq_loader.bin is stale: rebuild it as zynq_loader.S says"
    );
}
