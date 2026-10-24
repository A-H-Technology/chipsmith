//! Persistent flashing (`chipsmith flash --persist`): the design goes into
//! the board's configuration flash, so it survives a power cycle.
//!
//! DE2: the EP2C35 configures itself from an EPCS16 in active-serial mode
//! at power-up (RUN/PROG switch in RUN). chipsmith writes that flash over
//! JTAG, the way Quartus' serial flash loader does: a small design of its
//! own (`loader/`, a JTAG-to-SPI bridge on the FPGA's flash pins) goes into
//! the FPGA first, then the image goes through it (`epcs.rs`).

pub mod epcs;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};

use chipsmith_jtag::{svf, usb_blaster::UsbBlaster, Cable, TapState};

/// A prebuilt loader RBF; the nix package sets it.
pub const LOADER_VAR: &str = "CHIPSMITH_DE2_LOADER";
pub const LOADER_MANIFEST: &str = include_str!("loader/chipsmith.toml");
pub const LOADER_TOP: &str = include_str!("loader/top.v");

/// Where the serial flash loader's bitstream comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Loader {
    Prebuilt(PathBuf),
    /// Built into this project directory first.
    Build(PathBuf),
}

impl Loader {
    pub fn from_env(build_dir: &Path) -> Self {
        match std::env::var_os(LOADER_VAR) {
            Some(rbf) => Loader::Prebuilt(rbf.into()),
            None => Loader::Build(build_dir.join("flash-loader")),
        }
    }

    pub fn rbf(&self) -> PathBuf {
        match self {
            Loader::Prebuilt(rbf) => rbf.clone(),
            Loader::Build(dir) => dir.join("build/loader.rbf"),
        }
    }
}

/// Size of an uncompressed EP2C35 RBF (cyclone2-re README, "Container
/// format"): 0x186 header bytes, 2,725 frames of 314, 36 trailing.
const EP2C35_RBF_BYTES: usize = 856_076;
/// The option words, three copies at 0x10, 0x19 and 0x22. Their second
/// byte's bit 3 is set in a passive-serial RBF and clear in what Quartus
/// writes for the EPCS (`quartus_cpf` .sof -> .pof -> .rpd, otherwise the
/// same bytes as the RBF).
const OPTION_BYTES: [usize; 3] = [0x11, 0x1a, 0x23];
const OPTION_PS: u8 = 1 << 3;

/// What goes into the EPCS for an RBF: Quartus' active-serial bytes, each
/// bit-reversed. The FPGA takes the configuration stream LSB first, the
/// EPCS sends each byte MSB first; the DE2's factory image is stored the
/// same way.
pub fn epcs_image(rbf: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        rbf.len() == EP2C35_RBF_BYTES,
        "{} bytes is not an uncompressed EP2C35 RBF ({EP2C35_RBF_BYTES})",
        rbf.len()
    );
    let mut image = rbf.to_vec();
    for at in OPTION_BYTES {
        ensure!(
            image[at] & OPTION_PS != 0,
            "RBF option byte {at:#x} is {:#04x}, not a passive-serial RBF's",
            image[at]
        );
        image[at] &= !OPTION_PS;
    }
    Ok(image.iter().map(|b| b.reverse_bits()).collect())
}

/// What to do with the EPCS once the loader is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpcsOp {
    /// Write an RBF's image and read it back.
    Program(PathBuf),
    /// Save the whole flash, read twice and compared.
    Dump(PathBuf),
    /// Write a whole-flash dump back, verbatim, and read it back.
    Restore(PathBuf),
}

/// EP2C35 JTAG instructions (10-bit IR). PULSE_NCONFIG reconfigures the
/// device as a pulse on nCONFIG would: in active-serial mode, from the
/// EPCS. Observed on the DE2: the loader gives way to the design just
/// written, CONF_DONE high, its USERCODE.
const IR_LEN: usize = 10;
const PULSE_NCONFIG: u16 = 0x001;
const CHECK_STATUS: u16 = 0x004;
const USERCODE: u16 = 0x007;
/// CHECK_STATUS's register and CONF_DONE in it (cram's `jtag.rs`, as
/// Quartus' SVF checks it).
const STATUS_BITS: usize = 1449;
const CONF_DONE: usize = 562;

fn instruction<C: Cable>(cable: &mut C, ir: u16) -> Result<()> {
    cable.tms(&TapState::Reset.path_to(TapState::Reset))?;
    cable.tms(&TapState::Reset.path_to(TapState::IrShift))?;
    cable.shift(&ir.to_le_bytes(), IR_LEN, true, false)?;
    cable.tms(&TapState::IrExit1.path_to(TapState::Idle))
}

fn capture<C: Cable>(cable: &mut C, ir: u16, bits: usize) -> Result<Vec<u8>> {
    instruction(cable, ir)?;
    cable.tms(&TapState::Idle.path_to(TapState::DrShift))?;
    let tdo = cable.shift(&vec![0; bits.div_ceil(8)], bits, true, true)?;
    cable.tms(&TapState::DrExit1.path_to(TapState::Idle))?;
    tdo.context("no TDO")
}

/// The USERCODE an RBF sets: bit 2 of header bytes 0x4d..0x6d, LSB first
/// (cyclone2-re README, "Container format").
fn rbf_usercode(rbf: &[u8]) -> u32 {
    (0..32).fold(0, |uc, k| uc | u32::from(rbf[0x4d + k] >> 2 & 1) << k)
}

/// Have the FPGA configure itself from the EPCS, as at power-up, and wait
/// for CONF_DONE. Returns the USERCODE it came up with.
fn reboot_from_flash<C: Cable>(cable: &mut C) -> Result<u32> {
    instruction(cable, PULSE_NCONFIG)?;
    cable.idle(1000)?;
    cable.tms(&TapState::Idle.path_to(TapState::Reset))?;
    cable.flush()?;
    // An EP2C35 image is 6.8 Mbit; the EPCS sends it at DCLK, 20 MHz or
    // so, in well under a second.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let status = capture(cable, CHECK_STATUS, STATUS_BITS)?;
        if status[CONF_DONE / 8] >> (CONF_DONE % 8) & 1 == 1 {
            break;
        }
        if Instant::now() > deadline {
            bail!(
                "the FPGA didn't configure from the EPCS (CONF_DONE low); \
                 is the DE2's RUN/PROG switch in RUN?"
            );
        }
    }
    let uc = capture(cable, USERCODE, 32)?;
    Ok(u32::from_le_bytes(uc[..4].try_into()?))
}

/// Configure the loader from its SVF, then do `op`.
pub fn run_epcs(loader_svf: &Path, op: &EpcsOp) -> Result<()> {
    let text = std::fs::read_to_string(loader_svf)
        .with_context(|| format!("reading {}", loader_svf.display()))?;
    let commands = svf::parse(&text)?;
    let mut cable = UsbBlaster::open()?;
    let started = Instant::now();
    svf::Player::new(&mut cable).play(&commands)?;
    cable.flush()?;
    eprintln!(
        "    serial flash loader in, {:.1} s",
        started.elapsed().as_secs_f64()
    );
    let expect = epcs::with(&mut cable, |flash| {
        let started = Instant::now();
        Ok(match op {
            EpcsOp::Program(rbf) => {
                let bytes =
                    std::fs::read(rbf).with_context(|| format!("reading {}", rbf.display()))?;
                let image = epcs_image(&bytes)?;
                flash.write_verified(0, &image)?;
                eprintln!(
                    "    wrote and verified {} bytes in {:.1} s",
                    image.len(),
                    started.elapsed().as_secs_f64()
                );
                Some(rbf_usercode(&bytes))
            }
            EpcsOp::Dump(out) => {
                let all = flash.dump()?;
                std::fs::write(out, &all).with_context(|| format!("writing {}", out.display()))?;
                eprintln!(
                    "    read {} bytes twice, identical, in {:.1} s",
                    all.len(),
                    started.elapsed().as_secs_f64()
                );
                None
            }
            EpcsOp::Restore(dump) => {
                let all =
                    std::fs::read(dump).with_context(|| format!("reading {}", dump.display()))?;
                ensure!(
                    all.len() == epcs::SIZE,
                    "{} is {} bytes, not a whole EPCS16 ({})",
                    dump.display(),
                    all.len(),
                    epcs::SIZE
                );
                flash.write_verified(0, &all)?;
                eprintln!(
                    "    restored and verified {} bytes in {:.1} s",
                    all.len(),
                    started.elapsed().as_secs_f64()
                );
                None
            }
        })
    })?;
    // Leave the FPGA running what the flash holds, not the loader; that it
    // comes up is the check that the image boots.
    let started = Instant::now();
    let usercode = reboot_from_flash(&mut cable)?;
    if let Some(expect) = expect {
        ensure!(
            usercode == expect,
            "the FPGA came up from the EPCS with USERCODE {usercode:#010x}, not the image's {expect:#010x}"
        );
    }
    eprintln!(
        "    configured from the EPCS (PULSE_NCONFIG), CONF_DONE high, USERCODE {usercode:#010x}, in {:.1} s",
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_clears_the_passive_serial_option_and_reverses_bits() {
        let mut rbf = vec![0u8; EP2C35_RBF_BYTES];
        for at in OPTION_BYTES {
            rbf[at] = 0xdf;
        }
        rbf[0x10] = 0x6a;
        rbf[0x4c] = 0x01;
        let image = epcs_image(&rbf).unwrap();
        // 0xd7 reversed is 0xeb, 0x6a is 0x56, 0x01 is 0x80.
        assert_eq!(image[0x11], 0xeb);
        assert_eq!(image[0x1a], 0xeb);
        assert_eq!(image[0x10], 0x56);
        assert_eq!(image[0x4c], 0x80);
    }

    #[test]
    fn image_refuses_what_isnt_an_ep2c35_rbf() {
        assert!(epcs_image(&[0; 100]).is_err());
        assert!(epcs_image(&vec![0; EP2C35_RBF_BYTES]).is_err());
    }
}
