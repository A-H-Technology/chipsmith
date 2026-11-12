//! Phase 1 with another SPL, against Quartus: agilex5-re's two HPS-first
//! builds of designs/hpsf differ only in the SPL (a GCC 16 one and the Arm
//! GNU 11.2 one that boots the board), so either, given the other's SPL, must
//! be the other byte for byte. Needs agilex5-re's specimens, so it is
//! ignored by default (`cargo test -p chipsmith-agilex5 -- --ignored`):
//! `CHIPSMITH_AGILEX5_SPECIMENS` and `CHIPSMITH_AGILEX5_DB` (for the key).
use std::path::PathBuf;

use chipsmith_agilex5::rbf::phase1;

fn var(name: &str) -> PathBuf {
    std::env::var_os(name)
        .unwrap_or_else(|| panic!("{name} not set"))
        .into()
}

/// An Intel HEX file's data records as one image from address 0 (the SPL
/// hex files are contiguous from 0).
fn ihex(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ext = 0usize;
    for line in text.lines().filter_map(|l| l.trim().strip_prefix(':')) {
        let d: Vec<u8> = (0..line.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&line[i..i + 2], 16).unwrap())
            .collect();
        let (n, addr, kind) = (
            d[0] as usize,
            u16::from_be_bytes([d[1], d[2]]) as usize,
            d[3],
        );
        match kind {
            0 => {
                let at = ext + addr;
                assert_eq!(at, out.len(), "SPL hex is not contiguous from 0");
                out.extend_from_slice(&d[4..4 + n]);
            }
            2 => ext = (u16::from_be_bytes([d[4], d[5]]) as usize) << 4,
            4 => ext = (u16::from_be_bytes([d[4], d[5]]) as usize) << 16,
            _ => {}
        }
    }
    out
}

#[test]
#[ignore = "needs agilex5-re's specimens"]
fn a_phase1_with_the_other_spl_is_the_other_build() {
    let out = var("CHIPSMITH_AGILEX5_SPECIMENS").join("hps-first/out");
    let key = std::fs::read(var("CHIPSMITH_AGILEX5_DB").join("keys/rbf_keyed_hash.key")).unwrap();
    let read = |p: &str| std::fs::read(out.join(p)).unwrap();
    let spl = |p: &str| ihex(&std::fs::read_to_string(out.join(p)).unwrap());
    let gcc16 = (read("hpsf.hps.rbf"), spl("u-boot-spl-dtb.hex"));
    let gcc11 = (read("spl11/hpsf.hps.rbf"), spl("spl11/spl11.hex"));
    for ((base, _), (expected, spl), name) in [
        (&gcc16, &gcc11, "spl11/hpsf.hps.rbf"),
        (&gcc11, &gcc16, "hpsf.hps.rbf"),
    ] {
        let built = phase1::with_spl(base, spl, &key).unwrap();
        assert!(&built == expected, "{name} differs");
    }
}
