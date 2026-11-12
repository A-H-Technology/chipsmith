//! Phase 1 with another SPL, against Quartus: agilex5-re's two HPS-first
//! builds of designs/hpsf differ only in the SPL (a GCC 16 one and the Arm
//! GNU 11.2 one that boots the board), so either, given the other's SPL, must
//! be the other byte for byte. Needs agilex5-re's specimens, so it is
//! ignored by default (`cargo test -p chipsmith-agilex5 -- --ignored`):
//! `CHIPSMITH_AGILEX5_SPECIMENS` and `CHIPSMITH_AGILEX5_DB` (for the key).
use std::path::PathBuf;

use chipsmith_agilex5::{jic, rbf::phase1};
use sha2::{Digest, Sha256};

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

/// Each phase 1 as a QSPI image: Quartus' `.jic` of it (`quartus_pfg -c
/// hpsf.sof hpsf.jic -o device=MT25QU128 -o flash_loader=A5EB013BB23B
/// -o mode=ASX4 -o hps_path=<SPL hex> -o hps=1`), known by its SHA-256 so
/// the 16 MiB files needn't be kept. The first is the one in the board's
/// QSPI since 2026-11-12.
#[test]
#[ignore = "needs agilex5-re's specimens"]
fn phase1_jics_are_quartus_jics() {
    let out = var("CHIPSMITH_AGILEX5_SPECIMENS").join("hps-first/out");
    for (rbf, sha256) in [
        (
            "spl11/hpsf.hps.rbf",
            "70e9ffb6a13744f01023309775240d83b6e9982f69940af56aa1da8351cb32fb",
        ),
        (
            "hpsf.hps.rbf",
            "009eb5b04de0fdc19b71dc7cbd1eb4242a340740a8a86e96cbd9ed8df071e7f9",
        ),
    ] {
        let j = jic::jic(&std::fs::read(out.join(rbf)).unwrap()).unwrap();
        let got: String = Sha256::digest(&j)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(got, sha256, "{rbf}: .jic differs from Quartus'");
    }
}
